//! `list_directory` — recursive, indented directory tree (build/VCS/cache dirs
//! skipped). Non-destructive ⇒ always `Safe`.

use super::{err, is_skip_dir, not_found_hint, ok, resolve_path};
use crate::tool_feedback::parse_tool_args;
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// Entries shown in a folded result: first/last `FOLD_HALF` with a marker between.
const MAX_ENTRIES: usize = 350;
/// Half of the folded window — head and tail each keep this many lines.
const FOLD_HALF: usize = MAX_ENTRIES / 2;
/// Hard stop for the walk itself. Bounds the work while still collecting
/// enough lines past the cap for the tail half to be meaningful.
const COLLECT_CAP: usize = MAX_ENTRIES * 2;
const MAX_DEPTH_CAP: usize = 6;

pub struct ListDirTool;

#[derive(Deserialize)]
struct Args {
    #[serde(default, alias = "path")]
    target_directory: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_directory"
    }
    fn description(&self) -> &str {
        "List immediate files and subdirectories in a directory (respects .gitignore). \
         Use to inspect direct children of a known directory. For initial workspace exploration, pair with repo_map."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target_directory": {
                    "type": "string",
                    "default": ".",
                    "description": "Directory to list (default: working directory)."
                },
                "depth": {
                    "type": "integer",
                    "default": 1,
                    "description": "Recursion depth (default 1, max 6)."
                }
            }
        })
    }
    /// No side effects — a pure read. Makes it `parallel_safe` (concurrent
    /// execution) and allowed in plan mode.
    fn read_only_hint(&self) -> bool {
        true
    }
    // listing is non-destructive → risk() defaults to Safe.
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match parse_tool_args("list_directory", args, r#"{"target_directory":"<dir>"}"#) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        let raw = a.target_directory.unwrap_or_else(|| ".".to_string());
        let root = resolve_path(&raw, &ctx.working_dir);
        let depth = a.depth.unwrap_or(1).min(MAX_DEPTH_CAP);

        match tokio::fs::metadata(&root).await {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return err(format!(
                    "Not a directory: {}",
                    crate::pathnorm::to_display(&root)
                ))
            }
            Err(_) => {
                let hint = not_found_hint(&root, &ctx.working_dir).await;
                let base = format!(
                    "list_directory: Directory not found: {} (resolved to {})",
                    raw,
                    crate::pathnorm::to_display(&root)
                );
                let note = format!(
                    "\nNote: your current working directory is {}",
                    crate::pathnorm::to_display(&ctx.working_dir)
                );
                return err(format!("{base}{note}{hint}"));
            }
        }

        let root2 = root.clone();
        let lines = match tokio::task::spawn_blocking(move || collect_tree(&root2, depth)).await {
            Ok(v) => v,
            Err(_) => return err("list_directory: scan task failed".to_string()),
        };
        // An EMPTY directory is a valid result, not a failure: report it as an
        // explicit "empty" line so the model can distinguish it from an error.
        if lines.is_empty() {
            return ok("(empty directory)".to_string());
        }

        let total = lines.len();
        let out = if total <= MAX_ENTRIES {
            lines
                .iter()
                .map(|(_, l)| l.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            fold(&lines)
        };
        ok(out)
    }
}

/// Fold an oversized listing so the parts an agent actually needs survive:
///
/// 1. EVERY top-level entry (depth 0 — the project/subdirectory names) is kept.
/// 2. Children appear directly under their corresponding parent directory,
///    preserving the hierarchical tree structure instead of dumping children at the bottom.
/// 3. An elided marker states how many entries were elided and how to see the rest.
fn fold(lines: &[(usize, String)]) -> String {
    let top_count = lines.iter().filter(|(d, _)| *d == 0).count();
    if top_count >= MAX_ENTRIES {
        // Flat tree (top-level rows alone overflow the budget): plain head+tail fold.
        let head = lines[..FOLD_HALF]
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = lines[lines.len() - FOLD_HALF..]
            .iter()
            .map(|(_, l)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let elided = lines.len() - FOLD_HALF * 2;
        return format!(
            "{head}\n  ... ({elided} entries elided; total {}; pass a smaller `depth` or a subdirectory `path` to see them)\n{tail}",
            lines.len()
        );
    }

    // Group tree by top-level entries to preserve tree hierarchy
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (d, line) in lines {
        if *d == 0 {
            groups.push((line.clone(), Vec::new()));
        } else if let Some(last) = groups.last_mut() {
            last.1.push(line.clone());
        }
    }

    let total_nested: usize = groups.iter().map(|g| g.1.len()).sum();
    let budget = MAX_ENTRIES.saturating_sub(top_count);
    if total_nested <= budget {
        let mut parts = Vec::new();
        for (header, children) in groups {
            parts.push(header);
            parts.extend(children);
        }
        return parts.join("\n");
    }

    let dirs_with_children = groups.iter().filter(|g| !g.1.is_empty()).count();
    let per_group_budget = if dirs_with_children > 0 {
        (budget / dirs_with_children).max(1)
    } else {
        budget
    };

    let mut parts = Vec::new();
    let mut shown_nested = 0;
    for (header, children) in groups {
        parts.push(header);
        let take_count = children.len().min(per_group_budget);
        parts.extend(children.into_iter().take(take_count));
        shown_nested += take_count;
    }

    let elided = total_nested.saturating_sub(shown_nested);
    parts.push(format!(
        "  ... ({elided} entries elided; total {}; all top-level entries shown; pass a smaller `depth` or a subdirectory `path` to see the rest)",
        lines.len()
    ));
    parts.join("\n")
}

/// Collect the tree as `(depth, line)` pairs in pre-order traversal:
/// Each directory's children are collected immediately following that directory,
/// preserving the true parent-child directory hierarchy.
fn collect_tree(root: &Path, max_depth: usize) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut entries: Vec<_> = match std::fs::read_dir(root) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return out,
    };
    entries.sort_by_key(|e| e.file_name());

    let top_count = entries.len();
    let nested_budget = COLLECT_CAP.saturating_sub(top_count);
    let dir_count = entries
        .iter()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .count();
    let per_dir_budget = if dir_count > 0 {
        (nested_budget / dir_count).max(20)
    } else {
        nested_budget
    };

    for e in entries {
        if out.len() >= COLLECT_CAP {
            break;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if is_skip_dir(&name) {
                out.push((0, format!("{name}/ (skipped)")));
                continue;
            }
            out.push((0, format!("{name}/")));
            if max_depth >= 1 {
                let remaining_global = COLLECT_CAP.saturating_sub(out.len());
                let this_budget = per_dir_budget.min(remaining_global);
                walk_nested(&e.path(), 1, max_depth, this_budget, &mut out);
            }
        } else {
            out.push((0, name));
        }
    }
    out
}

/// Depth-first walk for nested rows, appending children immediately under parent.
fn walk_nested(
    dir: &Path,
    depth: usize,
    max: usize,
    budget: usize,
    out: &mut Vec<(usize, String)>,
) {
    if depth > max {
        return;
    }
    let start_len = out.len();
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return, // unreadable subtree → silently skip (e.g. permission denied)
    };
    entries.sort_by_key(|e| e.file_name());
    let indent = "  ".repeat(depth);
    for e in entries {
        if out.len() - start_len >= budget {
            break;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if is_skip_dir(&name) {
                out.push((depth, format!("{indent}{name}/ (skipped)")));
                continue;
            }
            out.push((depth, format!("{indent}{name}/")));
            let remaining = budget.saturating_sub(out.len() - start_len);
            walk_nested(&e.path(), depth + 1, max, remaining, out);
        } else {
            out.push((depth, format!("{indent}{name}")));
        }
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
    async fn lists_tree_with_dirs_marked() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(d.path().join("README.md"), "# hi").unwrap();
        let r = ListDirTool.execute(r#"{"path":"."}"#, &ctx(d.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("src/"), "{}", r.content);
        assert!(r.content.contains("  main.rs"), "{}", r.content);
        assert!(r.content.contains("README.md"), "{}", r.content);
    }

    #[tokio::test]
    async fn default_depth_stops_at_immediate_children() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/nested")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), "fn main(){}").unwrap();
        std::fs::write(d.path().join("src/nested/lib.rs"), "").unwrap();
        let r = ListDirTool.execute(r#"{"path":"."}"#, &ctx(d.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("src/"), "{}", r.content);
        assert!(r.content.contains("main.rs"), "{}", r.content);
        assert!(
            !r.content.contains("lib.rs"),
            "default depth 1 must not list grandchildren: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn skips_build_dirs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("target/junk"), "x").unwrap();
        let r = ListDirTool.execute(r#"{"path":"."}"#, &ctx(d.path())).await;
        assert!(r.content.contains("target/ (skipped)"), "{}", r.content);
        assert!(!r.content.contains("junk"), "{}", r.content);
    }

    #[tokio::test]
    async fn invalid_json_args_error() {
        let d = tempfile::tempdir().unwrap();
        let r = ListDirTool.execute("{not valid json", &ctx(d.path())).await;
        assert!(
            r.is_error,
            "malformed args must surface an error, not silently default"
        );
        assert!(r.content.contains("invalid arguments"), "{}", r.content);
    }

    #[tokio::test]
    async fn missing_dir_errors() {
        let d = tempfile::tempdir().unwrap();
        let r = ListDirTool
            .execute(r#"{"path":"nope"}"#, &ctx(d.path()))
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("Directory not found"), "{}", r.content);
    }

    /// Still an error, but it must carry the recovery clue — otherwise the model just guesses
    /// a different wrong path next turn (see `not_found_hint`).
    #[tokio::test]
    async fn missing_dir_error_carries_the_nearest_existing_ancestor() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("settings.gradle"), "").unwrap();
        let r = ListDirTool
            .execute(r#"{"path":"app/src/main"}"#, &ctx(d.path()))
            .await;
        assert!(r.is_error);
        assert!(
            r.content.contains("Nearest existing directory"),
            "{}",
            r.content
        );
        assert!(r.content.contains("settings.gradle"), "{}", r.content);
    }

    #[tokio::test]
    async fn under_cap_is_untouched() {
        // Off-by-one guard: exactly MAX_ENTRIES lines must NOT be flagged as
        // truncated (no fold, no marker). This is the regression the old
        // `> MAX_ENTRIES` checks were about to cause at 201 lines.
        let d = tempfile::tempdir().unwrap();
        for i in 0..MAX_ENTRIES {
            std::fs::write(d.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let r = ListDirTool.execute(r#"{"path":"."}"#, &ctx(d.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            !r.content.contains("truncated") && !r.content.contains("elided"),
            "exactly {} entries must pass through untouched: {}",
            MAX_ENTRIES,
            r.content
        );
        assert!(
            r.content.contains("f000.txt")
                && r.content.contains(&format!("f{:03}.txt", MAX_ENTRIES - 1)),
            "all entries present: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn over_cap_folds_head_and_tail() {
        // Fold: first/last FOLD_HALF lines kept, middle elided with a count and
        // a recovery hint. The TAIL — where deep subdirectories land — must
        // survive (this was the C1 defect: tail was silently dropped).
        let d = tempfile::tempdir().unwrap();
        for i in 0..COLLECT_CAP {
            std::fs::write(d.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let r = ListDirTool.execute(r#"{"path":"."}"#, &ctx(d.path())).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("elided"), "{}", r.content);
        // The fold marker reports BOTH the total and the elided count.
        assert!(
            r.content.contains(&format!("total {COLLECT_CAP}")),
            "{}",
            r.content
        );
        let elided = COLLECT_CAP - FOLD_HALF * 2;
        assert!(
            r.content.contains(&format!("{elided} entries elided")),
            "{}",
            r.content
        );
        // Head survives.
        assert!(r.content.contains("f000.txt"), "{}", r.content);
        // Tail survives — the crux of the fix.
        assert!(
            r.content.contains(&format!("f{:03}.txt", COLLECT_CAP - 1)),
            "tail entry must survive the fold: {}",
            r.content
        );
        // An elided middle entry must NOT leak into the output.
        assert!(
            !r.content.contains(&format!("f{:03}.txt", FOLD_HALF)),
            "{}",
            r.content
        );
    }

    /// The regression this fix targets: on a multi-project workspace the rows
    /// the agent actually needs ("is there a grok-build/?") are the TOP-LEVEL
    /// entries. A plain line-order fold drowns them in the elided middle and
    /// the agent falls back to `bash ls` — which defeats the purpose of the
    /// native tool. Every depth-0 entry must survive the fold.
    #[tokio::test]
    async fn over_cap_keeps_all_top_level_entries() {
        let d = tempfile::tempdir().unwrap();
        // 6 top-level projects, each with 80 nested files → ~486 lines total.
        for p in 0..6 {
            let dir = d.path().join(format!("project{p}"));
            std::fs::create_dir_all(dir.join("src")).unwrap();
            for i in 0..80 {
                std::fs::write(dir.join("src").join(format!("f{i:03}.txt")), "x").unwrap();
            }
        }
        let r = ListDirTool
            .execute(r#"{"path":".","depth":3}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("elided"), "{}", r.content);
        // Every top-level entry survives — the crux of the fix.
        for p in 0..6 {
            assert!(
                r.content.contains(&format!("project{p}/")),
                "top-level project{p}/ must survive the fold: {}",
                r.content
            );
        }
        // The marker must say top-level entries are all shown.
        assert!(
            r.content.contains("all top-level entries shown"),
            "{}",
            r.content
        );
    }

    #[tokio::test]
    async fn target_directory_param_works() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("hello.txt"), "world").unwrap();
        let r = ListDirTool
            .execute(r#"{"target_directory":"."}"#, &ctx(d.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("hello.txt"), "{}", r.content);
    }
}
