//! Code-intelligence capability (L1): tree-sitter symbol extraction + a cross-file
//! code graph, exposed as two read-only tools (`repo_map` + `code_explore`).
//! Sibling of `tools`/`provider` — depends only on the kernel + tree-sitter/ignore.
//!
//! Incremental indexing lives in [`CodeIndex`]. Behind the opt-in `codeintel`
//! cargo feature (12 grammars = heavy C compilation).

use atomcode_kernel::tool::{ToolRegistry, ToolResult};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod bilingual_nlp;
pub mod comment_index;
pub mod explore;
pub mod graph;
pub mod index;
pub mod index_db;
pub mod index_log;
pub mod lang;
pub mod repo_map;
pub mod retrieval;
pub mod symbols;

pub use explore::CodeExploreTool;
pub use graph::{CodeGraph, Edge, EdgeKind, SymbolId, SymbolKind, SymbolNode, Visibility};
pub use index::{
    build_graph, disk_cache_path, has_nonempty_codegraph, init_workspace_index, CodeIndex,
    IndexReport,
};
pub use index_db::DISK_CACHE_REL_DB;
pub use lang::Lang;
pub use repo_map::RepoMapTool;
pub use symbols::{extract_symbol, extract_symbols, skeleton, Symbol};

/// Operational mode for code-intelligence tools.
/// Fine-grained graph tools were removed; every mode mounts `repo_map` + `code_explore`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CodeIntelMode {
    #[default]
    Unified,
}

impl CodeIntelMode {
    pub fn from_env_or_config(_env_val: Option<&str>, _config_mode: Option<&str>) -> Self {
        Self::Unified
    }
}

/// Model-facing codeintel tools (`repo_map` + `code_explore`).
pub fn codeintel_unified_tool_names() -> &'static [&'static str] {
    &["repo_map", "code_explore"]
}

/// Same as [`codeintel_unified_tool_names`].
pub fn codeintel_tool_names() -> &'static [&'static str] {
    codeintel_unified_tool_names()
}

/// Register codeintel tools using default mode (or environment ATOMCODE_CODEINTEL_MODE).
pub fn register_codeintel_tools(reg: &mut ToolRegistry) {
    let env_mode = std::env::var("ATOMCODE_CODEINTEL_MODE").ok();
    let mode = CodeIntelMode::from_env_or_config(env_mode.as_deref(), None);
    register_codeintel_tools_with_mode(reg, &mode);
}

/// Process-wide shared code index.
///
/// Every session/runtime that registers codeintel tools reuses this ONE
/// instance, so N concurrent sessions (daemon /chat, /live, CLI, subagents)
/// share a single in-memory graph + sidecar caches (`dirindex` / `idf_stats` /
/// `concept_vectors`) instead of N copies — the key optimization for the
/// 30-40 concurrent read-heavy session scenario. `CodeIndex` is already
/// thread-safe (Mutex + single-flight + background refresh guarded by an
/// AtomicBool), so sharing is safe. A session operating on a different
/// project root will swap the index via the normal per-root get path.
pub fn shared_code_index() -> Arc<CodeIndex> {
    static SHARED: std::sync::OnceLock<Arc<CodeIndex>> = std::sync::OnceLock::new();
    SHARED
        .get_or_init(|| {
            let idx = Arc::new(CodeIndex::new());
            idx.start_background_refresh();
            idx
        })
        .clone()
}

/// Notify the global shared code index that a file was modified/created/deleted.
/// Triggers a fast 1-3ms in-memory incremental patch.
pub fn notify_code_index_file_changed(path: &Path, content: Option<&str>) {
    let index = shared_code_index();
    let _ = index.update_single_file(path, content);
}

/// Asynchronously prewarms the shared code index for `root` on a detached background thread.
/// Completely non-blocking to startup, LLM streaming, and TUI events, ensuring cold-start
/// queries execute in milliseconds.
///
/// Only runs when `{root}/.atomcode/codegraph` already has a non-empty index.
/// Missing or empty graphs are never auto-created — the user must run
/// `jeikcode init .` for a first index. Incremental refresh of an existing
/// index stays in the background.
pub fn prewarm_code_index(root: &Path) {
    let root_buf = canonical(root);
    if !has_nonempty_codegraph(&root_buf) {
        return;
    }
    let index = shared_code_index();
    std::thread::Builder::new()
        .name("codeintel-prewarm".to_string())
        .spawn(move || {
            let _ = index.get(&root_buf);
            let _ = index.get_idf_stats(&root_buf);
            let _ = index.get_dirindex(&root_buf);
        })
        .ok();
}

/// Register `repo_map` + `code_explore`. Mode is ignored (fine-grained tools retired).
pub fn register_codeintel_tools_with_mode(reg: &mut ToolRegistry, _mode: &CodeIntelMode) {
    let index = shared_code_index();
    reg.register(Arc::new(RepoMapTool::new(index.clone())));
    reg.register(Arc::new(CodeExploreTool::new(index)));
}

/// Canonicalize a path (resolve symlinks / `.`/`..`), falling back to the original on
/// error. The graph build AND the tool lookups both canonicalize, so a file referenced
/// via a different alias (e.g. macOS `/var` vs `/private/var`) still matches the graph's
/// stored paths instead of a false "not found".
///
/// Windows note: `std::fs::canonicalize` yields `\\?\E:\...` (extended-length
/// prefix) while the ignore-walker emits plain `E:\...` paths — the mismatch
/// breaks `strip_prefix(root)` (used for scope matching, relative display, and
/// dedup keys) and pollutes `DiskCache.root` / sidecars with `\\?\` noise.
/// We strip the prefix here so EVERY consumer (IndexState.root, disk caches,
/// code_explore lookups, path_matches_scope) sees one consistent
/// path form that matches the graph's stored file paths.
pub(crate) fn canonical(p: &Path) -> PathBuf {
    let c = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    let stripped = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s);
    PathBuf::from(stripped)
}

/// Lowercase, unify slashes, strip `\\?\` / trailing separators — matching key only.
pub(crate) fn normalize_path_for_match(p: &Path) -> String {
    let s = p.to_string_lossy();
    let stripped = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s);

    let mut unified = stripped.replace('/', "\\").to_ascii_lowercase();
    while unified.len() > 1 && unified.ends_with('\\') {
        unified.pop();
    }
    unified
}

fn path_components(norm: &str) -> Vec<&str> {
    norm.split('\\')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect()
}

fn is_windows_drive(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

/// True when `needle` appears as consecutive path components inside `haystack`.
fn components_contains(haystack: &[&str], needle: &[&str]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Relative indexed file vs canonical absolute scope (or the reverse): a suffix
/// of one side lines up with a prefix of the other, covering the entire remaining
/// shorter slice. Overlap of 1 is allowed only when that *is* the shorter path.
fn components_align(a: &[&str], b: &[&str]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shorter = a.len().min(b.len());
    for i in 0..a.len() {
        for j in 0..b.len() {
            let n = (a.len() - i).min(b.len() - j);
            if n == 0 {
                continue;
            }
            if a[i..i + n] != b[j..j + n] {
                continue;
            }
            // Entire remaining of at least one side must be consumed.
            if a.len() - i != n && b.len() - j != n {
                continue;
            }
            if n >= 2 || n == shorter {
                return true;
            }
        }
    }
    false
}

/// Multi-format scope match: relative vs absolute, `/` vs `\`, UNC prefix,
/// and segment-boundary alignment (so `coupon-mall-demo/backend/...` matches
/// `E:\code\agents\coupon-mall-demo\backend\...`). Does **not** expand to
/// the whole workspace when the scope is simply empty.
pub(crate) fn path_matches_scope(file_path: &Path, scope: &Path) -> bool {
    let f_norm = normalize_path_for_match(file_path);
    let sc_norm = normalize_path_for_match(scope);
    if f_norm.is_empty() || sc_norm.is_empty() {
        return false;
    }
    if f_norm == sc_norm {
        return true;
    }

    let fc = path_components(&f_norm);
    let sc = path_components(&sc_norm);
    if fc.is_empty() || sc.is_empty() {
        return false;
    }

    let fc: &[&str] = if is_windows_drive(fc[0]) {
        &fc[1..]
    } else {
        &fc
    };
    let sc: &[&str] = if is_windows_drive(sc[0]) {
        &sc[1..]
    } else {
        &sc
    };
    if fc.is_empty() || sc.is_empty() {
        return false;
    }

    components_contains(fc, sc) || components_contains(sc, fc) || components_align(fc, sc)
}

/// Human-readable path for CLI/logs. Windows `canonicalize` yields `\\?\E:\...`, which
/// looks like mojibake/noise on GBK consoles — strip the extended-length prefix.
pub(crate) fn path_for_display(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix("//?/"))
        .unwrap_or(&s)
        .to_string()
}

pub(crate) fn ok(content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: String::new(),
        content: content.into(),
        is_error: false,
        images: vec![],
    }
}
pub(crate) fn err(content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: String::new(),
        content: content.into(),
        is_error: true,
        images: vec![],
    }
}

/// Strip a UTF-8 BOM (U+FEFF) prefix from source text.
///
/// Windows editors (Notepad / VS Code with "UTF-8 with BOM") frequently save
/// files with a BOM; without stripping, the FIRST symbol / first line of every
/// parsed file carries a `\u{feff}` prefix that breaks symbol-name matching
/// (code_explore name bonus), line-number rendering, thesaurus
/// parsing, and reference scans on Windows.
pub(crate) fn strip_utf8_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_strips_windows_extended_length_prefix() {
        // Windows canonicalize yields `\\?\E:\...`; the ignore-walker emits
        // plain `E:\...`. The stripped form must match the walker form so
        // strip_prefix(root) / scope matching / dedup keys all agree.
        let p = Path::new(r"\\?\E:\code\agents\atomcode");
        let c = canonical(p);
        let s = c.to_string_lossy();
        assert!(!s.starts_with(r"\\?\"), "prefix must be stripped, got: {s}");
        assert!(s.contains("E:"), "drive letter must survive: {s}");
        // Unix-style `//?/` variant is also stripped.
        let p2 = Path::new("//?/E:/code/agents");
        let c2 = canonical(p2);
        assert!(!c2.to_string_lossy().starts_with("//?/"));
    }

    #[test]
    fn canonical_keeps_plain_paths_unchanged() {
        // A normal (non-prefixed) path passes through untouched.
        let p = Path::new("E:/code/agents/atomcode");
        let c = canonical(p);
        assert_eq!(c, PathBuf::from("E:/code/agents/atomcode"));
    }

    #[test]
    fn path_matches_scope_aligns_absolute_scope_with_relative_index() {
        let rel = Path::new(
            "coupon-mall-demo/backend/src/main/java/com/demo/coupon/service/CouponBatchIssueService.java",
        );
        let abs_scope = Path::new(
            r"E:\code\agents\coupon-mall-demo\backend\src\main\java\com\demo\coupon\service",
        );
        assert!(
            path_matches_scope(rel, abs_scope),
            "canonicalized absolute scope must match a relative indexed file"
        );
        assert!(path_matches_scope(
            Path::new(
                r"E:\code\agents\coupon-mall-demo\backend\src\main\java\com\demo\coupon\service\CouponService.java"
            ),
            Path::new("coupon-mall-demo/backend/src/main/java/com/demo/coupon/service")
        ));
        assert!(!path_matches_scope(
            Path::new("atomcode/crates/atomcode-tuix/src/lib.rs"),
            abs_scope
        ));
    }
}
