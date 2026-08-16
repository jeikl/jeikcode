//! Code-intelligence capability (L1): tree-sitter symbol extraction + a cross-file
//! code graph, exposed as read-only tools. Sibling of `tools`/`provider` — depends only
//! on the kernel + tree-sitter/ignore.
//!
//! # Layers
//!
//! - **symbol layer** (single-file, STATELESS): `list_symbols` / `read_symbol` parse one
//!   file on demand — no shared state, nothing from the kernel `ToolContext` beyond
//!   `working_dir`.
//! - **graph layer** (cross-file): `find_references` (whole-word text scan) plus
//!   `trace_callers` / `trace_callees` / `trace_chain` / `blast_radius` /
//!   `file_dependencies`, backed by a shared, lazily-built [`CodeIndex`] (the symbol
//!   layer's statelessness ends here — these tools HOLD an `Arc<CodeIndex>`).
//!
//! Deferred vs production: visibility inference; import-aware call
//! resolution. Incremental indexing lives in [`CodeIndex`]. Behind the
//! opt-in `codeintel` cargo feature (12 grammars = heavy C compilation).

use atomcode_kernel::tool::{ProgressSink, ToolRegistry, ToolResult};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod bilingual_nlp;
pub mod blast_radius;
pub mod comment_index;
pub mod explore;
pub mod file_deps;
pub mod find_references;
pub mod find_symbol;
pub mod graph;
pub mod index;
pub mod lang;
pub mod list_symbols;
pub mod read_symbol;
pub mod repo_map;
pub mod retrieval;
pub mod symbols;
pub mod trace_callees;
pub mod trace_callers;
pub mod trace_chain;

#[cfg(feature = "lsp")]
pub mod diagnostics;
#[cfg(feature = "lsp")]
pub mod lsp;
#[cfg(feature = "lsp")]
pub mod lsp_tool;

pub use blast_radius::BlastRadiusTool;
pub use explore::CodeExploreTool;
pub use file_deps::FileDependenciesTool;
pub use find_references::FindReferencesTool;
pub use find_symbol::FindSymbolTool;
pub use graph::{CodeGraph, Edge, EdgeKind, SymbolId, SymbolKind, SymbolNode, Visibility};
pub use index::{
    build_graph, disk_cache_path, init_workspace_index, CodeIndex, IndexReport, DISK_CACHE_REL,
};
pub use lang::Lang;
pub use list_symbols::ListSymbolsTool;
pub use read_symbol::ReadSymbolTool;
pub use repo_map::RepoMapTool;
pub use symbols::{extract_symbol, extract_symbols, skeleton, Symbol};
pub use trace_callees::TraceCalleesTool;
pub use trace_callers::TraceCallersTool;
pub use trace_chain::TraceChainTool;

#[cfg(feature = "lsp")]
pub use diagnostics::DiagnosticsTool;
#[cfg(feature = "lsp")]
pub use lsp::LspManager;
#[cfg(feature = "lsp")]
pub use lsp_tool::LspTool;

/// Operational mode for code-intelligence tools.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CodeIntelMode {
    /// Default: Mount only the two high-density core tools (`repo_map` + `code_explore`).
    #[default]
    Unified,
    /// Full: Mount all tools including fine-grained inspection tools.
    Full,
    /// Custom: Mount explicit list of tools.
    Custom(Vec<String>),
}

impl CodeIntelMode {
    pub fn from_env_or_config(env_val: Option<&str>, config_mode: Option<&str>) -> Self {
        let val = env_val.or(config_mode).unwrap_or("unified").trim().to_ascii_lowercase();
        match val.as_str() {
            "full" | "all" | "granular" => Self::Full,
            "unified" | "compact" => Self::Unified,
            other if other.contains(',') => {
                let list = other.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                Self::Custom(list)
            }
            _ => Self::Unified,
        }
    }
}

/// Default unified tool names (repo_map + code_explore).
pub fn codeintel_unified_tool_names() -> &'static [&'static str] {
    &["repo_map", "code_explore"]
}

/// Names of all possible graph/symbol tools.
pub fn codeintel_tool_names() -> &'static [&'static str] {
    &[
        "repo_map",
        "code_explore",
        "list_symbols",
        "read_symbol",
        "find_symbol",
        "find_references",
        "trace_callers",
        "trace_callees",
        "trace_chain",
        "blast_radius",
        "file_dependencies",
    ]
}

/// Register codeintel tools using default mode (or environment ATOMCODE_CODEINTEL_MODE).
pub fn register_codeintel_tools(reg: &mut ToolRegistry) {
    let env_mode = std::env::var("ATOMCODE_CODEINTEL_MODE").ok();
    let mode = CodeIntelMode::from_env_or_config(env_mode.as_deref(), None);
    register_codeintel_tools_with_mode(reg, &mode);
}

/// Register graph/symbol tools according to the chosen [`CodeIntelMode`].
pub fn register_codeintel_tools_with_mode(reg: &mut ToolRegistry, mode: &CodeIntelMode) {
    let index = Arc::new(CodeIndex::new());
    index.start_background_refresh();

    match mode {
        CodeIntelMode::Unified => {
            reg.register(Arc::new(RepoMapTool::new(index.clone())));
            reg.register(Arc::new(CodeExploreTool::new(index)));
        }
        CodeIntelMode::Full => {
            reg.register(Arc::new(RepoMapTool::new(index.clone())));
            reg.register(Arc::new(ListSymbolsTool));
            reg.register(Arc::new(ReadSymbolTool));
            reg.register(Arc::new(FindReferencesTool));
            reg.register(Arc::new(FindSymbolTool::new(index.clone())));
            reg.register(Arc::new(TraceCallersTool::new(index.clone())));
            reg.register(Arc::new(TraceCalleesTool::new(index.clone())));
            reg.register(Arc::new(TraceChainTool::new(index.clone())));
            reg.register(Arc::new(BlastRadiusTool::new(index.clone())));
            reg.register(Arc::new(FileDependenciesTool::new(index)));
        }
        CodeIntelMode::Custom(tools) => {
            let set: HashSet<String> = tools.iter().map(|s| s.to_ascii_lowercase()).collect();
            if set.contains("repo_map") {
                reg.register(Arc::new(RepoMapTool::new(index.clone())));
            }
            if set.contains("code_explore") {
                reg.register(Arc::new(CodeExploreTool::new(index.clone())));
            }
            if set.contains("list_symbols") {
                reg.register(Arc::new(ListSymbolsTool));
            }
            if set.contains("read_symbol") {
                reg.register(Arc::new(ReadSymbolTool));
            }
            if set.contains("find_references") {
                reg.register(Arc::new(FindReferencesTool));
            }
            if set.contains("find_symbol") {
                reg.register(Arc::new(FindSymbolTool::new(index.clone())));
            }
            if set.contains("trace_callers") {
                reg.register(Arc::new(TraceCallersTool::new(index.clone())));
            }
            if set.contains("trace_callees") {
                reg.register(Arc::new(TraceCalleesTool::new(index.clone())));
            }
            if set.contains("trace_chain") {
                reg.register(Arc::new(TraceChainTool::new(index.clone())));
            }
            if set.contains("blast_radius") {
                reg.register(Arc::new(BlastRadiusTool::new(index.clone())));
            }
            if set.contains("file_dependencies") {
                reg.register(Arc::new(FileDependenciesTool::new(index)));
            }
        }
    }
}

/// A neutral language-server entry supplied by an L2 assembly. Capabilities cannot
/// depend on a product config type, so the coding layer maps `[lsp.servers]` once.
#[derive(Debug, Clone)]
pub struct LspServerSetting {
    pub command: String,
    pub args: Vec<String>,
    pub root_markers: Vec<String>,
}

/// Runtime policy for the optional LSP tool. Disabled by default and never downloads
/// binaries; `auto_detect` only enables the built-in mapping to locally installed ones.
#[derive(Debug, Clone)]
pub struct LspSettings {
    pub enabled: bool,
    pub auto_detect: bool,
    pub servers: HashMap<String, LspServerSetting>,
    pub settle_delay_ms: u64,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_detect: false,
            servers: HashMap::new(),
            settle_delay_ms: 150,
        }
    }
}

/// Register one shared, lazily-started `lsp` tool when the driver explicitly enables
/// it. Returns whether registration occurred so callers only mount an existing tool.
#[cfg(feature = "lsp")]
pub fn register_lsp_tool(reg: &mut ToolRegistry, settings: &LspSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    let mut servers = if settings.auto_detect {
        lsp::LspServerRegistry::with_defaults()
    } else {
        lsp::LspServerRegistry::empty()
    };
    for (extension, server) in &settings.servers {
        servers.insert(
            extension.trim_start_matches('.').to_ascii_lowercase(),
            lsp::LspServerConfig {
                command: server.command.clone(),
                args: server.args.clone(),
                root_markers: server.root_markers.clone(),
            },
        );
    }
    let manager = LspManager::with_registry_and_delay(servers, settings.settle_delay_ms);
    reg.register(Arc::new(LspTool::new(Arc::new(manager))));
    true
}

#[cfg(not(feature = "lsp"))]
pub fn register_lsp_tool(_reg: &mut ToolRegistry, _settings: &LspSettings) -> bool {
    false
}

// Local path/result helpers (kept independent of the `tools` feature). Leading-`~`
// expansion routes through the crate-shared `pathutil` so codeintel tools
// (`read_symbol`, `blast_radius`, …) resolve `~/x` the SAME as `read_file`/`bash`.
pub(crate) fn resolve_path(raw: &str, working_dir: &Path) -> PathBuf {
    if let Some(home) = crate::pathutil::expand_tilde(raw) {
        return home;
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        working_dir.join(p)
    }
}

/// Canonicalize a path (resolve symlinks / `.`/`..`), falling back to the original on
/// error. The graph build AND the tool lookups both canonicalize, so a file referenced
/// via a different alias (e.g. macOS `/var` vs `/private/var`) still matches the graph's
/// stored paths instead of a false "not found".
pub(crate) fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
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

/// Display a path relative to `root` when possible, else shortened to `.../last3`.
pub(crate) fn display_path(p: &Path, root: &Path) -> String {
    if let Ok(rel) = p.strip_prefix(root) {
        return rel.display().to_string();
    }
    let comps: Vec<_> = p.components().collect();
    if comps.len() <= 3 {
        p.display().to_string()
    } else {
        format!(
            ".../{}",
            comps[comps.len() - 3..]
                .iter()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        )
    }
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

/// Load (or rebuild) the shared workspace code graph, streaming index progress to
/// the driver so large C#/monorepo cold starts do not look hung.
pub(crate) fn load_graph(
    index: &CodeIndex,
    root: &Path,
    progress: &ProgressSink,
) -> Arc<CodeGraph> {
    index.get_with_progress(root, &|msg| progress.emit(msg))
}

/// Append to graph-tool `description()` string literals via `concat!`.
macro_rules! graph_tool_desc {
    ($desc:expr) => {
        concat!(
            $desc,
            " Uses a workspace code graph with incremental per-file units: the FIRST call \
             indexes the repo (can take minutes on large C# monorepos); later edits/git pulls \
             only re-parse changed files. Prefer list_symbols/read_symbol for single-file work. \
             Warm with one find_symbol first if the index is cold."
        )
    };
}
pub(crate) use graph_tool_desc;
