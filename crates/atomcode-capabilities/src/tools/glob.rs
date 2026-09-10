//! `glob` — find files by glob pattern under a base directory, gitignore-aware.
//! Read-only ⇒ always `Safe`. Standard ripgrep / grok-build glob semantics:
//! - Patterns without `/` (e.g. `*.sh`, `*release*`) match against filename and recursively penetrate subdirectories.
//! - Patterns with `/` (e.g. `scripts/*.sh`, `./*.rs`, `**/*.rs`) match against the relative path.
//! Build/VCS/cache dirs are skipped; results sorted by modification time, capped at 300 by default (raise `limit`).

use super::{err, is_absolute_path, is_skip_dir, not_found_hint, ok, resolve_path};
use crate::tool_feedback::{format_path_not_found, parse_tool_args};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_MAX_RESULTS: usize = 300;
const MAX_RESULTS_CAP: usize = 2000;

pub struct GlobTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, deserialize_with = "super::read::lenient_usize")]
    limit: Option<usize>,
    /// Default false = case-insensitive (Windows-friendly). Set true to match literally.
    #[serde(default)]
    case_sensitive: bool,
    /// Default false = files only. Set true to also return matching directories.
    #[serde(default)]
    include_dirs: bool,
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files matching a glob pattern. Use to locate file paths by filename, extension, or directory layout. `path` must be a directory (not a file). Default match is case-insensitive; set `case_sensitive` to disable that. Set `include_dirs` to also return matching directories."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern." },
                "path": { "type": "string", "default": ".", "description": "Directory scope to match. Must be a directory, not a file." },
                "limit": { "type": "integer", "default": 300, "description": "Maximum paths to return." },
                "case_sensitive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Case-sensitive matching (default: false, case-insensitive). Set true on Linux to avoid matching README.RS for *.rs."
                },
                "include_dirs": {
                    "type": "boolean",
                    "default": false,
                    "description": "Also return matching directories (default: files only). Directory paths are shown with a trailing '/'."
                }
            },
            "required": ["pattern"]
        })
    }
    /// No side effects — a pure read. Makes it `parallel_safe` (concurrent
    /// execution) and allowed in plan mode.
    fn read_only_hint(&self) -> bool {
        true
    }
    // read-only → risk() defaults to Safe.
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match parse_tool_args("glob", args, r#"{"pattern":"<glob>","path":"<dir>"}"#)
        {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        // Models routinely paste an absolute path straight into `pattern` (e.g.
        // `G:/VR2024/keystore/*`) with no `path` base. Without honoring that, the walk
        // would run in the working dir and silently match nothing — making an existing
        // file look like it "does not exist". An absolute prefix in the pattern wins
        // over `path`; otherwise fall back to `path` (default: the working dir).
        let display_base = a.path.clone().unwrap_or_else(|| ".".to_string());
        let (base, match_pattern) = match split_absolute_base(&a.pattern) {
            Some((dir, rest)) => (dir, rest),
            None => {
                let raw = a.path.clone().unwrap_or_else(|| ".".to_string());
                (resolve_path(&raw, &ctx.working_dir), a.pattern.clone())
            }
        };
        match tokio::fs::metadata(&base).await {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return err(format!(
                    "glob: `path` must be a directory, but '{display_base}' is a file (resolved to {}). Use the parent directory as `path` and put the filename in `pattern` (e.g. {{\"pattern\":\"*.rs\",\"path\":\"src\"}}).",
                    crate::pathnorm::to_display(&base)
                ));
            }
            Err(_) => {
                let hint = not_found_hint(&base, &ctx.working_dir).await;
                return err(format!(
                    "{}{hint}",
                    format_path_not_found("glob", &display_base, &base, &ctx.working_dir)
                ));
            }
        }

        // Normalize Windows backslashes so GlobBuilder doesn't treat `\` as an escape character
        let normalized_pattern = match_pattern.replace('\\', "/");
        let has_separator = normalized_pattern.contains('/');
        let matcher = match GlobBuilder::new(&normalized_pattern)
            .literal_separator(true)
            .case_insensitive(!a.case_sensitive)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return err(format!("glob: invalid pattern '{}': {e}", a.pattern)),
        };

        let wd = ctx.working_dir.clone();
        let base2 = base.clone();
        let pattern = a.pattern.clone();
        let include_dirs = a.include_dirs;
        let search_secs = super::tool_timeouts().search_secs;
        let res = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(search_secs);
            let mut timed_out = false;
            let mut hits: Vec<(String, std::time::SystemTime)> = Vec::new();
            let mut builder = WalkBuilder::new(&base2);
            builder
                // Allow searching hidden files and directories unless gitignored
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .add_custom_ignore_filename(".codegraphignore")
                .add_custom_ignore_filename(".codegraignore");

            let global_config = crate::paths::config_dir();
            let global_ignore1 = global_config.join(".codegraphignore");
            if global_ignore1.is_file() {
                builder.add_ignore(global_ignore1);
            }
            let global_ignore2 = global_config.join(".codegraignore");
            if global_ignore2.is_file() {
                builder.add_ignore(global_ignore2);
            }

            let walk = builder
                .filter_entry(|e| {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        if let Some(name) = e.file_name().to_str() {
                            return !is_skip_dir(name);
                        }
                    }
                    true
                })
                .build();
            for entry in walk.flatten() {
                if Instant::now() >= deadline {
                    timed_out = true;
                    break;
                }
                let path = entry.path();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
                if path == base2 {
                    continue; // never list the search root itself
                }
                if is_file {
                    // keep
                } else if include_dirs && is_dir {
                    // keep
                } else {
                    continue;
                }
                // Match standard glob semantics (ripgrep / grok-build aligned):
                // If pattern contains a path separator (e.g. "src/*.rs", "**/*.sh"), match the relative path.
                // If pattern does not contain any path separator (e.g. "*.sh", "*release*", "Cargo.toml"),
                // match against the file basename directly, enabling recursive auto-penetration across all subdirectories.
                let matched = if has_separator {
                    let rel = path.strip_prefix(&base2).unwrap_or(path);
                    matcher.is_match(rel)
                } else {
                    matcher.is_match(entry.file_name())
                };

                if matched {
                    // Display relative to the working dir for usable paths.
                    let mut shown =
                        crate::pathnorm::to_display(path.strip_prefix(&wd).unwrap_or(path));
                    if is_dir && !shown.ends_with('/') {
                        shown.push('/');
                    }
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    hits.push((shown, mtime));
                }
            }
            // Sort by modification time descending (most recently modified first)
            hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let file_paths: Vec<String> = hits.into_iter().map(|(shown, _)| shown).collect();
            (file_paths, timed_out)
        })
        .await;

        match res {
            Ok((hits, timed_out)) if hits.is_empty() => {
                let noun = if include_dirs { "paths" } else { "files" };
                let mut msg = format!("No {noun} matching \"{pattern}\"");
                if timed_out {
                    msg.push_str(&format!(
                        "\n[Search timed out after {search_secs}s; narrow the pattern/path]"
                    ));
                }
                ok(msg)
            }
            Ok((mut hits, timed_out)) => {
                let cap = a
                    .limit
                    .unwrap_or(DEFAULT_MAX_RESULTS)
                    .clamp(1, MAX_RESULTS_CAP);
                let total = hits.len();
                let extra = total.saturating_sub(cap);
                if total > cap {
                    hits.truncate(cap);
                }
                let noun = if include_dirs { "paths" } else { "files" };
                let mut out = format!(
                    "{total} {noun} found (sorted by modification time, most recent first):\n{}",
                    hits.join("\n")
                );
                if extra > 0 {
                    out.push_str(&format!(
                        "\n[{extra} more {noun} not shown; raise `limit` or narrow the pattern/path]"
                    ));
                }
                if timed_out {
                    out.push_str(&format!(
                        "\n[Search timed out after {search_secs}s; showing matches collected so far]"
                    ));
                }
                ok(out)
            }
            Err(_) => err("glob: search task failed".to_string()),
        }
    }
}

/// If `pattern` begins with an ABSOLUTE directory prefix (the leading run of literal,
/// glob-free path segments), split it off as a search base and return the remaining
/// pattern relative to it. Splits on BOTH `/` and `\` and normalizes the remainder to
/// `/` so Windows-style paths work regardless of build target (`\` is glob-escape in
/// globset, so it must not survive into the matcher). Returns `None` for purely
/// relative patterns (e.g. `**/*.rs`, `src/**/*.ts`), which keep the existing
/// base-relative behavior.
fn split_absolute_base(pattern: &str) -> Option<(PathBuf, String)> {
    // Everything before the first glob metacharacter is a literal path region.
    let scan_end = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    // The base ends at the last separator within that literal region.
    let sep = pattern[..scan_end].rfind(['/', '\\'])?;
    let dir = &pattern[..sep];
    // A `~`-prefixed base is an absolute location too (parity with the `path` arg,
    // which resolves `~` via resolve_path); expand it so `glob("~/proj/**/*.rs")`
    // isn't silently walked relative to cwd.
    let base = if crate::pathutil::expand_tilde(dir).is_some() || is_absolute_path(dir) {
        // Same resolver as read_file: `~`, POSIX `/tmp`, MSYS `/c/Users`, Windows
        // drive/UNC. Dummy cwd is unused for these absolute/tilde forms.
        resolve_path(dir, Path::new("."))
    } else {
        return None;
    };
    let rest = pattern[sep + 1..].replace('\\', "/");
    // A trailing separator (a pasted directory path) leaves no remainder — list the
    // directory's direct children rather than building an empty matcher that matches
    // nothing (which would falsely report "No files matching").
    let rest = if rest.is_empty() {
        "*".to_string()
    } else {
        rest
    };
    Some((base, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::tool::ToolContext;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn split_absolute_base_handles_windows_and_unix_roots() {
        // Windows drive, forward slashes.
        let (base, rest) = split_absolute_base("G:/VR2024/keystore/*").unwrap();
        assert_eq!(base, PathBuf::from("G:/VR2024/keystore"));
        assert_eq!(rest, "*");
        // Windows drive, backslashes + recursive glob → remainder normalized to `/`.
        let (base, rest) = split_absolute_base(r"G:\VR2024\**\*.jks").unwrap();
        assert_eq!(base, PathBuf::from(r"G:\VR2024"));
        assert_eq!(rest, "**/*.jks");
        // An absolute exact file path (no metachar) is a degenerate glob that resolves
        // to its own directory + literal name.
        let (base, rest) = split_absolute_base("/abs/dir/screenshare.jks").unwrap();
        assert_eq!(base, PathBuf::from("/abs/dir"));
        assert_eq!(rest, "screenshare.jks");
        // A trailing separator (a pasted directory path) leaves no remainder; it must
        // become a "list this dir" glob, not an empty matcher that matches nothing.
        let (base, rest) = split_absolute_base("/abs/dir/").unwrap();
        assert_eq!(base, PathBuf::from("/abs/dir"));
        assert_eq!(rest, "*");
        // Relative patterns are left for base-relative matching.
        assert!(split_absolute_base("**/*.rs").is_none());
        assert!(split_absolute_base("src/**/*.ts").is_none());
    }

    #[test]
    fn split_absolute_base_expands_leading_tilde() {
        // A `~/…` base is absolute (home-relative), NOT a cwd-relative walk — parity
        // with the `path` arg. Assert relative to the same home the code reads.
        if let Some(home) = crate::pathutil::home_dir() {
            let (base, rest) = split_absolute_base("~/proj/**/*.rs").unwrap();
            assert_eq!(base, home.join("proj"));
            assert_eq!(rest, "**/*.rs");
            // Bare `~/` base lists the home dir's children.
            let (base, rest) = split_absolute_base("~/*").unwrap();
            assert_eq!(base, home);
            assert_eq!(rest, "*");
        }
    }

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        }
    }

    /// Same recovery clue as `grep`/`list_directory` — glob failed on the identical guessed
    /// path in the reported session.
    #[tokio::test]
    async fn missing_base_dir_error_carries_the_nearest_existing_ancestor() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("app")).unwrap();
        std::fs::write(d.path().join("app/build.gradle"), "").unwrap();
        let r = GlobTool
            .execute(
                r#"{"pattern":"**/*.java","path":"app/src/main/java"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(
            r.content.contains("Nearest existing directory"),
            "{}",
            r.content
        );
        assert!(r.content.contains("build.gradle"), "{}", r.content);
    }

    #[tokio::test]
    async fn matches_recursive_pattern() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/sub")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "").unwrap();
        std::fs::write(d.path().join("src/sub/b.rs"), "").unwrap();
        std::fs::write(d.path().join("src/c.txt"), "").unwrap();
        let r = GlobTool
            .execute(r#"{"pattern":"**/*.rs"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("src/a.rs"), "{}", r.content);
        assert!(r.content.contains("src/sub/b.rs"), "{}", r.content);
        assert!(!r.content.contains("c.txt"), "{}", r.content);
    }

    #[tokio::test]
    async fn single_star_with_slash_does_not_cross_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("top.rs"), "").unwrap();
        std::fs::write(d.path().join("src/deep.rs"), "").unwrap();
        // Pattern with path separator (e.g. "src/*.rs") strictly matches that directory level
        let r = GlobTool
            .execute(r#"{"pattern":"src/*.rs"}"#, &ctx(d.path()))
            .await;
        assert!(r.content.contains("src/deep.rs"), "{}", r.content);
        assert!(
            !r.content.contains("top.rs"),
            "pattern with slash must not cross /: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn no_match_reports_cleanly() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "").unwrap();
        let r = GlobTool
            .execute(r#"{"pattern":"**/*.zig"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("No files matching"), "{}", r.content);
    }

    #[tokio::test]
    async fn absolute_pattern_searches_outside_working_dir() {
        // The target lives OUTSIDE the working directory; the model pastes its
        // absolute path straight into `pattern` with no `path` base (exactly what
        // happened with `G:\VR2024\keystore\screenshare.jks`). It must still be found.
        let target = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(target.path().join("keystore")).unwrap();
        std::fs::write(target.path().join("keystore/screenshare.jks"), "").unwrap();

        let work = tempfile::tempdir().unwrap(); // unrelated cwd, on a "different drive"
        let pattern = format!("{}/keystore/*", target.path().display());
        let args = serde_json::json!({ "pattern": pattern }).to_string();
        let r = GlobTool.execute(&args, &ctx(work.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("screenshare.jks"), "{}", r.content);
    }

    #[tokio::test]
    async fn absolute_recursive_pattern_searches_outside_working_dir() {
        let target = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(target.path().join("keystore")).unwrap();
        std::fs::write(target.path().join("keystore/screenshare.jks"), "").unwrap();

        let work = tempfile::tempdir().unwrap();
        let pattern = format!("{}/**/*.jks", target.path().display());
        let args = serde_json::json!({ "pattern": pattern }).to_string();
        let r = GlobTool.execute(&args, &ctx(work.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("screenshare.jks"), "{}", r.content);
    }

    #[tokio::test]
    async fn absolute_directory_with_trailing_slash_lists_children() {
        // A model pastes a bare absolute directory path (trailing slash, no glob).
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("screenshare.jks"), "").unwrap();
        let work = tempfile::tempdir().unwrap();
        let pattern = format!("{}/", target.path().display());
        let args = serde_json::json!({ "pattern": pattern }).to_string();
        let r = GlobTool.execute(&args, &ctx(work.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("screenshare.jks"), "{}", r.content);
    }

    #[tokio::test]
    async fn skips_build_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("target/x.rs"), "").unwrap();
        std::fs::write(d.path().join("keep.rs"), "").unwrap();
        let r = GlobTool
            .execute(r#"{"pattern":"**/*.rs"}"#, &ctx(d.path()))
            .await;
        assert!(r.content.contains("keep.rs"), "{}", r.content);
        assert!(!r.content.contains("target/x.rs"), "{}", r.content);
    }

    #[tokio::test]
    async fn sorts_by_mtime_descending() {
        let d = tempfile::tempdir().unwrap();
        let f_old = d.path().join("old.txt");
        let f_new = d.path().join("new.txt");
        std::fs::write(&f_old, "old content").unwrap();
        // Brief pause to ensure distinct file modification timestamp on Windows filesystem
        std::thread::sleep(std::time::Duration::from_millis(60));
        std::fs::write(&f_new, "new content").unwrap();

        let r = GlobTool
            .execute(r#"{"pattern":"*.txt"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        let pos_new = r.content.find("new.txt").expect("must find new.txt");
        let pos_old = r.content.find("old.txt").expect("must find old.txt");
        assert!(
            pos_new < pos_old,
            "new.txt should appear before old.txt due to mtime descending: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn pattern_without_slash_recursively_penetrates_subdirectories() {
        // Aligned with ripgrep / grok-build glob semantics:
        // Patterns without path separators (e.g. "*release*", "*.sh") automatically
        // match across any subdirectory depth via basename matching.
        let d = tempfile::tempdir().unwrap();
        let sub_scripts = d.path().join("scripts");
        let deep_dir = d.path().join("a/b/c");
        std::fs::create_dir_all(&sub_scripts).unwrap();
        std::fs::create_dir_all(&deep_dir).unwrap();

        std::fs::write(d.path().join("root.sh"), "").unwrap();
        std::fs::write(sub_scripts.join("release-self-update.sh"), "").unwrap();
        std::fs::write(deep_dir.join("nested.sh"), "").unwrap();
        std::fs::write(sub_scripts.join("other.txt"), "").unwrap();

        // 1. "*.sh" matches root, scripts/, and deep a/b/c/
        let r1 = GlobTool
            .execute(r#"{"pattern":"*.sh"}"#, &ctx(d.path()))
            .await;
        assert!(!r1.is_error, "{}", r1.content);
        assert!(r1.content.contains("root.sh"), "{}", r1.content);
        assert!(
            r1.content.contains("release-self-update.sh"),
            "{}",
            r1.content
        );
        assert!(r1.content.contains("nested.sh"), "{}", r1.content);
        assert!(!r1.content.contains("other.txt"), "{}", r1.content);

        // 2. "*release*" matches release-self-update.sh in scripts/
        let r2 = GlobTool
            .execute(r#"{"pattern":"*release*"}"#, &ctx(d.path()))
            .await;
        assert!(!r2.is_error, "{}", r2.content);
        assert!(
            r2.content.contains("release-self-update.sh"),
            "{}",
            r2.content
        );
        assert!(!r2.content.contains("root.sh"), "{}", r2.content);

        // 3. Pattern with slash (e.g. "scripts/*.sh") preserves strict path matching
        let r3 = GlobTool
            .execute(r#"{"pattern":"scripts/*.sh"}"#, &ctx(d.path()))
            .await;
        assert!(!r3.is_error, "{}", r3.content);
        assert!(
            r3.content.contains("release-self-update.sh"),
            "{}",
            r3.content
        );
        assert!(!r3.content.contains("root.sh"), "{}", r3.content);
        assert!(!r3.content.contains("nested.sh"), "{}", r3.content);
    }

    #[tokio::test]
    async fn windows_backslash_and_case_insensitive_and_hidden_match() {
        let work = tempfile::tempdir().unwrap();
        let sub = work.path().join("src").join("components");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Chat.tsx"), "export const Chat = 1;\n").unwrap();
        std::fs::write(work.path().join(".env.local"), "SECRET=123\n").unwrap();

        // 1. Windows backslash matching
        let r1 = GlobTool
            .execute(r#"{"pattern":"src\\components\\*.tsx"}"#, &ctx(work.path()))
            .await;
        assert!(!r1.is_error, "r1 failed: {}", r1.content);
        assert!(r1.content.contains("Chat.tsx"));

        // 2. Case-insensitive matching
        let r2 = GlobTool
            .execute(r#"{"pattern":"src/**/*chat.tsx"}"#, &ctx(work.path()))
            .await;
        assert!(!r2.is_error, "r2 failed: {}", r2.content);
        assert!(r2.content.contains("Chat.tsx"));

        // 3. Hidden file matching
        let r3 = GlobTool
            .execute(r#"{"pattern":".env*"}"#, &ctx(work.path()))
            .await;
        assert!(!r3.is_error, "r3 failed: {}", r3.content);
        assert!(r3.content.contains(".env.local"));
    }

    #[tokio::test]
    async fn path_pointing_at_file_explains_usage() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("event.rs"), "").unwrap();
        let r = GlobTool
            .execute(r#"{"pattern":"*","path":"event.rs"}"#, &ctx(d.path()))
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(r.content.contains("must be a directory"), "{}", r.content);
        assert!(r.content.contains("is a file"), "{}", r.content);
        assert!(
            !r.content.contains("path does not exist"),
            "must not look like a missing path: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn case_sensitive_does_not_match_different_case() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Chat.tsx"), "").unwrap();
        let r_ci = GlobTool
            .execute(r#"{"pattern":"chat.tsx"}"#, &ctx(d.path()))
            .await;
        assert!(r_ci.content.contains("Chat.tsx"), "{}", r_ci.content);
        let r_cs = GlobTool
            .execute(
                r#"{"pattern":"chat.tsx","case_sensitive":true}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(
            !r_cs.content.contains("Chat.tsx"),
            "case-sensitive must not match Chat.tsx: {}",
            r_cs.content
        );
    }

    #[tokio::test]
    async fn include_dirs_returns_matching_directories() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/sub")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "").unwrap();
        let r_files = GlobTool
            .execute(r#"{"pattern":"src/**"}"#, &ctx(d.path()))
            .await;
        assert!(r_files.content.contains("src/a.rs"), "{}", r_files.content);
        assert!(
            !r_files.content.contains("src/sub/"),
            "dirs omitted by default: {}",
            r_files.content
        );
        let r_dirs = GlobTool
            .execute(
                r#"{"pattern":"src/**","include_dirs":true}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r_dirs.is_error, "{}", r_dirs.content);
        assert!(r_dirs.content.contains("src/a.rs"), "{}", r_dirs.content);
        assert!(r_dirs.content.contains("src/sub/"), "{}", r_dirs.content);
        assert!(r_dirs.content.contains("paths found"), "{}", r_dirs.content);
    }
}
