//! `grep` — regex content search under a directory, gitignore-aware. Read-only ⇒
//! always `Safe`. Smart-case (case-insensitive unless the pattern has an uppercase
//! letter); an invalid regex falls back to a literal search. Build/VCS/cache dirs and
//! `.log` files are skipped. Neutral core — the production graph/semantic annotations
//! are dropped.

use super::read::lenient_usize;
use super::{err, is_skip_dir, not_found_hint, ok, resolve_path};
use crate::tool_feedback::{format_path_not_found, parse_tool_args};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use globset::{GlobBuilder, GlobMatcher};
use grep::regex::{RegexMatcher, RegexMatcherBuilder};
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

const DEFAULT_MAX_RESULTS: usize = 200;
const DEFAULT_CONTEXT: usize = 0;
const MAX_CONTEXT: usize = 10;
const MAX_DISPLAY_LINE: usize = 1000;
/// Hard upper cap on `max_results` — bounds the in-memory result buffer even if a
/// caller sends an enormous value.
const MAX_RESULTS_CAP: usize = 10_000;
/// Per-file heap cap for the searcher's line buffer. Without it the searcher would
/// grow the buffer to hold the LONGEST single line — a multi-MB minified bundle or a
/// one-line giant log would still buffer whole and OOM a small machine. Past this, the
/// file's search errors and is skipped: the absolute memory guard, independent of the
/// per-line default.
const MAX_LINE_BUF_BYTES: usize = 10 * 1024 * 1024;

/// Grok default tool-output budget (40 KB). Remainder: narrow path/glob or raise max_results.
const MAX_OUTPUT_BYTES: usize = 40_000;

pub struct GrepTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default, deserialize_with = "lenient_usize")]
    max_results: Option<usize>,
    #[serde(default, deserialize_with = "lenient_usize")]
    context: Option<usize>,
    /// File-name glob (`*.rs`, `*.{ts,tsx}`, `src/**/*.go`). Ripgrep-style:
    /// a pattern with no `/` matches at any depth.
    #[serde(default)]
    glob: Option<String>,
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents by regular expression (literal / exact-string search).\n\
         pattern (required JSON field): the regex itself. NEVER put it in `description`.\n\
           GOOD: {\"pattern\":\"GetSalePostSettingData\",\"path\":\"src/auth\"}\n\
           GOOD: {\"pattern\":\"positionTracking\"}\n\
           BAD:  {\"description\":\" [Constraint: pattern: foo]\"}  — that is not `pattern`\n\
           BAD:  {\"description\":\" [Constraint: path: a.cs, pattern: Foo]\"}\n\
         path: directory or file to search (optional; default working directory).\n\
         For a feature, design, or code-logic question, use `code_explore` \
         (path = a directory/module, query = Chinese/English or a symbol) instead of grep.\n\
         Smart-case: case-insensitive unless the pattern contains an uppercase letter. \
         Escape regex metachars, e.g. `console\\.log\\(`. Use `glob` (e.g. `*.rs`) to \
         restrict file types. Default 200 matches and 0 context lines; raise `max_results` \
         or add `context` if needed. Results are capped; truncated pages report remaining work."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "REQUIRED JSON field: the regex to search for. Example: 'GetSalePostSettingData'. Do NOT put this in `description` or wrap it as '[Constraint: pattern: …]'."
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (default: the working directory). Pass as `path`, not inside `description`."
                },
                "glob": { "type": "string", "description": "File-name glob to restrict which files are searched, e.g. `*.rs`, `*.{ts,tsx}` (ripgrep-style: no `/` matches at any depth)" },
                "max_results": { "type": "integer", "description": "Max matching lines to return (default 200). Raise this instead of re-running a narrower crawl." },
                "context": { "type": "integer", "description": "Lines of context around each match (default 0, max 10)" }
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
        let (a, recovered) = match parse_grep_args(args) {
            Ok(v) => v,
            Err(e) => return e.into_tool_result(),
        };
        let raw = a.path.clone().unwrap_or_else(|| ".".to_string());
        let root = resolve_path(&raw, &ctx.working_dir);
        if tokio::fs::metadata(&root).await.is_err() {
            let hint = not_found_hint(&root, &ctx.working_dir).await;
            return err(format!(
                "{}{hint}",
                format_path_not_found("grep", &raw, &root, &ctx.working_dir)
            ));
        }
        let max = a
            .max_results
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS_CAP);
        let context = a.context.unwrap_or(DEFAULT_CONTEXT).min(MAX_CONTEXT);
        let glob_filter = match a.glob.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(g) => match GlobBuilder::new(g).literal_separator(true).build() {
                Ok(glob) => Some((glob.compile_matcher(), g.to_string())),
                Err(e) => return err(format!("grep: invalid glob '{g}': {e}")),
            },
        };

        // Smart-case + literal fallback, as a streaming ripgrep matcher.
        let has_upper = a.pattern.chars().any(|c| c.is_uppercase());
        let matcher = match RegexMatcherBuilder::new()
            .case_insensitive(!has_upper)
            .build(&a.pattern)
        {
            Ok(m) => m,
            Err(_) => match RegexMatcherBuilder::new()
                .case_insensitive(!has_upper)
                .build(&regex::escape(&a.pattern))
            {
                Ok(m) => m,
                Err(e) => return err(format!("grep: invalid pattern '{}': {e}", a.pattern)),
            },
        };

        let base = ctx.working_dir.clone();
        let pattern = a.pattern.clone();
        let display_path = raw.clone();
        let search_secs = super::tool_timeouts().search_secs;
        let deadline = Instant::now() + Duration::from_secs(search_secs);
        let res = tokio::task::spawn_blocking(move || {
            search(
                &root,
                &matcher,
                max,
                context,
                &base,
                glob_filter.as_ref(),
                deadline,
            )
        })
        .await;
        match res {
            Ok((lines, _, files, timed_out)) if lines.is_empty() => {
                let mut msg = format!(
                    "No matches found for '{pattern}' in {display_path} ({files} files searched)"
                );
                if timed_out {
                    msg.push_str(
                        "\n[Search timed out; narrow `path` / `glob` or use code_explore]",
                    );
                }
                if let Some(note) = recovered {
                    msg = format!("{note}\n{msg}");
                }
                ok(msg)
            }
            Ok((lines, matches, _, timed_out)) => {
                let capped = matches >= max;
                let mut out = lines.join("\n");
                if out.len() > MAX_OUTPUT_BYTES {
                    let mut end = MAX_OUTPUT_BYTES.min(out.len());
                    while end > 0 && !out.is_char_boundary(end) {
                        end -= 1;
                    }
                    out.truncate(end);
                    out.push_str(
                        "\n\n[Output truncated to 40KB. Raise `max_results` is not enough — add `glob` / a more specific `path`, or use code_explore(path, query=pattern) for feature/design/logic.]",
                    );
                } else if capped {
                    out.push_str(&format!(
                        "\n\n[Results capped at {max} matches; raise `max_results` or add `glob` / a more specific `path`. For a feature/design/logic question, prefer code_explore(path, query='{pattern}').]"
                    ));
                }
                if timed_out {
                    out.push_str(&format!(
                        "\n[Search timed out after {search_secs}s; showing matches collected so far. Narrow `path`/`glob` or use code_explore.]"
                    ));
                }
                if let Some(note) = recovered {
                    out = format!("{note}\n{out}");
                }
                ok(out)
            }
            Err(_) => err("grep: search task failed".to_string()),
        }
    }
}

const GREP_SHAPE: &str = r#"{"pattern":"<regex>","path":"<dir>"}"#;

fn parse_grep_args(args: &str) -> Result<(Args, Option<String>), crate::tool_feedback::ParamError> {
    if let Ok(a) = parse_tool_args::<Args>("grep", args, GREP_SHAPE) {
        if !a.pattern.trim().is_empty() {
            return Ok((a, None));
        }
    }
    let mut v: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
    let note = recover_pattern_into(&mut v);
    let dumped = serde_json::to_string(&v).unwrap_or_else(|_| args.to_string());
    match parse_tool_args::<Args>("grep", &dumped, GREP_SHAPE) {
        Ok(a) if !a.pattern.trim().is_empty() => Ok((a, note)),
        _ => parse_tool_args::<Args>("grep", args, GREP_SHAPE).map(|a| (a, None)),
    }
}

/// Weak models put `pattern`/`path` inside `description` as
/// `[Constraint: path: foo.cs, pattern: Bar]` (or only `pattern:`) instead of
/// JSON fields. Recover both so grep does not bounce with `missing_field`.
fn recover_pattern_into(v: &mut Value) -> Option<String> {
    let obj = v.as_object_mut()?;
    let constraint = obj
        .get("description")
        .and_then(|p| p.as_str())
        .map(extract_constraint_fields)
        .unwrap_or_default();

    let mut recovered_from = Vec::new();
    let has_pattern = obj
        .get("pattern")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if !has_pattern {
        let mut found: Option<(String, String)> = None;
        for key in ["query", "regex", "search", "q"] {
            if let Some(s) = obj
                .get(key)
                .and_then(|p| p.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                found = Some((key.to_string(), s.to_string()));
                break;
            }
        }
        if found.is_none() {
            if let Some(p) = constraint.pattern.clone() {
                found = Some(("description".into(), p));
            }
        }
        if let Some((src, pat)) = found {
            obj.insert("pattern".into(), Value::String(pat.clone()));
            recovered_from.push(format!("pattern←{src}({pat})"));
        }
    }

    let has_path = obj
        .get("path")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if !has_path {
        if let Some(p) = constraint.path.clone() {
            obj.insert("path".into(), Value::String(p.clone()));
            recovered_from.push(format!("path←description({p})"));
        }
    }

    let has_glob = obj
        .get("glob")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if !has_glob {
        if let Some(g) = constraint.glob.clone() {
            obj.insert("glob".into(), Value::String(g));
        }
    }

    if recovered_from.is_empty() {
        return None;
    }
    let pat = obj
        .get("pattern")
        .and_then(|p| p.as_str())
        .unwrap_or("<regex>");
    let path = obj.get("path").and_then(|p| p.as_str()).unwrap_or("<dir>");
    Some(format!(
        "[grep: recovered {} — next call use {{\"pattern\":\"{pat}\",\"path\":\"{path}\"}}]",
        recovered_from.join(", ")
    ))
}

#[derive(Default)]
struct ConstraintFields {
    pattern: Option<String>,
    path: Option<String>,
    glob: Option<String>,
}

fn extract_constraint_fields(desc: &str) -> ConstraintFields {
    let mut out = ConstraintFields::default();
    for key in ["pattern", "query", "regex", "path", "glob"] {
        if let Some(val) = extract_constraint_value(desc, key) {
            match key {
                "pattern" | "query" | "regex" if out.pattern.is_none() => {
                    out.pattern = Some(val);
                }
                "path" if out.path.is_none() => out.path = Some(val),
                "glob" if out.glob.is_none() => out.glob = Some(val),
                _ => {}
            }
        }
    }
    out
}

fn extract_constraint_value(desc: &str, key: &str) -> Option<String> {
    let lower = desc.to_ascii_lowercase();
    let needle = format!("{key}:");
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let abs = search_from + rel;
        let boundary_ok = abs == 0
            || desc.as_bytes()[abs - 1].is_ascii_whitespace()
            || matches!(desc.as_bytes()[abs - 1], b'[' | b',' | b'{' | b'(');
        if !boundary_ok {
            search_from = abs + 1;
            continue;
        }
        let rest = desc[abs + needle.len()..].trim_start();
        let end = next_constraint_field_start(rest);
        let raw = rest.get(..end).unwrap_or(rest);
        let val = raw
            .trim()
            .trim_end_matches([']', '}', ')'])
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .trim();
        if !val.is_empty() {
            return Some(val.to_string());
        }
        search_from = abs + 1;
    }
    None
}

/// End of a Constraint value: next `, <known-key>:` or closing bracket.
fn next_constraint_field_start(rest: &str) -> usize {
    const KEYS: &[&str] = &["pattern:", "query:", "regex:", "path:", "glob:", "search:"];
    let mut cut = rest
        .find(']')
        .or_else(|| rest.find('}'))
        .unwrap_or(rest.len());
    for (i, ch) in rest.char_indices() {
        if i >= cut {
            break;
        }
        if ch != ',' {
            continue;
        }
        let after = rest[i + 1..].trim_start();
        let after_l = after.to_ascii_lowercase();
        if KEYS.iter().any(|k| after_l.starts_with(k)) {
            cut = i;
            break;
        }
    }
    cut
}

/// Returns (formatted match+context lines, match count, files searched). Stops once
/// `max` matches are collected. Each file is searched by a STREAMING searcher (never
/// loads the whole file into memory; `heap_limit` caps the per-line buffer), so a huge
/// file — or a huge single line — can't OOM the process.
/// Ripgrep-style glob: `*.rs` matches at any depth; `src/**/*.rs` is path-relative.
fn grep_glob_matches(rel: &Path, matcher: &GlobMatcher, pattern: &str) -> bool {
    if matcher.is_match(rel) {
        return true;
    }
    if !pattern.contains('/') && !pattern.contains('\\') {
        if let Some(name) = rel.file_name() {
            return matcher.is_match(Path::new(name));
        }
    }
    false
}

fn search(
    root: &std::path::Path,
    matcher: &RegexMatcher,
    max: usize,
    context: usize,
    base: &std::path::Path,
    glob_filter: Option<&(GlobMatcher, String)>,
    deadline: Instant,
) -> (Vec<String>, usize, usize, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut match_count = 0usize;
    let mut files_searched = 0usize;
    let mut timed_out = false;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
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
            // Drop our extra skip-dirs (gitignore already covers most).
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    return !is_skip_dir(name);
                }
            }
            true
        })
        .build();

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context)
        .after_context(context)
        // Treat NUL-containing files as binary and stop (ripgrep-standard). Non-UTF-8
        // text is searched lossily rather than skipped, so ASCII patterns still match in
        // e.g. a GBK-encoded file (the old whole-file read skipped those entirely).
        .binary_detection(BinaryDetection::quit(b'\x00'))
        // Absolute memory guard: cap the per-file line buffer so a single huge line
        // (minified bundle / one-line log) can't grow the buffer without bound.
        .heap_limit(Some(MAX_LINE_BUF_BYTES))
        .build();

    for entry in walk.flatten() {
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        if match_count >= max {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some((glob, pat)) = glob_filter {
            let rel = path.strip_prefix(root).unwrap_or(path);
            if !grep_glob_matches(rel, glob, pat) {
                continue;
            }
        }
        if path
            .extension()
            .map(|x| x.eq_ignore_ascii_case("log"))
            .unwrap_or(false)
        {
            continue;
        }
        files_searched += 1;
        let rel = crate::pathnorm::to_display(path.strip_prefix(base).unwrap_or(path));
        let sink = GrepSink {
            rel: &rel,
            out: &mut out,
            match_count: &mut match_count,
            max,
        };
        // io / binary / decode errors ⇒ skip the file (same as the old read failure).
        let _ = searcher.search_path(matcher, path, sink);
    }
    (out, match_count, files_searched, timed_out)
}

/// Render a raw line (bytes from the searcher) for display: strip the trailing line
/// ending, lossily decode, and truncate an over-long (e.g. minified) line.
fn render_line(bytes: &[u8]) -> String {
    let cow = String::from_utf8_lossy(bytes);
    let line = cow.strip_suffix('\n').unwrap_or(&cow);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.chars().count() > MAX_DISPLAY_LINE {
        line.chars().take(MAX_DISPLAY_LINE).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

/// Sink that formats each match/context line exactly like the previous manual loop:
/// `rel:num:content` for a match, `rel-num-content` for context, `--` between
/// non-contiguous groups. Stops a file's search once the global `max` matches is hit.
struct GrepSink<'a> {
    rel: &'a str,
    out: &'a mut Vec<String>,
    match_count: &'a mut usize,
    max: usize,
}

impl<'a> Sink for GrepSink<'a> {
    type Error = std::io::Error;

    fn matched(&mut self, _s: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        let n = mat.line_number().unwrap_or(0);
        self.out
            .push(format!("{}:{n}:{}", self.rel, render_line(mat.bytes())));
        *self.match_count += 1;
        Ok(*self.match_count < self.max) // stop this file at the cap
    }

    fn context(&mut self, _s: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, std::io::Error> {
        let n = ctx.line_number().unwrap_or(0);
        self.out
            .push(format!("{}-{n}-{}", self.rel, render_line(ctx.bytes())));
        Ok(true)
    }

    fn context_break(&mut self, _s: &Searcher) -> Result<bool, std::io::Error> {
        self.out.push("--".to_string());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atomcode_kernel::tool::ToolContext;
    use tokio_util::sync::CancellationToken;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            cancel: CancellationToken::new(),
            progress: atomcode_kernel::tool::ProgressSink::noop(),
            requester: None,
        }
    }

    #[tokio::test]
    async fn finds_matches_with_line_numbers() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.rs"),
            "fn main() {\n    let TODO = 1;\n    other();\n}\n",
        )
        .unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"TODO","path":"."}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("a.rs:2:"), "{}", r.content);
    }

    /// The real-world shape (a Windows user, 2026-08-05): a Gradle project whose `app/` has no
    /// `src/`. The model grepped `app/src`, got a bare "path not found", and spent the next
    /// three turns guessing deeper paths. The error must name where the tree actually stops.
    #[tokio::test]
    async fn missing_path_error_carries_the_nearest_existing_ancestor() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("app")).unwrap();
        std::fs::write(d.path().join("app/build.gradle"), "").unwrap();
        let r = GrepTool
            .execute(
                r#"{"pattern":"Serial","path":"app/src/main/java"}"#,
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

    // Issue #722 parity (v2): weak models send max_results/context as a string ("50")
    // or float (50.0 / "3.0") instead of an integer; the args must still deserialize.
    #[test]
    fn args_accept_lenient_numeric_max_results_and_context() {
        let a: Args = serde_json::from_str(r#"{"pattern":"x","max_results":"50","context":3.0}"#)
            .expect("string max_results + float context must deserialize");
        assert_eq!(a.max_results, Some(50));
        assert_eq!(a.context, Some(3));

        let b: Args = serde_json::from_str(r#"{"pattern":"x","max_results":"50.0"}"#)
            .expect("float-string max_results must deserialize");
        assert_eq!(b.max_results, Some(50));
    }

    #[tokio::test]
    async fn smart_case_is_insensitive_for_lowercase_pattern() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "Hello World\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"hello"}"#, &ctx(d.path()))
            .await;
        assert!(r.content.contains("a.txt:1:"), "{}", r.content);
    }

    #[tokio::test]
    async fn zero_matches_is_success() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "nothing here\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"absent_xyz"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "zero matches must be a success: {}", r.content);
        assert!(r.content.contains("No matches found"), "{}", r.content);
    }

    #[tokio::test]
    async fn invalid_regex_falls_back_to_literal() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "value = foo(bar)\n").unwrap();
        // "foo(bar" is an invalid regex (unbalanced paren) → literal fallback finds it.
        let r = GrepTool
            .execute(r#"{"pattern":"foo(bar"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("a.txt:1:"), "{}", r.content);
    }

    #[tokio::test]
    async fn context_lines_are_marked_and_groups_separated() {
        let d = tempfile::tempdir().unwrap();
        // 10 lines, matches on line 2 and line 8 → two non-contiguous groups at context 1.
        let content = (1..=10)
            .map(|i| match i {
                2 => "NEEDLE two".to_string(),
                8 => "NEEDLE eight".to_string(),
                _ => format!("line {i}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(d.path().join("f.txt"), content + "\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE","context":1}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        // Match lines use `:`, context lines use `-`.
        assert!(
            r.content.contains("f.txt:2:NEEDLE two"),
            "match line: {}",
            r.content
        );
        assert!(
            r.content.contains("f.txt-1-line 1"),
            "before-context: {}",
            r.content
        );
        assert!(
            r.content.contains("f.txt-3-line 3"),
            "after-context: {}",
            r.content
        );
        assert!(
            r.content.contains("f.txt:8:NEEDLE eight"),
            "second match: {}",
            r.content
        );
        // Non-contiguous groups are separated by `--`.
        assert!(
            r.content.contains("\n--\n"),
            "group separator: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn searches_a_large_file_via_streaming_not_whole_read() {
        let d = tempfile::tempdir().unwrap();
        // ~3 MB of filler + one match near the end. The streaming searcher finds it
        // WITHOUT a whole-file `read_to_string` (which was the OOM/freeze risk).
        let mut big = "filler line\n".repeat(250_000); // ~3 MB
        big.push_str("HAYSTACK_NEEDLE at the end\n");
        std::fs::write(d.path().join("big.txt"), big).unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"HAYSTACK_NEEDLE"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            r.content.contains("big.txt:250001:HAYSTACK_NEEDLE"),
            "{}",
            r.content
        );
    }

    #[tokio::test]
    async fn giant_single_line_file_is_skipped_not_buffered_whole() {
        let d = tempfile::tempdir().unwrap();
        // One line LARGER than the heap cap (e.g. a minified bundle). The searcher errors
        // on it and skips it, instead of buffering the whole line into memory (the OOM case).
        let mut giant = String::from("NEEDLE ");
        giant.push_str(&"x".repeat(MAX_LINE_BUF_BYTES + 1024)); // > cap, no newline
        std::fs::write(d.path().join("min.js"), &giant).unwrap();
        std::fs::write(d.path().join("ok.txt"), "NEEDLE\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "grep must not error/hang on a giant line");
        assert!(
            r.content.contains("ok.txt:1:"),
            "normal match found: {}",
            &r.content[..r.content.len().min(120)]
        );
        assert!(
            !r.content.contains("min.js"),
            "over-cap single-line file must be skipped"
        );
    }

    #[tokio::test]
    async fn capped_message_counts_matches_not_output_rows() {
        let d = tempfile::tempdir().unwrap();
        // 3 scattered matches at context 3 → ~23 output ROWS but only 3 MATCHES.
        let lines: Vec<String> = (1..=30)
            .map(|i| {
                if i % 10 == 5 {
                    format!("HIT {i}")
                } else {
                    format!("line {i}")
                }
            })
            .collect();
        std::fs::write(d.path().join("f.txt"), lines.join("\n") + "\n").unwrap();
        // max_results 10: output rows (23) >= 10 but matches (3) < 10 → must NOT report capped.
        let r = GrepTool
            .execute(
                r#"{"pattern":"HIT","max_results":10,"context":3}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            r.content.matches("HIT").count(),
            3,
            "exactly 3 matches: {}",
            r.content
        );
        assert!(
            !r.content.contains("Results capped"),
            "false 'capped' with only 3<10 matches: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn skips_binary_files() {
        let d = tempfile::tempdir().unwrap();
        // A NUL byte ⇒ binary ⇒ the searcher quits and reports nothing for it.
        std::fs::write(d.path().join("blob"), b"\x00 NEEDLE inside binary\n").unwrap();
        std::fs::write(d.path().join("text.txt"), "NEEDLE\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE"}"#, &ctx(d.path()))
            .await;
        assert!(
            r.content.contains("text.txt:1:"),
            "text match: {}",
            r.content
        );
        assert!(
            !r.content.contains("blob"),
            "binary file must be skipped: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn skips_gitignored_and_build_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("target/junk.rs"), "NEEDLE\n").unwrap();
        std::fs::write(d.path().join("keep.rs"), "NEEDLE\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE"}"#, &ctx(d.path()))
            .await;
        assert!(r.content.contains("keep.rs:1:"), "{}", r.content);
        assert!(
            !r.content.contains("junk.rs"),
            "target/ should be skipped: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn glob_restricts_files_at_any_depth() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "NEEDLE rust\n").unwrap();
        std::fs::write(d.path().join("src/a.txt"), "NEEDLE text\n").unwrap();
        std::fs::write(d.path().join("top.rs"), "NEEDLE top\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE","glob":"*.rs"}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("a.rs"), "{}", r.content);
        assert!(r.content.contains("top.rs"), "{}", r.content);
        assert!(
            !r.content.contains("a.txt"),
            "*.rs must not match .txt: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn default_context_is_zero() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("f.txt"), "line 1\nNEEDLE two\nline 3\n").unwrap();
        let r = GrepTool
            .execute(r#"{"pattern":"NEEDLE"}"#, &ctx(d.path()))
            .await;
        assert!(r.content.contains("f.txt:2:NEEDLE two"), "{}", r.content);
        assert!(
            !r.content.contains("f.txt-1-") && !r.content.contains("f.txt-3-"),
            "default context must be 0: {}",
            r.content
        );
    }

    #[test]
    fn description_forbids_constraint_description_payload() {
        let d = GrepTool.description();
        assert!(
            d.contains("never in `description`") || d.contains("NEVER put it in `description`"),
            "{d}"
        );
        assert!(d.contains("[Constraint: pattern:"), "{d}");
        assert!(d.contains("code_explore"), "{d}");
        let schema = GrepTool.parameters_schema();
        let pat = schema["properties"]["pattern"]["description"]
            .as_str()
            .unwrap_or("");
        assert!(
            pat.contains("Constraint") || pat.contains("description"),
            "{pat}"
        );
    }

    #[tokio::test]
    async fn recovers_pattern_from_query_alias_and_constraint_description() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "首次运行 ok\n").unwrap();
        let r = GrepTool
            .execute(
                r#"{"path":".","description":" [Constraint: pattern: 首次运行]"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("recovered"), "{}", r.content);
        assert!(r.content.contains("a.txt:1:"), "{}", r.content);

        let r2 = GrepTool
            .execute(r#"{"path":".","query":"首次运行"}"#, &ctx(d.path()))
            .await;
        assert!(!r2.is_error, "{}", r2.content);
        assert!(r2.content.contains("query"), "{}", r2.content);

        // Weak-model payload: path+pattern both stuffed into description.
        std::fs::create_dir_all(d.path().join("Models/Service")).unwrap();
        std::fs::write(
            d.path().join("Models/Service/CustomerRelationService.cs"),
            "GetSalePostSettingData() {}\n",
        )
        .unwrap();
        let r3 = GrepTool
            .execute(
                r#"{"description":" [Constraint: path: Models/Service/CustomerRelationService.cs, pattern: GetSalePostSettingData]"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r3.is_error, "{}", r3.content);
        assert!(
            r3.content.contains("GetSalePostSettingData"),
            "{}",
            r3.content
        );
        assert!(
            r3.content.contains("CustomerRelationService.cs"),
            "{}",
            r3.content
        );

        // Weak-model payload: pattern-only Constraint, no top-level pattern.
        std::fs::write(d.path().join("track.txt"), "positionTracking here\n").unwrap();
        let r4 = GrepTool
            .execute(
                r#"{"description":" [Constraint: pattern: positionTracking]"}"#,
                &ctx(d.path()),
            )
            .await;
        assert!(!r4.is_error, "{}", r4.content);
        assert!(r4.content.contains("positionTracking"), "{}", r4.content);
    }
}
