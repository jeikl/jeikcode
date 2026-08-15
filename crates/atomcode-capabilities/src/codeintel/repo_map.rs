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
use super::index::{collect_source_paths, CodeIndex};
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
        "MANDATORY Round 1 architecture radar: prints the COMPLETE index-backed DIRECTORY \
         tree (every indexed directory — files summarized per directory as a count) plus, \
         optionally, a budgeted AST symbol outline. Default `mode: tree` is structure-only \
         so the output stays small enough to never be truncated by the host; pass \
         `mode: full` (or `symbols`) when you also need types/functions, and narrow with \
         `path:` for a single repo in a multi-project workspace. ALWAYS call this in \
         Round 1 on any unfamiliar project to see the real module/layer layout in 1 round \
         without blind searches."
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

    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
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
                resolved
            }
            _ => ctx.working_dir.clone(),
        };

        if !target_dir.exists() {
            return err(format!(
                "repo_map: target directory does not exist: {}",
                target_dir.display()
            ));
        }

        let max_files = a.max_files.unwrap_or(DEFAULT_MAX_FILES).min(MAX_ALLOWED_FILES);
        let mode = a
            .mode
            .unwrap_or_else(|| "tree".to_string())
            .to_ascii_lowercase();
        let working_dir = ctx.working_dir.clone();
        let index = self.index.clone();

        let result = tokio::task::spawn_blocking(move || {
            build_repo_map(&index, &target_dir, &working_dir, max_files, &mode)
        })
        .await;

        match result {
            Ok(content) => ok(content),
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
    // Shared, cache-warm walk: identical to what code_explore can resolve.
    let files = collect_source_paths(target_dir);
    if files.is_empty() {
        return "(no indexed source files found in target directory)".to_string();
    }

    let graph = index.get(target_dir);

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
        out.push_str("\n-- DIRECTORY TREE (complete: every indexed directory) --\n");
        out.push_str(&render_dir_tree(target_dir, &files));
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

/// A complete, deterministic, compact DIRECTORY tree (no file leaves). The
/// default view for structure exploration: small enough to never be truncated
/// by the host, while still showing every indexed directory.
///
/// Files directly under the root (entry points like `main.rs` / `Cargo.toml`)
/// are summarized as a count line so the tree stays directory-only but the
/// root's shape is not silently hidden.
fn render_dir_tree(root: &Path, files: &[PathBuf]) -> String {
    #[derive(Default)]
    struct Dir {
        dirs: BTreeMap<String, Dir>,
        files: usize,
    }

    let mut top = Dir::default();
    for p in files {
        let rel = match p.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => p,
        };
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if comps.is_empty() {
            continue;
        }
        let mut node = &mut top;
        for comp in &comps[..comps.len() - 1] {
            node = node.dirs.entry(comp.clone()).or_default();
        }
        node.files += 1;
    }

    let mut out = String::new();
    fn emit(node: &Dir, prefix: &str, out: &mut String) {
        for (name, child) in &node.dirs {
            out.push_str(&format!("{prefix}{name}/\n"));
            emit(child, &format!("{prefix}  "), out);
        }
        if node.files > 0 {
            out.push_str(&format!(
                "{prefix}({count} file{plural})\n",
                count = node.files,
                plural = if node.files == 1 { "" } else { "s" },
            ));
        }
    }
    emit(&top, "", &mut out);
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
        // Tree mode is directory-only: root files are summarized as a count,
        // never listed as leaves.
        assert!(map_output.contains("(2 files)"));
        assert!(!map_output.contains("a.rs"));
        assert!(!map_output.contains("b.rs"));
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
        assert!(map_output.contains("(1 file)"));
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
