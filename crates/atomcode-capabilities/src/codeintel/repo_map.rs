//! `repo_map` — high-density, multi-language architectural codebase map.
//!
//! Index-backed: reuses the shared [`CodeIndex`] the graph tools hold, so the
//! directory tree shows EXACTLY the files `code_explore` / `find_symbol` can
//! resolve — one source of truth, never a separate (and drifting) walk.
//!
//! The **full directory tree is never truncated**: every index-backed source
//! file is rendered, regardless of `max_files` or output-budget pressure. The
//! symbol-detail section below it is budgeted; when it is cut, an explicit
//! marker tells the model that the tree above is still complete and which
//! paths to drill into next (or that `mode: "tree"` / a `path:` scope can
//! trade detail for space).

use super::graph::CodeGraph;
use super::index::CodeIndex;
use super::{err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Default number of files whose SYMBOLS are rendered (the tree is unaffected).
const DEFAULT_MAX_FILES: usize = 100;
const MAX_ALLOWED_FILES: usize = 300;
const MAX_SYMBOLS_PER_FILE: usize = 40;
/// Budget for the symbol-detail section only; the tree above it is never cut.
const MAX_SYMBOL_OUTPUT_BYTES: usize = 64 * 1024;

/// Appended to every tree section: what the default tree shows, how to explore
/// deeper files with a scoped `code_explore` (never workspace-root `.`),
/// and the cross-check duty — if `code_explore`'s hits don't cover every
/// subdirectory the tree shows, re-explore the directories it missed.
const TREE_NOTE: &str = "\
NOTE: top-level files are listed in full; subdirectories are recursed to the \
deepest level but files inside them are only counted, not named — nothing is \
elided. To explore deeper files, pick a concrete subdirectory from this tree \
and call `code_explore` with that `path` (e.g. `crates/atomcode-coding`, \
`src/auth`, `backend`) — `path` is a directory/module, NEVER a single file \
(`.rs`/`.ts`/… that is `read_file`). do NOT pass `.` / the workspace root; \
that is reserved for `repo_map`. If hits miss a subdirectory shown here, \
re-explore that directory.";

pub struct RepoMapTool {
    index: Arc<CodeIndex>,
}

impl RepoMapTool {
    pub fn new(index: Arc<CodeIndex>) -> Self {
        Self { index }
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_files: Option<usize>,
    /// "tree" (default) = complete directory tree only (structure exploration);
    /// "full" = directory tree + budgeted symbol detail;
    /// "symbols" = symbol detail only.
    #[serde(default)]
    mode: Option<String>,
}

#[async_trait]
impl Tool for RepoMapTool {
    fn name(&self) -> &str {
        "repo_map"
    }

    fn description(&self) -> &str {
        "WHEN TO USE — FIRST CALL on any unfamiliar repo, BEFORE writing code or running deeper \
         searches. Prints the COMPLETE index-backed DIRECTORY TREE: every top-level file is listed \
         by name and every subdirectory is recursed to the deepest level (files inside deeper \
         directories are counted, not named — never elided), so you see the real module/layer \
         layout in one round. Do NOT also call list_directory — that is `ls` for one directory \
         you already know, not a second workspace tree.\n\
         \n\
         HOW IT FITS THE FLOW — structure first, then dive:\n\
         1. Round 1: `repo_map` ONLY (full layout) — never skip on an unfamiliar repo. Do not \
         pair it with list_directory.\n\
         2. Dive with several parallel `code_explore` calls (one per DIRECTORY/module + question \
         or symbol; never a file as `path`). `grep` only for exact literals. `read_file` only the \
         hot spans Coverage/CATALOG already named.\n\
         3. Only if you need actual file names under a specific dir you already know, use \
         list_directory (like `ls`, default depth 1); only to read a specific file's full body, \
         use read_file.\n\
         \n\
         MODES — default `tree` = structure only (small, never truncated). Pass `mode: full` \
         (tree + budgeted symbol outline) or `symbols` (symbols only) when you already know the \
         layout and need types/functions. In a multi-project workspace, pass `path:` to map ONE repo \
         at a time (the default spans ALL repos as separate subtrees; `.` is allowed here). To see \
         files under deeper directories, call `code_explore` with a concrete subdirectory from this \
         tree (directory/module, never a `.rs`/`.ts` file) — never `path: '.'`. If hits miss a \
         subdirectory shown here, re-explore that directory.\n\
         \n\
         CAUTION — a directory tree is NOT proof a mechanism is absent: it shows WHERE things live, \
         not WHAT exists. Empty-looking trees can hide code in sibling crates/layers (interface here, \
         impl elsewhere). Do not conclude 'project lacks X' from the tree alone — follow up with \
         `code_explore` on a concrete subdirectory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Subdirectory or workspace path to map (default: working directory root)"
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files whose SYMBOLS are rendered in mode full/symbols (default 100, max 300). The directory tree always shows every directory."
                },
                "mode": {
                    "type": "string",
                    "enum": ["tree", "full", "symbols"],
                    "description": "tree (default): complete directory tree only (structure exploration); full: directory tree + budgeted symbol detail; symbols: symbol detail only"
                }
            }
        })
    }

    fn read_only_hint(&self) -> bool {
        true
    }

    /// The complete directory tree is a structured, load-bearing payload: it
    /// must reach the model verbatim, so ArtifactMiddleware and the kernel size
    /// cap both skip it.
    fn never_truncate_result(&self) -> bool {
        true
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let t0 = std::time::Instant::now();
        let a: Args = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(_) => Args {
                path: None,
                max_files: None,
                mode: None,
            },
        };

        let target_dir = match a.path {
            Some(ref p) if !p.trim().is_empty() => {
                let resolved = if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    ctx.working_dir.join(p)
                };
                crate::pathnorm::canonicalize(&resolved).unwrap_or(resolved)
            }
            _ => crate::pathnorm::canonicalize(&ctx.working_dir).unwrap_or_else(|_| ctx.working_dir.clone()),
        };

        if !target_dir.exists() {
            return err(format!(
                "repo_map: target directory does not exist: {}",
                target_dir.display()
            ));
        }

        let max_files = a.max_files.unwrap_or(DEFAULT_MAX_FILES).clamp(1, MAX_ALLOWED_FILES);
        let mode = a
            .mode
            .unwrap_or_else(|| "tree".to_string())
            .to_ascii_lowercase();
        let working_dir = ctx.working_dir.clone();
        let _log_guard = super::index_log::ToolCallGuard::enter(
            "repo_map",
            json!({
                "path": a.path,
                "max_files": max_files,
                "mode": mode,
            }),
        );
        let index = self.index.clone();
        let log_root = working_dir.clone();

        let result = tokio::task::spawn_blocking(move || {
            build_repo_map(&index, &target_dir, &working_dir, max_files, &mode)
        })
        .await;

        match result {
            Ok(content) => {
                let cost_time = t0.elapsed();
                let stats = self.index.last_stats(&log_root);
                super::index_log::log_tool_call(
                    &log_root,
                    json!({
                        "outcome": "ok",
                        "total_ms": cost_time.as_millis() as u64,
                        "result_chars": content.len(),
                        "cache_hit": stats.as_ref().map(|s| s.cache_hit),
                        "reparsed": stats.as_ref().map(|s| s.reparsed),
                        "miss_files": stats.as_ref().map(|s| {
                            s.reparsed_files.iter().take(200).map(|p| p.display().to_string()).collect::<Vec<_>>()
                        }),
                    }),
                );
                ok(format!("> ⏱️ **Cost Time**: {}ms\n\n{content}", cost_time.as_millis()))
            }
            Err(e) => err(format!("repo_map execution failed: {e}")),
        }
    }
}

/// Score a relative path so key architectural files and entry points are
/// prioritized in the SYMBOL section (the tree keeps full order).
fn file_priority_score(rel_path: &str) -> i32 {
    let lower = rel_path.to_ascii_lowercase();
    let mut score = 0;

    // Entry points and exports
    if lower.contains("main.")
        || lower.contains("lib.")
        || lower.contains("index.")
        || lower.contains("app.")
        || lower.contains("mod.rs")
    {
        score += 50;
    }
    // Core contracts and schemas
    if lower.contains("types.")
        || lower.contains("schema.")
        || lower.contains("models.")
        || lower.contains("protocol.")
        || lower.contains("interface.")
    {
        score += 40;
    }
    // High-signal architectural layers
    if lower.contains("service")
        || lower.contains("controller")
        || lower.contains("agent")
        || lower.contains("kernel")
        || lower.contains("engine")
    {
        score += 30;
    }
    // Shallow depth bonus
    let depth = rel_path.split('/').count();
    score += (10 - depth.min(10)) as i32 * 5;

    // Deprioritize tests, mocks, examples, and generated fixtures
    if lower.contains("test")
        || lower.contains("mock")
        || lower.contains("spec")
        || lower.contains("fixture")
        || lower.contains("example")
    {
        score -= 60;
    }

    score
}

fn build_repo_map(
    index: &CodeIndex,
    target_dir: &Path,
    working_dir: &Path,
    max_symbol_files: usize,
    mode: &str,
) -> String {
    // Fully index-backed: the shared CodeGraph is the single source of truth.
    // `index.get(target_dir)` triggers an INCREMENTAL index of the target
    // directory on first use (fast multi-threaded unit sync — the same update
    // `atomcode init` produces), so a `path:` pointing at a never-indexed
    // directory is indexed on the spot, exactly like every other index-backed
    // tool (code_explore / find_symbol). The graph is rooted at `target_dir`,
    // so every indexed file lives inside it — no disk walk, no filter needed.
    let graph = index.get(target_dir);
    let files: Vec<PathBuf> = graph.file_symbols.keys().cloned().collect();
    if files.is_empty() {
        return "(no indexed source files found in target directory)".to_string();
    }

    let mut out = String::new();
    out.push_str("=== CODEBASE ARCHITECTURE MAP (index-backed) ===\n");
    out.push_str(&format!(
        "Overview: {} indexed source files\n",
        files.len()
    ));

    // Language distribution by extension (same matrix the index walks).
    let mut lang_counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in &files {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?")
            .to_ascii_lowercase();
        *lang_counts.entry(ext).or_default() += 1;
    }
    if !lang_counts.is_empty() {
        out.push_str(&format!(
            "Languages: {}\n",
            lang_counts
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Multi-repo workspace detection: when the target root itself contains
    // multiple git repos (a parent folder opened over several projects), say so
    // and steer the agent to map each repo with a `path:` scope — a tree over
    // the whole workspace mixes projects and can exceed output budgets.
    let sub_repos: Vec<String> = match std::fs::read_dir(target_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let p = entry.path();
                if p.is_dir() && p.join(".git").exists() {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if sub_repos.len() > 1 {
        out.push_str(&format!(
            "\n⚠️ Multi-repo workspace: {} git repos detected under the target root — \
             `{}`. The tree below spans ALL of them. For a focused map, call `repo_map` \
             with `path` set to one repo (e.g. `path: {}`).\n",
            sub_repos.len(),
            sub_repos.join("`, `"),
            sub_repos[0]
        ));
    }

    let want_tree = mode != "symbols";
    let want_symbols = mode != "tree";

    if want_tree {
        if sub_repos.len() > 1 {
            // Multi-repo workspace: render one subtree PER repo instead of one
            // giant tree that can blow host budgets. Stray root files (not in
            // any repo) are summarized as a count so nothing is silently hidden.
            out.push_str("\n-- DIRECTORY TREE (complete, per-repo) --\n");
            for repo in &sub_repos {
                let repo_dir = target_dir.join(repo);
                let repo_files: Vec<PathBuf> = files
                    .iter()
                    .filter(|p| path_within(p, &repo_dir))
                    .cloned()
                    .collect();
                out.push_str(&format!("{}/\n", repo));
                out.push_str(&render_dir_tree_indented(&repo_dir, &repo_files, "  "));
            }
            let stray = files
                .iter()
                .filter(|p| {
                    !sub_repos
                        .iter()
                        .any(|r| path_within(p, &target_dir.join(r)))
                })
                .count();
            if stray > 0 {
                out.push_str(&format!(
                    "(workspace root: {} file{})\n",
                    stray,
                    if stray == 1 { "" } else { "s" }
                ));
            }
            out.push_str(&format!("{TREE_NOTE}\n"));
        } else {
            out.push_str("\n-- DIRECTORY TREE (complete: every indexed directory) --\n");
            out.push_str(&render_dir_tree(target_dir, &files));
            out.push_str(&format!("{TREE_NOTE}\n"));
        }
    }

    if want_symbols {
        out.push_str("\n-- SYMBOL DETAIL (priority-ranked; budgeted) --\n");
        let (detail, cut) = render_symbols(&graph, working_dir, max_symbol_files);
        out.push_str(&detail);
        if cut {
            out.push_str(&format!(
                "\n[symbol detail cut at {}KB budget; the tree above is COMPLETE. \
                 Re-run with a narrower `path:` (or `mode: \"symbols\"` + a subdir) to see \
                 the remaining files' symbols.]\n",
                MAX_SYMBOL_OUTPUT_BYTES / 1024
            ));
        }
    }

    out
}

/// A complete, deterministic, compact DIRECTORY tree. The default view for
/// structure exploration: every top-level file AND every subdirectory (recursed
/// to the deepest level) is shown, with files under subdirectories summarized
/// as counts — small enough to never be truncated by the host while still
/// showing the full layout. `TREE_NOTE` explains how to explore deeper files.
///
/// Top-level files (entry points like `main.rs` / `Cargo.toml`) are listed in
/// full: they are the orientation rows the model needs by name.
/// Whether `p` lives under `dir`, compared on normalized absolute paths so
/// Windows separators / casing never cause a false negative (a bare
/// `starts_with` would also match `E:\agents\atomcode-x` under `E:\agents`).
/// The `\\?\` verbatim prefix is stripped first so graph paths (which carry it
/// on Windows) match plain test / user-supplied paths.
fn path_within(p: &Path, dir: &Path) -> bool {
    let norm = |x: &Path| {
        let s = x.to_string_lossy();
        let s = if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest
        } else {
            &s
        };
        s.replace('/', "\\").to_ascii_lowercase()
    };
    let p_n = norm(p);
    let d_n = norm(dir).trim_end_matches('\\').to_string();
    p_n.starts_with(&d_n)
        && (p_n.len() == d_n.len() || p_n[d_n.len()..].starts_with('\\'))
}

fn render_dir_tree(root: &Path, files: &[PathBuf]) -> String {
    render_dir_tree_indented(root, files, "")
}

/// Relative path from `root` to `p`, tolerant of the `\\?\` verbatim prefix
/// (graph paths carry it on Windows) and of separator/casing differences.
/// Returns `None` when `p` is not under `root` (or equals it).
fn rel_path(p: &Path, root: &Path) -> Option<PathBuf> {
    let norm = |x: &Path| {
        let s = x.to_string_lossy();
        let s = if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest
        } else {
            &s
        };
        s.replace('/', "\\").to_ascii_lowercase()
    };
    let p_n = norm(p);
    let r_n = norm(root).trim_end_matches('\\').to_string();
    if !(p_n.starts_with(&r_n)
        && (p_n.len() == r_n.len() || p_n[r_n.len()..].starts_with('\\')))
    {
        return None;
    }
    // Normalization is length-preserving (lowercase + `/`→`\` only), so slice
    // the ORIGINAL (case-preserving) string at the same offset.
    let s = p.to_string_lossy();
    let s = if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest
    } else {
        &s
    };
    let rest = s[r_n.len()..].trim_start_matches(['\\', '/']);
    if rest.is_empty() {
        return None;
    }
    Some(PathBuf::from(rest))
}

/// Render the directory tree with an indentation prefix (used to nest each
/// repo's subtree under its own header in a multi-repo workspace).
fn render_dir_tree_indented(root: &Path, files: &[PathBuf], indent: &str) -> String {
    #[derive(Default)]
    struct Dir {
        dirs: BTreeMap<String, Dir>,
        files: Vec<String>,
    }

    let mut top = Dir::default();
    for p in files {
        let Some(rel) = rel_path(p, root) else {
            continue;
        };
        let s = rel.to_string_lossy();
        let comps: Vec<String> = s
            .split(['\\', '/'])
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect();
        if comps.is_empty() {
            continue;
        }
        let mut node = &mut top;
        for comp in &comps[..comps.len() - 1] {
            node = node.dirs.entry(comp.clone()).or_default();
        }
        node.files.push(comps[comps.len() - 1].clone());
    }

    let mut out = String::new();
    fn emit(node: &Dir, prefix: &str, depth: usize, out: &mut String) {
        for (name, child) in &node.dirs {
            out.push_str(&format!("{prefix}{name}/\n"));
            emit(child, &format!("{prefix}  "), depth + 1, out);
        }
        if !node.files.is_empty() {
            let mut sorted = node.files.clone();
            sorted.sort();
            let count = sorted.len();
            if depth == 0 {
                // Top level of the mapped root: list EVERY file by name. These
                // are the orientation rows the model needs (entry points like
                // `main.rs` / `Cargo.toml`), so nothing is elided or folded.
                for f in &sorted {
                    out.push_str(&format!("{prefix}{f}\n"));
                }
            } else {
                // Deeper directories: directories themselves are recursed in
                // full; their files are counted, not named (see TREE_NOTE).
                let plural = if count == 1 { "" } else { "s" };
                out.push_str(&format!("{prefix}({count} file{plural})\n"));
            }
        }
    }
    emit(&top, indent, 0, &mut out);
    out
}

/// Render per-file symbol outlines from the shared graph, priority-ranked and
/// budgeted. Returns (text, was_cut).
fn render_symbols(graph: &CodeGraph, working_dir: &Path, max_symbol_files: usize) -> (String, bool) {
    // Rank files by architectural priority for the SYMBOL section.
    let mut ranked: Vec<(&PathBuf, &Vec<u64>)> = graph.file_symbols.iter().collect();
    ranked.sort_by(|a, b| {
        let ra = file_priority_score(
            &a.0.strip_prefix(working_dir).unwrap_or(a.0).to_string_lossy().replace('\\', "/"),
        );
        let rb = file_priority_score(
            &b.0.strip_prefix(working_dir).unwrap_or(b.0).to_string_lossy().replace('\\', "/"),
        );
        rb.cmp(&ra).then_with(|| a.0.cmp(b.0))
    });

    let mut out = String::new();
    let mut cut = false;
    for (path, ids) in ranked.iter().take(max_symbol_files) {
        let rel = path
            .strip_prefix(working_dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        let mut formatted: Vec<String> = Vec::new();
        for id in ids.iter() {
            if formatted.len() >= MAX_SYMBOLS_PER_FILE {
                break;
            }
            let Some(node) = graph.node(*id) else {
                continue;
            };
            if node.name.is_empty() {
                continue;
            }
            let kind_label = symbol_kind_label(&node.kind);
            formatted.push(format!("{kind_label} {}:{}", node.name, node.start_line));
        }

        out.push_str(&format!("  📄 {rel}\n"));
        if !formatted.is_empty() {
            out.push_str(&format!("     └─ {}\n", formatted.join(", ")));
        }

        if out.len() > MAX_SYMBOL_OUTPUT_BYTES {
            cut = true;
            break;
        }
    }
    (out, cut)
}

fn symbol_kind_label(kind: &super::graph::SymbolKind) -> &'static str {
    use super::graph::SymbolKind;
    match kind {
        SymbolKind::Function => "fn",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Class => "class",
        SymbolKind::Trait => "trait",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::Constant => "const",
        SymbolKind::Variable => "var",
        SymbolKind::Property => "prop",
        SymbolKind::Module => "mod",
        SymbolKind::Import => "import",
        SymbolKind::TypeAlias => "type",
        SymbolKind::RouteEndpoint => "route",
        SymbolKind::SqlStatement => "sql",
        SymbolKind::ConfigProperty => "config",
        SymbolKind::PluginDeclaration => "plugin",
        SymbolKind::Middleware => "middleware",
        SymbolKind::UiElement => "ui",
        SymbolKind::Other(_) => "sym",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_file_priority_scoring() {
        assert!(file_priority_score("src/main.rs") > file_priority_score("tests/fixture.rs"));
        assert!(file_priority_score("crates/auth/src/types.rs") > file_priority_score("crates/auth/src/util_mock.rs"));
    }

    #[tokio::test]
    async fn test_index_backed_repo_map() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // 1. Rust file
        std::fs::write(
            root.join("main.rs"),
            "pub struct ServerConfig { port: u16 }\npub fn start_server() {}\n",
        )
        .unwrap();

        // 2. Python file
        std::fs::write(
            root.join("app.py"),
            "class UserModel:\n    def __init__(self):\n        pass\n\ndef run_app():\n    pass\n",
        )
        .unwrap();

        // 3. TypeScript file
        std::fs::write(
            root.join("index.ts"),
            "export interface AuthPayload { token: string; }\nexport function verifyToken() {}\n",
        )
        .unwrap();

        // 4. Go file
        std::fs::write(
            root.join("service.go"),
            "package main\ntype OrderService struct {}\nfunc ProcessOrder() {}\n",
        )
        .unwrap();

        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "full");
        assert!(map_output.contains("CODEBASE ARCHITECTURE MAP"));
        assert!(map_output.contains("DIRECTORY TREE"));
        // Complete tree shows EVERY file (never max_files-truncated).
        assert!(map_output.contains("main.rs"));
        assert!(map_output.contains("app.py"));
        assert!(map_output.contains("index.ts"));
        assert!(map_output.contains("service.go"));
        // Symbol detail from the shared graph.
        assert!(map_output.contains("ServerConfig"));
        assert!(map_output.contains("UserModel"));
        assert!(map_output.contains("AuthPayload"));
        assert!(map_output.contains("OrderService"));
    }

    #[tokio::test]
    async fn test_tree_mode_skips_symbols_and_is_complete() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "pub fn b() {}\n").unwrap();

        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "tree");
        assert!(map_output.contains("DIRECTORY TREE"));
        assert!(!map_output.contains("SYMBOL DETAIL"));
        // Tree mode lists EVERY top-level file by name (no count summary, no fold).
        assert!(map_output.contains("a.rs"));
        assert!(map_output.contains("b.rs"));
        assert!(!map_output.contains("(2 files"));
        // The TREE_NOTE explaining deeper-file exploration is present.
        assert!(map_output.contains("code_explore"));
    }

    #[tokio::test]
    async fn test_tree_mode_recurses_subdirs_and_counts_deep_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        // Top-level files: listed in full.
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        // Nested dirs: recursed to the deepest level; deep files counted, not named.
        std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
        std::fs::write(root.join("crates/core/src/lib.rs"), "pub fn core() {}\n").unwrap();
        std::fs::write(root.join("crates/core/src/util.rs"), "pub fn util() {}\n").unwrap();

        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "tree");
        // Top-level file names appear verbatim; no root count summary.
        assert!(map_output.contains("main.rs"));
        assert!(!map_output.contains("(1 file"));
        // Every subdirectory recursed to the deepest level.
        assert!(map_output.contains("crates/"));
        assert!(map_output.contains("core/"));
        assert!(map_output.contains("src/"));
        // Deep files are counted, not named; nothing is elided or folded.
        assert!(map_output.contains("(2 files"));
        assert!(!map_output.contains("util.rs"));
        // The TREE_NOTE explains deeper-file exploration via scoped code_explore.
        assert!(map_output.contains("code_explore"));
        assert!(map_output.contains("nothing is elided"));
        assert!(
            map_output.contains("do NOT pass `.`") || map_output.contains("do NOT pass"),
            "TREE_NOTE must forbid workspace-root code_explore:\n{map_output}"
        );
    }

    #[tokio::test]
    async fn test_default_mode_is_tree() {
        // The tool's default (no mode arg) must be the small structure-only view,
        // so a huge workspace never produces a host-truncated blob.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn b() {}\n").unwrap();

        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "tree");
        assert!(map_output.contains("src/"));
        assert!(map_output.contains("(1 file"));
        assert!(!map_output.contains("SYMBOL DETAIL"));
        assert!(!map_output.contains("fn a:"));
    }

    #[tokio::test]
    async fn test_multi_repo_workspace_is_flagged() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for repo in ["repo-a", "repo-b"] {
            let r = root.join(repo);
            std::fs::create_dir_all(r.join("src")).unwrap();
            std::fs::write(r.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
            std::fs::create_dir(r.join(".git")).unwrap();
        }
        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "tree");
        assert!(
            map_output.contains("Multi-repo workspace"),
            "must flag a multi-repo workspace root"
        );
        assert!(map_output.contains("repo-a") && map_output.contains("repo-b"));
    }

    #[tokio::test]
    async fn test_symbols_mode_omits_tree() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();

        let index = Arc::new(CodeIndex::new());
        let map_output = build_repo_map(&index, root, root, 10, "symbols");
        assert!(map_output.contains("SYMBOL DETAIL"));
        assert!(!map_output.contains("DIRECTORY TREE"));
        assert!(map_output.contains("fn a:1"));
    }
}
