//! `list_directory` — recursive, indented directory tree (build/VCS/cache dirs
//! skipped). Non-destructive ⇒ always `Safe`.

use super::{err, is_skip_dir, not_found_hint, ok, resolve_path};
use crate::tool_feedback::{format_path_not_found, parse_tool_args};
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
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_directory"
    }
    fn description(&self) -> &str {
        "List ONE directory like `ls` (indented; directories end with '/'). `depth` \
         default 1 = this directory plus immediate children (max 6). This is NOT a \
         workspace overview — use `repo_map` for that, and do not pair the two. \
         Build/VCS/cache directories (node_modules, .git, target, …) are skipped. \
         Relative paths resolve against the working directory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: the working directory)" },
                "depth": { "type": "integer", "description": "Max recursion depth (default 1, max 6). 1 = this directory plus immediate children. Workspace tree → repo_map." }
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
        let a: Args = match parse_tool_args(
            "list_directory",
            args,
            r#"{"path":"<dir>"}"#,
        ) {
            Ok(a) => a,
            Err(e) => return e.into_tool_result(),
        };
        let raw = a.path.unwrap_or_else(|| ".".to_string());
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
        let lines = match tokio::task::spawn_blocking(move || collect_tree(&root2, depth)).await
        {
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
///    A plain line-order head+tail fold drowns these in the elided middle: on a
///    multi-project workspace the rows the model needs ("is there a
///    `grok-build/`?") are exactly the ones that vanish, pushing it to fall
///    back to `bash ls` for the same information.
/// 2. The nested rows are then head+tail folded around a marker that states
///    how many were elided and how to see them.
///
/// Falls back to a plain head+tail fold for pathological flat trees whose
/// top-level rows alone overflow the budget.
fn fold(lines: &[(usize, String)]) -> String {
    let top: Vec<&str> = lines
        .iter()
        .filter(|(d, _)| *d == 0)
        .map(|(_, l)| l.as_str())
        .collect();
    if top.len() >= MAX_ENTRIES {
        // Flat tree (everything at depth 0): plain head+tail fold.
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
    let nested: Vec<&str> = lines
        .iter()
        .filter(|(d, _)| *d > 0)
        .map(|(_, l)| l.as_str())
        .collect();
    let budget = MAX_ENTRIES.saturating_sub(top.len());
    let mut parts: Vec<String> = Vec::new();
    parts.extend(top.iter().map(|s| s.to_string()));
    if nested.len() <= budget {
        // Nothing actually elided — show all nested rows too.
        parts.extend(nested.iter().map(|s| s.to_string()));
        return parts.join("\n");
    }
    let half = budget / 2;
    let head_n = &nested[..half.min(nested.len())];
    let tail_start = nested.len().saturating_sub(half);
    let tail_n = &nested[tail_start..];
    let elided = nested.len().saturating_sub(half * 2);
    parts.extend(head_n.iter().map(|s| s.to_string()));
    parts.push(format!(
        "  ... ({elided} entries elided; total {}; all top-level entries shown; pass a smaller `depth` or a subdirectory `path` to see the rest)",
        lines.len()
    ));
    parts.extend(tail_n.iter().map(|s| s.to_string()));
    parts.join("\n")
}

/// Collect the tree as `(depth, line)` pairs with a structural budget:
///
/// 1. ALL top-level entries (depth 0) are always collected first — they are
///    the rows an agent needs to orient ("is there a `grok-build/`?") and
///    must never be starved out by a deep first subdirectory.
/// 2. Nested entries share the remaining budget (COLLECT_CAP - top-level
///    count), breadth-fairly: each directory gets a fair share of the nested
///    budget before any directory can consume it all.
///
/// Depth-first + a global cap starves the LAST top-level projects (they
/// appear after the first project's deep subtree ate the budget), which is
/// exactly the "grok-build/ and opencode/ invisible" failure.
fn collect_tree(root: &Path, max_depth: usize) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut entries: Vec<_> = match std::fs::read_dir(root) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return out,
    };
    entries.sort_by_key(|e| e.file_name());
    let indent = String::new();
    for e in &entries {
        if out.len() >= COLLECT_CAP {
            break;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if is_skip_dir(&name) {
                out.push((0, format!("{indent}{name}/ (skipped)")));
                continue;
            }
            out.push((0, format!("{indent}{name}/")));
        } else {
            out.push((0, format!("{indent}{name}")));
        }
    }
    // Nested share: everything after the top-level rows.
    let nested_budget = COLLECT_CAP.saturating_sub(out.len());
    let mut nested: Vec<(usize, String)> = Vec::new();
    for e in entries {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if is_skip_dir(&name) {
            continue;
        }
        walk_nested(&e.path(), 1, max_depth, nested_budget, &mut nested);
        if nested.len() >= nested_budget {
            break;
        }
    }
    out.extend(nested);
    out
}

/// Depth-first walk for nested rows, sharing the overall nested budget.
fn walk_nested(dir: &Path, depth: usize, max: usize, budget: usize, out: &mut Vec<(usize, String)>) {
    if depth > max || out.len() >= budget {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return, // unreadable subtree → silently skip (e.g. permission denied)
    };
    entries.sort_by_key(|e| e.file_name());
    let indent = "  ".repeat(depth);
    for e in entries {
        if out.len() >= budget {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if is_skip_dir(&name) {
                out.push((depth, format!("{indent}{name}/ (skipped)")));
                continue;
            }
            out.push((depth, format!("{indent}{name}/")));
            walk_nested(&e.path(), depth + 1, max, budget, out);
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
            r.content.contains("f000.txt") && r.content.contains(&format!("f{:03}.txt", MAX_ENTRIES - 1)),
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
        assert!(!r.content.contains(&format!("f{:03}.txt", FOLD_HALF)), "{}", r.content);
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
}
