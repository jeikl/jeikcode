//! `code_explore` — One-shot surgical code intelligence & flow exploration tool.
//!
//! Replaces multi-round search/read cycles by consolidating:
//! 1. Bilingual NLP + Dense Vector semantic intent understanding (Chinese/English)
//! 2. Proximity-bound comment & docstring matching
//! 3. Cross-layer flow spine extraction (Callers/Callees/Routes/SQL)
//! 4. Verbatim line-numbered source slicing (<line>\t<code>, Read-equivalent)
//! 5. Proportional adaptive token budgeting & Whole-file buy rules
//! 6. Session-level duplicate code suppression
//! 7. Multi-symbol extraction & overlapping line-range merging
//! 8. Test file deprioritization & Zero-hit rich diagnostic feedback

use super::bilingual_nlp::{
    calculate_lexical_similarity, calculate_text_similarity, derive_project_name_tokens,
    parse_bilingual_query_with_thesaurus, parse_field_qualified_query, DynamicThesaurus,
    ParsedQuery, SearchTokens,
};
use super::graph::{CodeGraph, EdgeKind, SymbolId, SymbolKind, SymbolNode};
use super::index::CodeIndex;
use super::{canonical, err, normalize_path_for_match, ok, path_matches_scope};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

const DEFAULT_MAX_FILES: usize = 12;
const MAX_ALLOWED_FILES: usize = 30;

/// Business hops drawn in the spine card (full spine is still computed).
const MAX_SPINE_HOPS: usize = 12;
/// Source spans bought into the evidence section (layer-diverse auction).
const MAX_EVIDENCE_SPANS: usize = 5;
/// Hard cap on verbatim source characters in the evidence section.
const EVIDENCE_BUDGET_CHARS: usize = 24_000;
/// Remaining (not-rendered) candidate rows in the catalog.
const MAX_CATALOG_REMAINING: usize = 15;
const MAX_ANCHOR_DIRS: usize = 5;
const MAX_GRAPH_DIRS: usize = 5;
/// Fold a span when it exceeds this many lines (keep head+tail).
const FOLD_AFTER_LINES: usize = 40;
const FOLD_HEAD_LINES: usize = 20;
const FOLD_TAIL_LINES: usize = 10;

/// Thread-safe session-level sent code ranges to avoid duplicate context bloat.
static SESSION_SENT_SPANS: std::sync::LazyLock<RwLock<HashMap<String, Vec<(usize, usize)>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

pub struct CodeExploreTool {
    index: Arc<CodeIndex>,
    thesaurus: Arc<RwLock<DynamicThesaurus>>,
}

/// Process-wide query-result cache, keyed by (graph fingerprint, query, scope,
/// max_files, bm25_enabled, concept_enabled). Stores the **body only** — never
/// Performance / Index Status. Those two lines are rewritten on every call
/// from this request's wall-clock and `last_stats`. Caching the full Markdown
/// used to freeze "增量补丁（重解析 2）" and the first-hit millisecond
/// snapshot across identical queries.
static QUERY_RESULT_CACHE: std::sync::LazyLock<
    RwLock<std::collections::HashMap<(u64, String, String, usize, bool, bool), String>>,
> = std::sync::LazyLock::new(|| RwLock::new(std::collections::HashMap::new()));
const QUERY_CACHE_MAX_ENTRIES: usize = 128;

#[allow(clippy::too_many_arguments)]
fn query_cache_get(
    fingerprint: u64,
    query: &str,
    scope: &str,
    max_files: usize,
    bm25_enabled: bool,
    concept_enabled: bool,
) -> Option<String> {
    let guard = QUERY_RESULT_CACHE.read().unwrap();
    guard
        .get(&(
            fingerprint,
            query.to_string(),
            scope.to_string(),
            max_files,
            bm25_enabled,
            concept_enabled,
        ))
        .cloned()
}

#[allow(clippy::too_many_arguments)]
fn query_cache_insert(
    fingerprint: u64,
    query: &str,
    scope: &str,
    max_files: usize,
    bm25_enabled: bool,
    concept_enabled: bool,
    output: String,
) {
    let mut guard = QUERY_RESULT_CACHE.write().unwrap();
    if guard.len() >= QUERY_CACHE_MAX_ENTRIES {
        // Simple FIFO eviction: drop the oldest entry.
        if let Some(oldest) = guard.keys().next().cloned() {
            guard.remove(&oldest);
        }
    }
    guard.insert(
        (
            fingerprint,
            query.to_string(),
            scope.to_string(),
            max_files,
            bm25_enabled,
            concept_enabled,
        ),
        strip_diagnostic_headers(&output),
    );
}

fn is_diagnostic_header(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("> ⚡ **Performance**")
        || t.starts_with("> ⚡ **Index Status**")
        || t.starts_with("> 🔄 **Index Status**")
}

/// Drop Cost Time / Index Status so the query cache never freezes them.
fn strip_diagnostic_headers(output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    for line in output.lines() {
        if is_diagnostic_header(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !output.is_empty() && !output.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

fn format_perf_header(
    total: Duration,
    index_t: Duration,
    ret_t: Duration,
    ren_t: Duration,
) -> String {
    format!(
        "> ⚡ **Performance**: ⏱️ **Cost Time**: {}ms (Index: {}ms | Retrieval: {}ms | Render: {}ms)\n",
        total.as_millis(),
        index_t.as_millis(),
        ret_t.as_millis(),
        ren_t.as_millis()
    )
}

fn format_index_status_header(stats: Option<&super::index::RefreshStats>) -> String {
    match stats {
        Some(stats) => index_status_line(stats),
        None => "> ⚡ **Index Status**: [Cache HIT] 内存索引已就绪\n".to_string(),
    }
}

/// Label from actual index work, not a reparsed-count threshold.
///
/// `reparsed > 8` used to be printed as `[Cache MISS]` even when 1万+ units were
/// reused (`kept` huge). That was a 64-file discover cap looking like a cold miss.
fn index_status_line(stats: &super::index::RefreshStats) -> String {
    if stats.cache_hit {
        format!(
            "> ⚡ **Index Status**: [Cache HIT] 内存索引已就绪（复用 {} 个文件单元，0 重建）\n",
            stats.kept
        )
    } else if stats.kept == 0 {
        format!(
            "> 🔄 **Index Status**: [Cache MISS] 全量重建（解析 {} 个文件，移除 {} 个）\n",
            stats.reparsed, stats.removed
        )
    } else {
        format!(
            "> ⚡ **Index Status**: [Incremental] 增量补丁（重解析 {} 个文件，保留 {} 个，移除 {} 个）\n",
            stats.reparsed, stats.kept, stats.removed
        )
    }
}

/// Prepend this-request diagnostic headers onto a cached (header-less) body.
fn with_fresh_diagnostic_headers(
    body: &str,
    cost: (Duration, Duration, Duration, Duration),
    stats: Option<&super::index::RefreshStats>,
) -> String {
    let (total, index_t, ret_t, ren_t) = cost;
    let mut out = String::with_capacity(body.len() + 256);
    out.push_str(&format_perf_header(total, index_t, ret_t, ren_t));
    out.push('\n');
    out.push_str(&format_index_status_header(stats));
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    out
}

#[cfg(test)]
fn query_cache_clear() {
    QUERY_RESULT_CACHE.write().unwrap().clear();
}

impl CodeExploreTool {
    pub fn new(index: Arc<CodeIndex>) -> Self {
        let mut dt = DynamicThesaurus::new();
        // Fork channel bootstrap: on first run, seed the user config dir with
        // this fork's bundled thesaurus + builtin-tools list. Idempotent —
        // existing files are NEVER overwritten (the user owns their copies),
        // so subsequent launches / upgrades leave user edits untouched.
        seed_fork_defaults();
        // Load default user-level thesaurus from ~/.atomcode/thesaurus (or ATOMCODE_HOME)
        let global_thesaurus = crate::paths::config_dir().join("thesaurus");
        if global_thesaurus.is_dir() {
            dt.load_from_dir(&global_thesaurus);
        }
        Self {
            index,
            thesaurus: Arc::new(RwLock::new(dt)),
        }
    }
}

/// Seed this fork's bundled defaults (thesaurus dictionaries + builtin-tools
/// list + mcp.json + .codegraphignore) into the user config dir, once per
/// file. Pure additive: an existing file (user-edited or previously seeded)
/// is left untouched.
fn seed_fork_defaults() {
    let dir = crate::paths::config_dir();
    let thes_dir = dir.join("thesaurus");
    // Best-effort: any failure is silently ignored (the user can copy the
    // assets manually; a read-only home must not break startup).
    let _ = std::fs::create_dir_all(&thes_dir);
    for name in [
        "admin_system.txt",
        "agent_core.txt",
        "ai_agent.txt",
        "ailaierp.txt",
        "computer_science.txt",
        "fullstack_dev.txt",
        "medical.txt",
        "robotics.txt",
        "web_http.txt",
    ] {
        let dest = thes_dir.join(name);
        if dest.exists() {
            continue;
        }
        if let Some(embedded) = THESAURUS_ASSETS.get(name) {
            let _ = std::fs::write(&dest, embedded);
        }
    }
    // builtin-tools catalog (which tool names may enter no_fold_tools).
    let tools_dest = dir.join("builtin-tools.txt");
    if !tools_dest.exists() {
        let _ = std::fs::write(&tools_dest, BUILTIN_TOOLS_ASSET);
    }
    // mcp.json — the fork's default MCP server wiring (brave-search /
    // ddg-search / js-reverse). Only seeded when the user has no mcp.json.
    let mcp_dest = dir.join("mcp.json");
    if !mcp_dest.exists() {
        let _ = std::fs::write(&mcp_dest, MCP_JSON_ASSET);
    }
    // .codegraphignore — the fork's default code-graph ignore rules (generated
    // artifacts / minified bundles / vendored deps). Only seeded when absent.
    let ignore_dest = dir.join(".codegraphignore");
    if !ignore_dest.exists() {
        let _ = std::fs::write(&ignore_dest, CODEGRAPH_IGNORE_ASSET);
    }
}

/// Bundled thesaurus dictionaries (this fork's domain word-lists), embedded so
/// a fresh install starts with the same thesaurus the fork was developed with.
static THESAURUS_ASSETS: std::sync::LazyLock<
    std::collections::HashMap<&'static str, &'static str>,
> = std::sync::LazyLock::new(|| {
    let mut m = std::collections::HashMap::new();
    m.insert(
        "admin_system.txt",
        include_str!("../../assets/thesaurus/admin_system.txt"),
    );
    m.insert(
        "agent_core.txt",
        include_str!("../../assets/thesaurus/agent_core.txt"),
    );
    m.insert(
        "ai_agent.txt",
        include_str!("../../assets/thesaurus/ai_agent.txt"),
    );
    m.insert(
        "ailaierp.txt",
        include_str!("../../assets/thesaurus/ailaierp.txt"),
    );
    m.insert(
        "computer_science.txt",
        include_str!("../../assets/thesaurus/computer_science.txt"),
    );
    m.insert(
        "fullstack_dev.txt",
        include_str!("../../assets/thesaurus/fullstack_dev.txt"),
    );
    m.insert(
        "medical.txt",
        include_str!("../../assets/thesaurus/medical.txt"),
    );
    m.insert(
        "robotics.txt",
        include_str!("../../assets/thesaurus/robotics.txt"),
    );
    m.insert(
        "web_http.txt",
        include_str!("../../assets/thesaurus/web_http.txt"),
    );
    m
});

/// Bundled builtin-tools catalog (what tool names may be whitelisted in
/// `[tools.tool_output] no_fold_tools`), embedded for first-run seeding.
static BUILTIN_TOOLS_ASSET: &str = include_str!("../../assets/builtin-tools.txt");

/// Bundled default MCP server wiring (brave-search / ddg-search / js-reverse),
/// seeded as `~/.atomcode/mcp.json` on first run (never overwrites existing).
static MCP_JSON_ASSET: &str = include_str!("../../assets/mcp.json");

/// Bundled default code-graph ignore rules (generated/minified artifacts,
/// vendored deps), seeded as `~/.atomcode/.codegraphignore` on first run.
static CODEGRAPH_IGNORE_ASSET: &str = include_str!("../../assets/.codegraphignore");

#[derive(Deserialize)]
struct Args {
    query: String,
    #[serde(default)]
    max_files: Option<usize>,
    #[serde(default)]
    path: Option<String>,
}

fn looks_like_single_file(p: &str) -> bool {
    let t = p.trim().trim_end_matches(['/', '\\']);
    let Some(ext) = Path::new(t).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rs" | "py"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "go"
            | "java"
            | "kt"
            | "cs"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "php"
            | "rb"
            | "swift"
            | "scala"
            | "vue"
            | "svelte"
            | "md"
            | "toml"
            | "json"
            | "yaml"
            | "yml"
            | "xml"
            | "sql"
            | "html"
            | "css"
            | "sh"
            | "txt"
            | "lock"
    )
}

fn file_scope_err(file: &Path, workspace: &Path) -> String {
    let rel = crate::pathnorm::to_display(file.strip_prefix(workspace).unwrap_or(file));
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| crate::pathnorm::to_display(p.strip_prefix(workspace).unwrap_or(p)))
        .filter(|s| !s.is_empty() && s != ".")
        .unwrap_or_else(|| "the parent module directory".to_string());
    format!(
        "code_explore `path` must be a directory or module, not a single file (`{rel}`). \
         A file path is `read_file` and misses the call graph. \
         Retry: path=`{parent}`  query=<precise symbol or Chinese/English question> \
         — put the symbol in `query`, never in `path`."
    )
}

/// Tokens that mean "the whole workspace".
fn is_workspace_root_token(p: &str) -> bool {
    let t = p.trim().trim_end_matches(['/', '\\']);
    matches!(t, "." | "./" | ".\\" | "~" | "")
}

#[derive(Debug, Clone)]
struct ScoredSymbol {
    node: SymbolNode,
    total_score: f64,
    name_score: f64,
    doc_score: f64,
    inline_score: f64,
    graph_mass: f64,
}

#[derive(Debug, Clone)]
struct FileCandidate {
    file: PathBuf,
    top_score: f64,
    symbols: Vec<ScoredSymbol>,
}

/// Discovers independent subproject roots in a workspace (directories containing
/// Cargo.toml, package.json, go.mod, pom.xml, pyproject.toml, build.gradle, or .git).
pub fn detect_subproject_roots(root: &Path) -> Vec<PathBuf> {
    let mut projects = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return projects;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist"
            {
                continue;
            }
            if path.join("Cargo.toml").exists()
                || path.join("package.json").exists()
                || path.join("go.mod").exists()
                || path.join("pom.xml").exists()
                || path.join("pyproject.toml").exists()
                || path.join("build.gradle").exists()
                || path.join(".git").exists()
            {
                projects.push(path);
            }
        }
    }
    projects
}

#[async_trait]
impl Tool for CodeExploreTool {
    fn name(&self) -> &str {
        "code_explore"
    }

    fn description(&self) -> &str {
        "PRIMARY code-intelligence tool. Search inside the workspace or a directory/module — never a file.\n\
         \n\
         path (required): workspace root or a directory/module.\n\
           GOOD: .   crates/atomcode-coding   src/auth   backend/service\n\
           BAD:  src/auth.rs   crates/foo/src/lib.rs   foo.go   (a file → use read_file)\n\
         query (required): the search term, either\n\
           - a precise symbol: CodeExploreTool, assemble_parts, AuthService\n\
           - natural Chinese or English: 鉴权怎么做, how does session compaction work\n\
           Put the symbol/question HERE, not in path. This field is `query`, never `description`.\n\
         \n\
         Inside that directory the index finds the symbol or the feature and returns the call graph \
         plus verbatim source. Narrowing path to one file misses callers/callees and is identical to \
         read_file — do not do that. Prefer this over grep+read. Fire several scoped DIRECTORY calls \
         in parallel. A thin result is INCONCLUSIVE: retry synonyms / a broader directory; CATALOG \
         files are related, not absence. Reserve grep for exact literals."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "REQUIRED. Workspace root ('.', './', '~', or the working directory) or a directory/module such as 'crates/atomcode-coding', 'src/auth', 'backend/service'. NEVER a file ('src/auth.rs', 'lib.rs', 'foo.go') — that is read_file and will error. Put the symbol or question in `query`, not here."
                },
                "query": {
                    "type": "string",
                    "description": "REQUIRED. Precise symbol name (CodeExploreTool, assemble_parts) OR natural language in Chinese or English (鉴权怎么做, how does X work). This is `query`, never `description` or `pattern`. Do not put the symbol in `path`."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to render source code from (default: 12, max: 30)"
                }
            },
            "required": ["query", "path"]
        })
    }

    fn read_only_hint(&self) -> bool {
        true
    }

    /// code_explore 的输出是模型做架构判断的完整上下文(候选表/目录全景/
    /// 调用链/源码片段),中途折叠成「预览+fetch_output 取回」会打断阅读流。
    /// 与 repo_map 同款:声明完整载荷 → ArtifactMiddleware 与内核大小上限都跳过,
    /// 直接全量展示(超大仓库场景仍受工具自身的 max_files/span 折叠控制)。
    fn never_truncate_result(&self) -> bool {
        true
    }

    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: Args = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Invalid arguments for code_explore: {e}")),
        };

        if a.query.trim().is_empty() {
            return err("code_explore requires a non-empty `query`".to_string());
        }

        let path_str = match a.path.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p,
            _ => {
                return err(
                    "code_explore requires a `path` parameter naming the workspace root or a directory/module (e.g. '.', 'src/tools', 'crates/atomcode-coding', 'backend').".to_string()
                );
            }
        };
        if looks_like_single_file(path_str) {
            let resolved = if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                ctx.working_dir.join(path_str)
            };
            return err(file_scope_err(&resolved, &ctx.working_dir));
        }

        let root = canonical(&ctx.working_dir);
        let max_files = a
            .max_files
            .unwrap_or(DEFAULT_MAX_FILES)
            .clamp(1, MAX_ALLOWED_FILES);
        let _log_guard = super::index_log::ToolCallGuard::enter(
            "code_explore",
            json!({
                "query": a.query,
                "path": path_str,
                "max_files": max_files,
            }),
        );

        // Load project-specific thesaurus from `.atomcode/thesaurus`
        {
            let project_thesaurus_dir = root.join(".atomcode").join("thesaurus");
            if project_thesaurus_dir.is_dir() {
                if let Ok(mut th) = self.thesaurus.write() {
                    th.load_from_dir(&project_thesaurus_dir);
                }
            }
        }

        let parsed_query = parse_field_qualified_query(&a.query);
        let project_tokens = derive_project_name_tokens(&root);
        let search_text = if parsed_query.clean_text.is_empty() {
            &a.query
        } else {
            &parsed_query.clean_text
        };

        let thesaurus_guard = self.thesaurus.read().unwrap();
        let query_tokens = parse_bilingual_query_with_thesaurus(search_text, &thesaurus_guard);
        drop(thesaurus_guard);

        let t0 = std::time::Instant::now();

        let scope_path = if !parsed_query.path_filters.is_empty() {
            let p = &parsed_query.path_filters[0];
            let pb = Path::new(p);
            if is_workspace_root_token(p) {
                Some(root.clone())
            } else if pb.is_absolute() {
                Some(canonical(pb))
            } else {
                Some(canonical(&root.join(pb)))
            }
        } else {
            a.path.as_deref().map(|p| {
                let pb = Path::new(p);
                if is_workspace_root_token(p) {
                    root.clone()
                } else if pb.is_absolute() {
                    canonical(pb)
                } else {
                    canonical(&root.join(pb))
                }
            })
        };
        if let Some(sc) = scope_path.as_deref() {
            if sc.is_file() {
                return err(file_scope_err(sc, &root));
            }
        }

        let t_index_start = std::time::Instant::now();
        let graph = self.index.get_scoped(&root, scope_path.as_deref());
        let t_index = t_index_start.elapsed();

        // Query-result cache: look up AFTER get_scoped (fingerprint + last_stats
        // must reflect this restat) but BEFORE scoring / rendering. HIT skips
        // the 34万-symbol scan. Diagnostic headers are never cached — they are
        // rewritten from this request's wall-clock and last_stats.
        let fp = self.index.fingerprint(&root).unwrap_or(0);
        let scope_key = scope_path
            .as_deref()
            .map(|s| s.display().to_string())
            .unwrap_or_default();
        let bm25_enabled = std::env::var("ATOMCODE_EXPLORE_BM25").as_deref() == Ok("1");
        let concept_enabled = std::env::var("ATOMCODE_EXPLORE_CONCEPT").as_deref() == Ok("1");
        if let Some(cached_body) = query_cache_get(
            fp,
            &a.query,
            &scope_key,
            max_files,
            bm25_enabled,
            concept_enabled,
        ) {
            let t_retrieval = Duration::ZERO;
            let t_render = Duration::ZERO;
            let total_cost = t0.elapsed();
            let stats = self.index.last_stats(&root);
            log_explore_outcome(
                &root,
                &self.index,
                json!({
                    "outcome": "query_cache_hit",
                    "index_ms": t_index.as_millis() as u64,
                    "retrieval_ms": 0,
                    "render_ms": 0,
                    "total_ms": total_cost.as_millis() as u64,
                }),
            );
            return ok(with_fresh_diagnostic_headers(
                &cached_body,
                (total_cost, t_index, t_retrieval, t_render),
                stats.as_ref(),
            ));
        }

        // Step 1: Score all symbols in the workspace.
        let t_retrieval_start = std::time::Instant::now();
        // Opt-in BM25 lexical recall (ATOMCODE_EXPLORE_BM25=1): surface naming-plain
        // core files (run_loop.rs / turn.rs / tool_calls.rs) that the semantic-anchor
        // gate would otherwise drop before scoring.
        let bm25_scores: HashMap<SymbolId, f64> = {
            let enabled = std::env::var("ATOMCODE_EXPLORE_BM25").as_deref() == Ok("1");
            if enabled {
                // 复用 CodeIndex 缓存的 IDF 统计: 首次从 stats.v1.json 加载/
                // 构建后落盘, 之后每次查询(含所有共享该索引的会话)零重算。
                if let Some(stats) = self.index.get_idf_stats(&root) {
                    super::retrieval::bm25_search(
                        &graph,
                        &stats,
                        &query_tokens,
                        scope_path.as_deref(),
                        64,
                    )
                    .into_iter()
                    .map(|h| (h.node_id, h.score))
                    .collect()
                } else {
                    HashMap::new()
                }
            } else {
                HashMap::new()
            }
        };
        // Opt-in semantic concept-vector path (ATOMCODE_EXPLORE_CONCEPT=1):
        // project the query (with thesaurus expansions) onto concept axes so a
        // Chinese query can cosine-match English code without any model.
        let query_concept_vec: Vec<f32> = {
            let enabled = std::env::var("ATOMCODE_EXPLORE_CONCEPT").as_deref() == Ok("1");
            if enabled {
                super::retrieval::concept_projection(search_text, &query_tokens.expanded_terms)
            } else {
                Vec::new()
            }
        };
        // Concept vectors are opt-in. Building them walks every symbol in the
        // graph (31万 on a full ERP) — that was the 40–50s "Retrieval" stall
        // even when ATOMCODE_EXPLORE_CONCEPT was unset and the vectors unused.
        let concept_vectors = if concept_enabled {
            self.index.get_concept_vectors(&root)
        } else {
            None
        };
        let scored_files = score_workspace_symbols(
            &graph,
            &query_tokens,
            &parsed_query,
            &project_tokens,
            scope_path.as_deref(),
            &bm25_scores,
            &query_concept_vec,
            concept_vectors.as_deref(),
        );

        if scored_files.is_empty() {
            let total_nodes = graph.nodes.len();
            let mut files_in_scope = HashSet::new();
            let mut symbols_in_scope = 0;
            let mut lang_hints = HashSet::new();

            for (_, node) in &graph.nodes {
                if let Some(sc) = scope_path.as_deref() {
                    if path_matches_scope(&node.file, sc) {
                        files_in_scope.insert(&node.file);
                        symbols_in_scope += 1;
                        if let Some(ext) = node.file.extension().and_then(|e| e.to_str()) {
                            lang_hints.insert(ext);
                        }
                    }
                } else {
                    files_in_scope.insert(&node.file);
                    symbols_in_scope += 1;
                    if let Some(ext) = node.file.extension().and_then(|e| e.to_str()) {
                        lang_hints.insert(ext);
                    }
                }
            }

            let scope_desc = a.path.as_deref().unwrap_or("<entire workspace>");
            let langs: Vec<&str> = lang_hints.into_iter().collect();
            let query_terms_str = if !query_tokens.code_identifiers.is_empty() {
                format!(
                    "Identifiers: [{}] | Expanded terms: [{}]",
                    query_tokens.code_identifiers.join(", "),
                    query_tokens
                        .expanded_terms
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                format!("Keywords: [{}]", query_tokens.words.join(", "))
            };

            let disk_inspection = if let Some(sc) = scope_path.as_deref() {
                if sc.exists() {
                    let (disk_files, disk_exts) = count_disk_source_files(sc);
                    let mut ext_list: Vec<String> = disk_exts.into_iter().collect();
                    ext_list.sort();
                    if files_in_scope.is_empty() && disk_files > 0 {
                        format!("* ⚠️ Disk Inspection: Found {} source file(s) on disk {:?}, but 0 files indexed in memory. (Likely filtered by ignore rules or index needs rebuild).\n", disk_files, ext_list)
                    } else {
                        format!(
                            "* Disk Inspection: {} source file(s) on disk {:?}.\n",
                            disk_files, ext_list
                        )
                    }
                } else {
                    format!(
                        "* ⚠️ Disk Inspection: Specified path `{}` does not exist on disk.\n",
                        sc.display()
                    )
                }
            } else {
                String::new()
            };

            log_explore_outcome(
                &root,
                &self.index,
                json!({
                    "outcome": "zero_hit",
                    "index_ms": t_index.as_millis() as u64,
                    "scope_files": files_in_scope.len(),
                    "scope_symbols": symbols_in_scope,
                }),
            );
            return ok(format!(
                "🔍 Zero-Hit Diagnostic for query '{}':\n\
                 ⚠️ **0 hits ≠ feature absent** — an empty result is INCONCLUSIVE, not proof the \
                 feature does not exist (thresholds, per-file caps, index coverage, or synonyms can \
                 all hide matches).\n\
                 - Scope Path: `{}` (Matched {} files in index, {} indexed symbols, Language Exts: {:?})\n\
                 - Workspace Total: {} symbols indexed across {} files\n\
                 - Query Analysis: {}\n\
                 - Diagnostic Assessment:\n\
                 {}  * Scope file(s) contained {} AST symbols in memory.\n\
                   * None of the symbols/comments matched query tokens with threshold >= 12.0.\n\
                 👉 Suggested follow-ups (do at least one before concluding absence):\n\
                    1. Retry with synonyms / English terms / a shorter keyword (thesaurus pairs).\n\
                    2. Drop `path:`/`kind:`/`name:` filters or widen the scope.\n\
                    3. Check `repo_map` for available modules/exports.\n\
                    4. Grep the workspace for likely identifiers.",
                a.query,
                scope_desc,
                files_in_scope.len(),
                symbols_in_scope,
                langs,
                total_nodes,
                graph.nodes.values().map(|n| &n.file).collect::<HashSet<_>>().len(),
                query_terms_str,
                disk_inspection,
                symbols_in_scope
            ));
        }

        // Step 2: Attempt Flow Spine extraction from top seeds
        let (flow_spine, connected) = extract_flow_spine(&graph, &scored_files);
        let t_retrieval = t_retrieval_start.elapsed();

        // Step 3: Render. t_render wraps the real Markdown assembly, not just
        // get_dirindex / last_stats (those two used to make Render always 0ms).
        let dirindex = self.index.get_dirindex(&root);
        let cache_status = self.index.last_stats(&root);
        let t_render_start = std::time::Instant::now();
        let body = render_explore_output(
            &graph,
            &root,
            &a.query,
            &scored_files,
            &flow_spine,
            connected,
            max_files,
            scope_path.as_deref(),
            &query_tokens,
            dirindex.as_deref(),
            None,
            None,
        );
        let t_render = t_render_start.elapsed();
        let total_cost = t0.elapsed();
        let output = with_fresh_diagnostic_headers(
            &body,
            (total_cost, t_index, t_retrieval, t_render),
            cache_status.as_ref(),
        );
        query_cache_insert(
            fp,
            &a.query,
            &scope_key,
            max_files,
            bm25_enabled,
            concept_enabled,
            output.clone(),
        );

        log_explore_outcome(
            &root,
            &self.index,
            json!({
                "outcome": "ok",
                "index_ms": t_index.as_millis() as u64,
                "retrieval_ms": t_retrieval.as_millis() as u64,
                "render_ms": t_render.as_millis() as u64,
                "total_ms": total_cost.as_millis() as u64,
                "candidates": scored_files.len(),
                "shown": max_files.min(scored_files.len()),
                "result_chars": output.len(),
            }),
        );
        ok(output)
    }
}

fn log_explore_outcome(root: &Path, index: &CodeIndex, mut extra: serde_json::Value) {
    if let Some(stats) = index.last_stats(root) {
        if let Some(obj) = extra.as_object_mut() {
            obj.insert("cache_hit".into(), json!(stats.cache_hit));
            obj.insert("reparsed".into(), json!(stats.reparsed));
            obj.insert("kept".into(), json!(stats.kept));
            obj.insert("removed".into(), json!(stats.removed));
            if !stats.cache_hit || stats.reparsed > 0 {
                let files: Vec<String> = stats
                    .reparsed_files
                    .iter()
                    .take(200)
                    .map(|p| p.display().to_string())
                    .collect();
                obj.insert("miss_files".into(), json!(files));
                obj.insert("miss_file_count".into(), json!(stats.reparsed_files.len()));
            }
        }
    }
    super::index_log::log_tool_call(root, extra);
}

/// Helper to scan physical source files on disk for diagnostic reporting.
fn count_disk_source_files(path: &Path) -> (usize, HashSet<String>) {
    let mut count = 0;
    let mut exts = HashSet::new();
    if path.is_file() {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if super::index::is_indexed_ext(ext) {
                count += 1;
                exts.insert(ext.to_string());
            }
        }
    } else if path.is_dir() {
        let mut dirs = vec![path.to_path_buf()];
        let mut scanned = 0;
        while let Some(dir) = dirs.pop() {
            if scanned > 1000 {
                break;
            }
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                            if !name.starts_with('.')
                                && name != "node_modules"
                                && name != "target"
                                && name != "dist"
                            {
                                dirs.push(p);
                            }
                        }
                    } else if p.is_file() {
                        scanned += 1;
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            if super::index::is_indexed_ext(ext) {
                                count += 1;
                                exts.insert(ext.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    (count, exts)
}

/// Check whether a symbol or its surrounding comments have a genuine semantic/lexical anchor to the query.
fn has_genuine_match_anchor(tokens: &SearchTokens, node: &SymbolNode) -> bool {
    let node_name_lower = node.name.to_ascii_lowercase();
    let query_lower = tokens.raw_query.to_ascii_lowercase();

    // 1. Exact query match or exact symbol containment
    if tokens.raw_query.eq_ignore_ascii_case(&node.name)
        || node_name_lower == query_lower
        || (node_name_lower.contains(&query_lower) && query_lower.len() >= 4)
    {
        return true;
    }

    let is_single_identifier = tokens.cjk_phrases.is_empty()
        && !tokens.raw_query.contains(' ')
        && (!tokens.code_identifiers.is_empty() || tokens.words.len() <= 4);

    if is_single_identifier {
        // Single Identifier mode (e.g. "completeToolCall", "thisSymbolDoesNotExistAnywhere"):
        // Strict symbol lookup intent — reject random stop-word fragmentation.
        for id in &tokens.code_identifiers {
            let id_lower = id.to_ascii_lowercase();
            if node_name_lower == id_lower
                || (node_name_lower.contains(&id_lower) && id_lower.len() >= 4)
            {
                return true;
            }
            if id_lower.contains(&node_name_lower) && node_name_lower.len() >= 5 {
                return true;
            }
        }

        // Subword coverage threshold: require at least 50% of the identifier's subwords
        if tokens.words.len() >= 2 {
            let matched_subwords = tokens
                .words
                .iter()
                .filter(|w| w.len() >= 3 && node_name_lower.contains(*w))
                .count();
            if matched_subwords * 2 >= tokens.words.len() && matched_subwords >= 2 {
                return true;
            }
        }

        // For single identifier, only match docstring/comment if it contains the full identifier
        if let Some(doc) = &node.docstring {
            let doc_lower = doc.to_ascii_lowercase();
            if doc_lower.contains(&query_lower) {
                return true;
            }
        }
        for c in &node.inline_comments {
            let c_lower = c.to_ascii_lowercase();
            if c_lower.contains(&query_lower) {
                return true;
            }
        }

        return false;
    }

    // Natural Language / Multi-token Mode:
    // 2. Full Code identifier hits
    for id in &tokens.code_identifiers {
        let id_lower = id.to_ascii_lowercase();
        if node_name_lower.contains(&id_lower) {
            return true;
        }
    }

    // 3. Bilingual expanded domain terms in symbol name
    if tokens
        .expanded_terms
        .iter()
        .any(|term| node_name_lower.contains(term))
    {
        return true;
    }

    // 4. CJK phrases in symbol name
    if tokens.cjk_phrases.iter().any(|p| node.name.contains(p)) {
        return true;
    }

    // 5. Meaningful query words in symbol name (require >= 2 words or 1 significant word)
    let matched_words_sym = tokens
        .words
        .iter()
        .filter(|w| w.len() >= 3 && node_name_lower.contains(*w))
        .count();
    if matched_words_sym >= 2
        || tokens
            .words
            .iter()
            .any(|w| w.len() >= 5 && node_name_lower.contains(w))
    {
        return true;
    }

    // 6. Check docstring
    if let Some(doc) = &node.docstring {
        let doc_lower = doc.to_ascii_lowercase();
        if tokens.cjk_phrases.iter().any(|p| doc.contains(p))
            || tokens.expanded_terms.iter().any(|t| doc_lower.contains(t))
        {
            return true;
        }
        let matched_in_doc = tokens
            .words
            .iter()
            .filter(|w| w.len() >= 3 && doc_lower.contains(*w))
            .count();
        if matched_in_doc >= 2 {
            return true;
        }
    }

    // 7. Check inline comments
    for c in &node.inline_comments {
        let c_lower = c.to_ascii_lowercase();
        if tokens.cjk_phrases.iter().any(|p| c.contains(p))
            || tokens.expanded_terms.iter().any(|t| c_lower.contains(t))
        {
            return true;
        }
        let matched_in_comment = tokens
            .words
            .iter()
            .filter(|w| w.len() >= 3 && c_lower.contains(*w))
            .count();
        if matched_in_comment >= 2 {
            return true;
        }
    }

    // 8. Check structured comments
    for sc in &node.comments {
        let sc_lower = sc.text.to_ascii_lowercase();
        if tokens.cjk_phrases.iter().any(|p| sc.text.contains(p))
            || tokens.expanded_terms.iter().any(|t| sc_lower.contains(t))
        {
            return true;
        }
    }

    // 9. Check SQL predicates and string literals
    for sql in &node.sql_predicates {
        let sql_lower = sql.raw_clause.to_ascii_lowercase();
        if tokens
            .cjk_phrases
            .iter()
            .any(|p| sql.raw_clause.contains(p))
            || tokens.expanded_terms.iter().any(|t| sql_lower.contains(t))
            || tokens
                .words
                .iter()
                .any(|w| w.len() >= 3 && sql_lower.contains(w))
        {
            return true;
        }
    }
    for lit in &node.string_literals {
        let lit_lower = lit.to_ascii_lowercase();
        if tokens.cjk_phrases.iter().any(|p| lit.contains(p))
            || tokens.expanded_terms.iter().any(|t| lit_lower.contains(t))
            || tokens
                .words
                .iter()
                .any(|w| w.len() >= 3 && lit_lower.contains(w))
        {
            return true;
        }
    }

    false
}

/// Score all symbols across files using multi-field similarity & graph topology.
/// `bm25_scores` is the opt-in BM25 lexical recall (symbol id → raw BM25 score).
///
/// 4-Stage Hybrid Scoring Architecture:
/// 1. Multi-channel text similarity (branch comments, SQL predicates, docstrings, names)
/// 2. AST role & Active logic multipliers (core SQL/logic 1.5x, enums 1.3x, DTO dampening)
/// 3. BM25 / Concept vector fusion & Graph centrality
/// 4. Directory co-occurrence boost (boosting peers in algorithmic directories)
fn score_workspace_symbols(
    graph: &CodeGraph,
    tokens: &SearchTokens,
    parsed_query: &ParsedQuery,
    project_tokens: &HashSet<String>,
    scope: Option<&Path>,
    bm25_scores: &HashMap<SymbolId, f64>,
    query_concept_vec: &[f32],
    concept_vectors: Option<&std::collections::HashMap<SymbolId, Vec<f32>>>,
) -> Vec<FileCandidate> {
    let bm25_max = bm25_scores.values().copied().fold(0.0f64, f64::max);
    let name_filters_lower: Vec<String> = parsed_query
        .name_filters
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let path_filters_lower: Vec<String> = parsed_query
        .path_filters
        .iter()
        .map(|p| p.to_ascii_lowercase())
        .collect();

    // Score only in-scope files. A workspace-wide `par_iter` over 36万 symbols
    // just to `path_matches_scope`-reject 99% of them is why Retrieval was 5–7s
    // on a 32-file `path:` query. `path_sim` is also per-file, not per-symbol.
    let scoped_files: Vec<(&PathBuf, &Vec<SymbolId>)> = match scope {
        Some(sc) => graph
            .file_symbols
            .iter()
            .filter(|(f, _)| path_matches_scope(f, sc))
            .collect(),
        None => graph.file_symbols.iter().collect(),
    };
    // Ranking formula is unchanged; this only caps/binds the existing rayon
    // scan so a 31万-symbol ERP uses the same 2–8 worker band as indexing.
    let score_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 8);
    let score_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(score_threads)
        .thread_name(|i| format!("codegraph-score-{i}"))
        .build()
        .ok();
    let path_sims: HashMap<PathBuf, f64> = {
        let job = || {
            scoped_files
                .par_iter()
                .map(|(file, _)| {
                    (
                        (*file).clone(),
                        calculate_lexical_similarity(tokens, &file.to_string_lossy()),
                    )
                })
                .collect()
        };
        match score_pool.as_ref() {
            Some(p) => p.install(job),
            None => job(),
        }
    };
    let nodes: Vec<&SymbolNode> = scoped_files
        .iter()
        .flat_map(|(_, ids)| ids.iter().filter_map(|id| graph.nodes.get(id)))
        .collect();

    let scored: Vec<(PathBuf, ScoredSymbol)> = {
        let job = || {
            nodes
                .into_par_iter()
                .filter_map(|node| {
                    if !parsed_query.kind_filters.is_empty() {
                        let kind_str = format!("{:?}", node.kind).to_ascii_lowercase();
                        if !parsed_query
                            .kind_filters
                            .iter()
                            .any(|k| kind_str.contains(k))
                        {
                            return None;
                        }
                    }
                    if !name_filters_lower.is_empty() {
                        let name_lower = node.name.to_ascii_lowercase();
                        if !name_filters_lower.iter().any(|n| name_lower.contains(n)) {
                            return None;
                        }
                    }
                    if !path_filters_lower.is_empty() {
                        let f_lower = node.file.to_string_lossy().to_ascii_lowercase();
                        if !path_filters_lower.iter().any(|p| f_lower.contains(p)) {
                            return None;
                        }
                    }

                    let name_lex = calculate_lexical_similarity(tokens, &node.name);
                    // Dense name embedding only after a real token hit — not on every
                    // `*Order*Service` just because BKOrderType split to "order".
                    let name_sim = if name_lex > 0.0 {
                        calculate_text_similarity(tokens, &node.name)
                    } else {
                        0.0
                    };
                    let mut name_bonus = 0.0;
                    let node_name_lower = node.name.to_ascii_lowercase();

                    if tokens.raw_query.eq_ignore_ascii_case(&node.name) {
                        name_bonus += 100.0;
                    }
                    for id in &tokens.code_identifiers {
                        if id.eq_ignore_ascii_case(&node.name)
                            || node_name_lower.contains(&id.to_ascii_lowercase())
                        {
                            name_bonus += if id.eq_ignore_ascii_case(&node.name) {
                                70.0
                            } else if id.len() >= 8 {
                                50.0
                            } else {
                                0.0
                            };
                        }
                    }
                    if tokens.expanded_terms.iter().any(|term| {
                        term.len() >= 8
                            && (node_name_lower == *term || node_name_lower.contains(term))
                    }) {
                        name_bonus += 30.0;
                    }
                    if project_tokens.contains(&node_name_lower)
                        && !tokens.raw_query.eq_ignore_ascii_case(&node.name)
                    {
                        name_bonus *= 0.2;
                    }

                    // Body fields: lexical + 词林 `contains` only. Dense 128-dim embedding on
                    // SQL/comment walls × 31万 symbols is what made Retrieval 50–150s.
                    let mut branch_comment_sim = 0.0f64;
                    let mut doc_sim = 0.0f64;
                    let mut plain_inline_sim = 0.0f64;
                    let mut sql_sim = 0.0f64;
                    let scan_body = true;
                    if scan_body {
                        if let Some(doc) = &node.docstring {
                            doc_sim = doc_sim.max(calculate_lexical_similarity(tokens, doc));
                        }
                        for c in &node.inline_comments {
                            plain_inline_sim =
                                plain_inline_sim.max(calculate_lexical_similarity(tokens, c));
                        }
                        for sc in &node.comments {
                            let sim = calculate_lexical_similarity(tokens, &sc.text);
                            match sc.scope {
                                super::graph::CommentScope::BranchInline { .. } => {
                                    branch_comment_sim = branch_comment_sim.max(sim);
                                }
                                super::graph::CommentScope::Docstring
                                | super::graph::CommentScope::MethodHeader => {
                                    doc_sim = doc_sim.max(sim);
                                }
                                super::graph::CommentScope::PropertyDoc
                                | super::graph::CommentScope::PlainInline => {
                                    plain_inline_sim = plain_inline_sim.max(sim);
                                }
                            }
                        }
                        for sql in &node.sql_predicates {
                            sql_sim =
                                sql_sim.max(calculate_lexical_similarity(tokens, &sql.raw_clause));
                            for f in &sql.target_fields {
                                sql_sim = sql_sim.max(calculate_lexical_similarity(tokens, f));
                            }
                        }
                        for lit in &node.string_literals {
                            sql_sim = sql_sim.max(calculate_lexical_similarity(tokens, lit));
                        }
                    }

                    let path_sim = path_sims.get(&node.file).copied().unwrap_or(0.0);

                    let text_match = (name_sim + name_bonus) * 0.15
                        + branch_comment_sim * 0.35
                        + sql_sim * 0.30
                        + doc_sim * 0.20
                        + plain_inline_sim * 0.10
                        + path_sim * 0.05;

                    let has_strong_anchor = branch_comment_sim >= 20.0
                        || sql_sim >= 20.0
                        || doc_sim >= 25.0
                        || name_bonus >= 30.0;

                    let genuine = bm25_scores.contains_key(&node.id)
                        || has_strong_anchor
                        || (scan_body && has_genuine_match_anchor(tokens, node));
                    let text_relevant = genuine || name_bonus >= 30.0 || text_match >= 12.0;
                    if !text_relevant {
                        return None;
                    }

                    let anchor_decay = if genuine { 1.0 } else { 0.3 };

                    let callers_cnt = graph.callers(node.id).map(|v| v.len()).unwrap_or(0);
                    let callees_cnt = graph.callees(node.id).map(|v| v.len()).unwrap_or(0);
                    // Call-graph mass is a boost for hits, not a free ticket into the catalog.
                    let graph_mass = ((callers_cnt + callees_cnt) as f64).min(12.0);

                    // 4. AST Role & Active Logic weighting
                    let kind_weight = match node.kind {
                        SymbolKind::SqlStatement => 1.50,
                        _ if node.metrics.has_sql_or_qs || branch_comment_sim >= 20.0 => 1.45,
                        SymbolKind::Enum => 1.30,
                        SymbolKind::Function
                        | SymbolKind::Method
                        | SymbolKind::Middleware
                        | SymbolKind::RouteEndpoint => {
                            if node.metrics.is_active_logic {
                                1.25
                            } else {
                                1.0
                            }
                        }
                        SymbolKind::Class
                        | SymbolKind::Struct
                        | SymbolKind::Interface
                        | SymbolKind::Trait => 0.95,
                        SymbolKind::PluginDeclaration => 1.05,
                        SymbolKind::ConfigProperty | SymbolKind::UiElement => 0.85,
                        SymbolKind::Constant | SymbolKind::Variable => 0.75,
                        _ if node.metrics.is_pure_dto => 0.45,
                        _ => 0.7,
                    };

                    let active_bonus = if node.metrics.is_active_logic {
                        let b = (node.metrics.branch_count as f64 * 6.0).min(18.0);
                        let s = if node.metrics.has_sql_or_qs {
                            15.0
                        } else {
                            0.0
                        };
                        b + s
                    } else if node.metrics.is_pure_dto && name_sim < 60.0 {
                        -10.0
                    } else {
                        0.0
                    };

                    // RRF-style BM25 fusion
                    let bm25_bonus = if bm25_max > 0.0 {
                        let norm = bm25_scores.get(&node.id).copied().unwrap_or(0.0) / bm25_max;
                        norm * 25.0
                    } else {
                        0.0
                    };

                    let concept_bonus = if !query_concept_vec.is_empty() {
                        let sim = match concept_vectors.and_then(|m| m.get(&node.id)) {
                            Some(v) => super::retrieval::concept_cosine(query_concept_vec, v),
                            None => {
                                let node_vec = super::retrieval::concept_projection(
                                    &node.name,
                                    &HashSet::new(),
                                );
                                super::retrieval::concept_cosine(query_concept_vec, &node_vec)
                            }
                        };
                        sim * 20.0
                    } else {
                        0.0
                    };

                    let raw_score = ((text_match * anchor_decay
                        + graph_mass * 1.0
                        + active_bonus
                        + bm25_bonus
                        + concept_bonus)
                        * kind_weight)
                        .max(0.0);

                    if raw_score >= 10.0 || (has_strong_anchor && raw_score >= 5.0) {
                        Some((
                            node.file.clone(),
                            ScoredSymbol {
                                node: node.clone(),
                                total_score: raw_score,
                                name_score: name_sim + name_bonus,
                                doc_score: doc_sim.max(branch_comment_sim),
                                inline_score: plain_inline_sim.max(sql_sim),
                                graph_mass,
                            },
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        };
        match score_pool.as_ref() {
            Some(p) => p.install(job),
            None => job(),
        }
    };

    // Merge parallel hits into per-file buckets.
    let mut file_map: HashMap<PathBuf, Vec<ScoredSymbol>> = HashMap::new();
    for (file, sym) in scored {
        file_map.entry(file).or_default().push(sym);
    }

    let mut candidates = Vec::new();
    for (file, mut syms) in file_map {
        syms.sort_by(|a, b| b.total_score.partial_cmp(&a.total_score).unwrap());
        let top_score = syms.first().map(|s| s.total_score).unwrap_or(0.0);
        let path_str = file.to_string_lossy().to_ascii_lowercase();
        let is_test = path_str.contains("test")
            || path_str.contains("mock")
            || path_str.contains("spec")
            || path_str.contains("fixture");
        let is_generated = path_str.contains(".g.")
            || path_str.contains(".generated.")
            || path_str.contains(".min.")
            || path_str.contains(".bundle.");
        let is_peripheral = path_str.contains("/script/")
            || path_str.contains(r"\script\")
            || path_str.contains("/scripts/")
            || path_str.contains(r"\scripts\")
            || path_str.contains("/examples/")
            || path_str.contains(r"\examples\")
            || path_str.contains("/benchmarks/")
            || path_str.contains(r"\benchmarks\")
            || path_str.contains("/docs/")
            || path_str.contains(r"\docs\");
        let is_core_src = path_str.contains("/src/")
            || path_str.contains(r"\src\")
            || path_str.contains("/packages/")
            || path_str.contains(r"\packages\")
            || path_str.contains("/crates/")
            || path_str.contains(r"\crates\")
            || path_str.contains("/lib/")
            || path_str.contains(r"\lib\");

        let adjusted_score = if is_generated {
            (top_score * 0.10).max(1.0)
        } else if is_test {
            // Heavily penalize test code so production files always rank first
            (top_score * 0.20 - 30.0).max(1.0)
        } else if is_peripheral {
            // Heavily penalize peripheral maintenance scripts and helper scripts
            (top_score * 0.30 - 15.0).max(1.0)
        } else if is_core_src {
            top_score + 35.0 // Core production source boost!
        } else {
            top_score + 15.0
        };

        candidates.push(FileCandidate {
            file,
            top_score: adjusted_score,
            symbols: syms,
        });
    }

    // 5. Enhanced Directory Cluster Co-occurrence Boost
    // When a directory (e.g. `WhereModel/`, `Strategy/`, `Core/`) contains multiple relevant files
    // or strong algorithmic classes, cluster synergy elevates all peer files together into Top 5.
    let mut dir_stats: HashMap<PathBuf, (f64, usize)> = HashMap::new();
    for cand in &candidates {
        if let Some(parent) = cand.file.parent() {
            let entry = dir_stats.entry(parent.to_path_buf()).or_insert((0.0, 0));
            if cand.top_score > entry.0 {
                entry.0 = cand.top_score;
            }
            if cand.top_score >= 35.0 {
                entry.1 += 1;
            }
        }
    }

    for cand in &mut candidates {
        let file_name_lower = cand
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_strategy_or_sql_class = file_name_lower.contains("sqlstr")
            || file_name_lower.contains("wheremodel")
            || file_name_lower.contains("strategy")
            || file_name_lower.contains("calculator")
            || file_name_lower.contains("handler");

        if is_strategy_or_sql_class && cand.top_score >= 25.0 {
            cand.top_score += 20.0; // Algorithmic strategy class boost!
        }

        if let Some(parent) = cand.file.parent() {
            if let Some(&(top, count)) = dir_stats.get(parent) {
                if top >= 45.0 {
                    // Only lift peers that already have a real ident/SQL/CJK hit.
                    // Otherwise Ailai.Order/Service rides AllPerformanceStatSqlStr
                    // and 11 unrelated *Order*Service files flood the catalog.
                    let own_anchor = cand.symbols.iter().any(|s| {
                        s.name_score >= 40.0 || s.inline_score >= 25.0 || s.doc_score >= 25.0
                    });
                    if own_anchor {
                        let boost_factor = if count >= 2 { 1.50 } else { 1.25 };
                        if cand.top_score < top {
                            cand.top_score = (cand.top_score * boost_factor).min(top * 0.98);
                        }
                    }
                }
            }
        }
    }

    // Sort production files ahead of test files and peripheral scripts
    candidates.sort_by(|a, b| {
        b.top_score
            .partial_cmp(&a.top_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Trace flow spine bidirectionally from the scored seed symbols.
///
/// Seeds come from the top-ranked files only. A 15k-file ERP scores thousands
/// of weak hits; tracing callers/callees for every one of them produced
/// 2万+ hops (then the renderer threw 99% away). Completeness still holds
/// *per seed*: every workspace-internal caller/callee within the hop budget
/// is traced (no `.take()` on edges).
/// Whether a symbol lives in a workspace file that the graph indexes. External
/// crates / std-library calls are NOT in `file_symbols`, so this filters the
/// flow spine down to in-repo business edges (an `iter → map → collect` tail
/// from the standard library would otherwise drown the real call chain).
fn is_workspace_symbol(graph: &CodeGraph, id: SymbolId) -> bool {
    graph
        .node(id)
        .map(|n| graph.file_symbols.contains_key(&n.file))
        .unwrap_or(false)
}

/// Seed high-score files so STAT↔ORDER call edges come back. Walking every
/// weak hit (9k files) was 2万 hops; 32 files was too small and lost modules.
/// Ranking itself does **not** use spine size — catalog order is `top_score`.
const MAX_SPINE_SEED_FILES: usize = 96;
const MAX_SPINE_SEEDS: usize = 384;
const MAX_SPINE_EDGES: usize = 8192;
const MAX_EDGES_PER_SEED: usize = 64;
/// Files at or above this score always seed the spine (cross-module SQL/STAT).
const SPINE_ALWAYS_SEED_SCORE: f64 = 180.0;

/// renderer may still cap the mermaid drawing, but it reports the omitted
/// count/names via the Coverage line and the omitted-edge list.
fn extract_flow_spine(
    graph: &CodeGraph,
    candidates: &[FileCandidate],
) -> (Vec<(SymbolId, Option<SymbolId>, EdgeKind)>, bool) {
    let mut high_seeds = Vec::new();
    let mut rest_seeds = Vec::new();
    for (i, fc) in candidates.iter().enumerate() {
        if i >= MAX_SPINE_SEED_FILES && fc.top_score < SPINE_ALWAYS_SEED_SCORE {
            continue;
        }
        for s in &fc.symbols {
            if s.total_score < 12.0 {
                continue;
            }
            if fc.top_score >= SPINE_ALWAYS_SEED_SCORE {
                high_seeds.push(s.node.id);
            } else {
                rest_seeds.push(s.node.id);
            }
        }
    }
    high_seeds.truncate(MAX_SPINE_SEEDS);
    let mut seeds = high_seeds;
    for id in rest_seeds {
        if seeds.len() >= MAX_SPINE_SEEDS {
            break;
        }
        if !seeds.contains(&id) {
            seeds.push(id);
        }
    }

    if seeds.is_empty() {
        return (Vec::new(), false);
    }

    let mut spine_edges = Vec::new();
    let mut visited = HashSet::new();

    for &seed in &seeds {
        if spine_edges.len() >= MAX_SPINE_EDGES {
            break;
        }
        let seed_start = spine_edges.len();
        let over_seed = |n: usize| n - seed_start >= MAX_EDGES_PER_SEED;
        visited.insert(seed);
        if let Some(callees) = graph.callees(seed) {
            for e in callees {
                if spine_edges.len() >= MAX_SPINE_EDGES || over_seed(spine_edges.len()) {
                    break;
                }
                if visited.insert(e.to) && is_workspace_symbol(graph, e.to) {
                    spine_edges.push((seed, Some(e.to), e.kind.clone()));
                    if let Some(sub_callees) = graph.callees(e.to) {
                        for sub_e in sub_callees {
                            if spine_edges.len() >= MAX_SPINE_EDGES || over_seed(spine_edges.len())
                            {
                                break;
                            }
                            if visited.insert(sub_e.to) && is_workspace_symbol(graph, sub_e.to) {
                                spine_edges.push((e.to, Some(sub_e.to), sub_e.kind.clone()));
                            }
                        }
                    }
                }
            }
        }
        if let Some(callers) = graph.callers(seed) {
            for e in callers {
                if spine_edges.len() >= MAX_SPINE_EDGES || over_seed(spine_edges.len()) {
                    break;
                }
                if visited.insert(e.to) && is_workspace_symbol(graph, e.to) {
                    spine_edges.push((e.to, Some(seed), e.kind.clone()));
                }
            }
        }
    }

    let connected = spine_edges.len() >= 2;
    (spine_edges, connected)
}

/// 目录全景分组的六类优先级:锚定 > 子树 > 父链 > 兄弟 > 图连通 > 路径词命中。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DirGroup {
    Anchor,
    Subtree,
    ParentChain,
    Sibling,
    GraphLinked,
    PathHit,
}

impl DirGroup {
    fn label(self) -> &'static str {
        match self {
            DirGroup::Anchor => "① 锚定目录(有锚定符号的文件)",
            DirGroup::Subtree => "② 子树(锚定目录的更深子目录)",
            DirGroup::ParentChain => "③ 父链(锚定目录的祖先目录)",
            DirGroup::Sibling => "④ 兄弟目录(与锚定目录同级未命中)",
            DirGroup::GraphLinked => "⑤ 图连通目录(锚定符号的 callee/caller 所在)",
            DirGroup::PathHit => "⑥ 路径词命中目录(路径含查询词/词林词)",
        }
    }
}

/// 一个目录候选:路径 + 分组 + 分数 + 统计 + 可执行 grep 关键词。
#[derive(Debug, Clone)]
struct DirCandidate {
    path: PathBuf,
    group: DirGroup,
    /// 目录分 0..100,可解释(锚定类 = ratio/peak/diversity;弱类 = 结构兜底)。
    score: f64,
    /// 目录内锚定文件数。
    anchored_files: usize,
    /// 目录内索引文件总数。
    total_files: usize,
    /// 目录内最高文件分。
    peak_file_score: f64,
    /// 目录内命中文件(路径 + 分数)。
    hits: Vec<(PathBuf, f64)>,
    /// 自动派生的 grep 关键词(查询词 + 词林扩展 + 目录内锚定符号名)。
    grep_terms: Vec<String>,
}

/// Collect a FULL six-group directory panorama around the top-ranked hits:
/// anchored dirs, their subtrees, parent chains, siblings, graph-linked dirs,
/// and path-term-hit dirs. Groups are mutually exclusive (a dir joins the
/// highest-priority group it qualifies for) and the union covers every
/// query-related directory — nothing related is dropped, and every entry
/// carries a scorable ranking plus grep keywords for fallback search.
fn collect_directory_panorama(
    graph: &CodeGraph,
    top_files: &[FileCandidate],
    tokens: &SearchTokens,
    scope: Option<&Path>,
    dirindex: Option<&super::retrieval::DirIndex>,
) -> Vec<DirCandidate> {
    // 1. 索引目录集合(目录 -> 文件数),尊重 scope。
    //    dirindex 直查:sidecar 预计算目录统计,免全树扫描;缺失则回退遍历。
    let mut dir_files: HashMap<PathBuf, usize> = HashMap::new();
    if let Some(di) = dirindex {
        for key in di.all_dirs() {
            let dir = super::retrieval::DirIndex::key_to_path(key);
            if let Some(sc) = scope {
                if !path_matches_scope(&dir, sc) {
                    continue;
                }
            }
            if let Some(e) = di.entry(&dir) {
                dir_files.insert(dir, e.file_count);
            }
        }
    } else {
        for file in graph.file_symbols.keys() {
            if let Some(sc) = scope {
                if !path_matches_scope(file, sc) {
                    continue;
                }
            }
            if let Some(dir) = file.parent() {
                *dir_files.entry(dir.to_path_buf()).or_insert(0) += 1;
            }
        }
    }
    if dir_files.is_empty() {
        return Vec::new();
    }
    let indexed_dirs: HashSet<PathBuf> = dir_files.keys().cloned().collect();

    // 2. 锚定目录:候选文件所在目录 -> (锚定文件数, 峰值分, 命中文件)。
    let mut anchor_dirs: HashMap<PathBuf, (usize, f64, Vec<(PathBuf, f64)>)> = HashMap::new();
    for fc in top_files {
        let Some(dir) = fc.file.parent() else {
            continue;
        };
        let e = anchor_dirs.entry(dir.to_path_buf()).or_default();
        e.0 += 1;
        if fc.top_score > e.1 {
            e.1 = fc.top_score;
        }
        e.2.push((fc.file.clone(), fc.top_score));
    }
    let anchor_set: HashSet<PathBuf> = anchor_dirs.keys().cloned().collect();

    // 3. 归属:每个索引目录进入优先级最高的分组(互斥,不重复)。
    let mut group_of: HashMap<PathBuf, DirGroup> = HashMap::new();
    for d in &anchor_set {
        group_of.entry(d.clone()).or_insert(DirGroup::Anchor);
    }
    // 子树 / 父链:与锚定目录的包含关系(向下 / 向上)。
    for d in &indexed_dirs {
        if group_of.contains_key(d) {
            continue;
        }
        let mut is_subtree = false;
        let mut is_parent_chain = false;
        for a in &anchor_set {
            if d.starts_with(a) && d != a {
                is_subtree = true;
                break;
            }
            if a.starts_with(d) && a != d {
                is_parent_chain = true;
                break;
            }
        }
        if is_subtree {
            group_of.entry(d.clone()).or_insert(DirGroup::Subtree);
        } else if is_parent_chain {
            group_of.entry(d.clone()).or_insert(DirGroup::ParentChain);
        }
    }
    // 兄弟:与锚定目录同父的其它目录。
    for d in &indexed_dirs {
        if group_of.contains_key(d) {
            continue;
        }
        let parent_dir = d.parent();
        let has_anchor_sibling = anchor_set.iter().any(|a| match (a.parent(), parent_dir) {
            (Some(ap), Some(pd)) => ap == pd,
            _ => false,
        });
        if has_anchor_sibling {
            group_of.entry(d.clone()).or_insert(DirGroup::Sibling);
        }
    }
    // 图连通:锚定符号的 callee/caller 所在目录。
    {
        let mut linked: HashSet<PathBuf> = HashSet::new();
        for fc in top_files {
            for s in &fc.symbols {
                if let Some(callees) = graph.callees(s.node.id) {
                    for e in callees {
                        if let Some(n) = graph.node(e.to) {
                            if let Some(dir) = n.file.parent() {
                                linked.insert(dir.to_path_buf());
                            }
                        }
                    }
                }
                if let Some(callers) = graph.callers(s.node.id) {
                    for e in callers {
                        if let Some(n) = graph.node(e.to) {
                            if let Some(dir) = n.file.parent() {
                                linked.insert(dir.to_path_buf());
                            }
                        }
                    }
                }
            }
        }
        for d in linked {
            group_of.entry(d).or_insert(DirGroup::GraphLinked);
        }
    }
    // 路径词命中:目录路径含查询词/词林扩展词/标识符。
    {
        let mut terms: Vec<String> = Vec::new();
        for w in &tokens.words {
            if w.len() >= 3 {
                terms.push(w.clone());
            }
        }
        terms.extend(tokens.expanded_terms.iter().cloned());
        terms.extend(tokens.code_identifiers.iter().cloned());
        if !terms.is_empty() {
            for d in &indexed_dirs {
                if group_of.contains_key(d) {
                    continue;
                }
                let p = normalize_path_for_match(d);
                if terms.iter().any(|t| p.contains(&t.to_ascii_lowercase())) {
                    group_of.entry(d.clone()).or_insert(DirGroup::PathHit);
                }
            }
        }
    }

    // 4. 计算每个目录的分数与 grep 关键词。
    let mut out: Vec<DirCandidate> = Vec::new();
    for (dir, group) in group_of {
        let total = dir_files.get(&dir).copied().unwrap_or(0);
        let (anchored, peak, hits) = anchor_dirs
            .get(&dir)
            .cloned()
            .unwrap_or((0, 0.0, Vec::new()));

        // grep 词只含与本次查询相关的词(防污染:目录内无关符号名不得混入)。
        let mut grep_terms: Vec<String> = Vec::new();
        // 1. 查询标识符(最精确)。
        for id in &tokens.code_identifiers {
            if !grep_terms.contains(id) {
                grep_terms.push(id.clone());
            }
        }
        // 2. 查询单词(≥3 字母,切词时已滤停用词)。
        for w in &tokens.words {
            if w.len() >= 3 && !grep_terms.contains(w) {
                grep_terms.push(w.clone());
            }
        }
        // 3. 词林扩展词(≥3,避免单字母噪声)。
        for t in &tokens.expanded_terms {
            if t.len() >= 3 && !grep_terms.contains(t) {
                grep_terms.push(t.clone());
            }
        }
        // 4. 锚定目录:仅纳入与查询词/扩展词/标识符有词法关联的同族符号名
        //    (名称包含查询词,或查询词包含该名称 —— 无关符号名一律排除)。
        if anchor_set.contains(&dir) {
            let query_terms: Vec<String> = tokens
                .words
                .iter()
                .chain(tokens.code_identifiers.iter())
                .chain(tokens.expanded_terms.iter())
                .map(|s| s.to_ascii_lowercase())
                .collect();
            for id in symbols_in_dir(graph, &dir) {
                if let Some(n) = graph.node(*id) {
                    let name_lower = n.name.to_ascii_lowercase();
                    let related = query_terms.iter().any(|t| {
                        t.len() >= 3 && (name_lower.contains(t) || t.contains(&name_lower))
                    });
                    if related && n.name.len() >= 3 && !grep_terms.contains(&n.name) {
                        grep_terms.push(n.name.clone());
                        if grep_terms.len() >= 6 {
                            break;
                        }
                    }
                }
            }
        }
        grep_terms.truncate(6);

        let score = match group {
            DirGroup::Anchor | DirGroup::GraphLinked => {
                let ratio = if total > 0 {
                    anchored as f64 / total as f64
                } else {
                    0.0
                };
                let peak_norm = (peak / 80.0).min(1.0);
                let div_norm = directory_term_diversity(tokens, graph, &dir);
                (0.60 * ratio + 0.25 * peak_norm + 0.15 * div_norm) * 100.0
            }
            _ => weak_dir_score(&dir, &anchor_set, tokens, graph),
        };

        out.push(DirCandidate {
            path: dir,
            group,
            score,
            anchored_files: anchored,
            total_files: total,
            peak_file_score: peak,
            hits,
            grep_terms,
        });
    }

    // 5. 组间固定顺序(①→⑥)+ 组内按分数降序。
    out.sort_by(|a, b| {
        a.group.cmp(&b.group).then_with(|| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    out
}

/// 目录下所有符号 id(file_symbols 的 key 是文件,目录需遍历)。
fn symbols_in_dir<'g>(graph: &'g CodeGraph, dir: &Path) -> Vec<&'g SymbolId> {
    let mut out: Vec<&SymbolId> = Vec::new();
    for (f, ids) in &graph.file_symbols {
        if f.parent() == Some(dir) {
            out.extend(ids.iter());
        }
    }
    out
}

/// 目录内锚定符号命中的不同查询词种类(归一 0..1,上限 5 种)。
fn directory_term_diversity(tokens: &SearchTokens, graph: &CodeGraph, dir: &Path) -> f64 {
    let query_terms: Vec<String> = tokens
        .words
        .iter()
        .chain(tokens.expanded_terms.iter())
        .chain(tokens.code_identifiers.iter())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if query_terms.is_empty() {
        return 0.0;
    }
    let mut hit: HashSet<String> = HashSet::new();
    for id in symbols_in_dir(graph, dir) {
        if let Some(n) = graph.node(*id) {
            let name = n.name.to_ascii_lowercase();
            for t in &query_terms {
                if name.contains(t) {
                    hit.insert(t.clone());
                }
            }
        }
    }
    (hit.len() as f64 / 5.0).min(1.0)
}

/// 弱类目录分:父链近距 + 路径词法 + 兄弟锚定比 + 活跃度,保证可排序。
fn weak_dir_score(
    dir: &Path,
    anchor_set: &HashSet<PathBuf>,
    tokens: &SearchTokens,
    graph: &CodeGraph,
) -> f64 {
    // 父链近距:离最近锚定目录的层级距离(1 级 = 1.0,2 级 = 0.5,……)。
    let mut proximity: f64 = 0.0;
    for a in anchor_set {
        let mut d = dir.to_path_buf();
        let mut depth = 1.0;
        loop {
            if d == *a {
                proximity = proximity.max(1.0 / depth);
                break;
            }
            match d.parent() {
                Some(p) if !p.as_os_str().is_empty() => {
                    d = p.to_path_buf();
                    depth += 1.0;
                }
                _ => break,
            }
        }
    }

    // 路径词法:目录路径含查询词/词林扩展词。
    let p = normalize_path_for_match(dir);
    let mut path_sim = 0.0;
    for w in &tokens.words {
        if w.len() >= 3 && p.contains(&w.to_ascii_lowercase()) {
            path_sim = 1.0;
            break;
        }
    }
    if path_sim == 0.0 {
        for t in &tokens.expanded_terms {
            if p.contains(&t.to_ascii_lowercase()) {
                path_sim = 1.0;
                break;
            }
        }
    }

    // 兄弟锚定比:同级目录中被锚定的比例。
    let parent_dir = dir.parent();
    let mut siblings_total = 0usize;
    let mut siblings_anchored = 0usize;
    for a in anchor_set {
        if let (Some(ap), Some(pd)) = (a.parent(), parent_dir) {
            if ap == pd {
                siblings_total += 1;
                siblings_anchored += 1;
            }
        }
    }
    let sib_ratio = if siblings_total > 0 {
        siblings_anchored as f64 / siblings_total as f64
    } else {
        0.0
    };

    // 活跃度:目录在索引中是否含符号文件。
    let active = if symbols_in_dir(graph, dir).is_empty() {
        0.8
    } else {
        1.0
    };

    (0.30 * proximity + 0.25 * path_sim + 0.25 * sib_ratio + 0.20 * active) * 100.0
}

/// Per-symbol match signal, so the candidate table marks EACH symbol (a file
/// with one exact hit and three semantic hits shows all four) instead of a
/// single file-level label derived from only the top symbol.
fn symbol_match_signal(s: &ScoredSymbol) -> &'static str {
    if s.name_score > 40.0 {
        "🎯 精确符号/标识符"
    } else if s.doc_score > 30.0 {
        "📝 注释/文档"
    } else if s.inline_score > 30.0 {
        "🔍 内部代码逻辑"
    } else {
        "🌐 语义/拓扑相关"
    }
}

/// Architectural layer of a hit — used to buy one evidence span per layer so a
/// Controller/Service/Mapper/XML (or route/handler/repo/SQL, or page/api/store)
/// panorama survives a tight token budget. Language-agnostic: path + kind + name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum CodeLayer {
    Http,
    Ui,
    Service,
    Impl,
    Data,
    Sql,
    Config,
    Doc,
    Core,
}

impl CodeLayer {
    fn label(self) -> &'static str {
        match self {
            CodeLayer::Http => "HTTP",
            CodeLayer::Ui => "UI",
            CodeLayer::Service => "SVC",
            CodeLayer::Impl => "IMPL",
            CodeLayer::Data => "DATA",
            CodeLayer::Sql => "SQL",
            CodeLayer::Config => "CFG",
            CodeLayer::Doc => "DOC",
            CodeLayer::Core => "CORE",
        }
    }

    fn is_primary(self) -> bool {
        !matches!(self, CodeLayer::Config | CodeLayer::Doc)
    }
}

fn path_slash(p: &Path) -> String {
    normalize_path_for_match(p).replace('\\', "/")
}

fn file_ext_lower(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn classify_layer(file: &Path, sym: &ScoredSymbol) -> CodeLayer {
    let ext = file_ext_lower(file);
    match sym.node.kind {
        SymbolKind::RouteEndpoint | SymbolKind::Middleware => return CodeLayer::Http,
        SymbolKind::SqlStatement => return CodeLayer::Sql,
        SymbolKind::ConfigProperty | SymbolKind::PluginDeclaration => return CodeLayer::Config,
        SymbolKind::UiElement => return CodeLayer::Ui,
        SymbolKind::Module if matches!(ext.as_str(), "md" | "mdx" | "rst" | "adoc") => {
            return CodeLayer::Doc;
        }
        _ => {}
    }
    match ext.as_str() {
        "md" | "mdx" | "rst" | "adoc" | "markdown" => return CodeLayer::Doc,
        "yml" | "yaml" | "toml" | "ini" | "properties" | "env" => return CodeLayer::Config,
        "sql" => return CodeLayer::Sql,
        "vue" | "svelte" | "astro" | "css" | "scss" | "less" | "sass" => return CodeLayer::Ui,
        "xml" => {
            let p = path_slash(file);
            if p.contains("mapper") || p.contains("/resources/") || p.contains("mybatis") {
                return CodeLayer::Sql;
            }
        }
        "json" => {
            let p = path_slash(file);
            if p.contains("config") || p.contains("/locales/") {
                return CodeLayer::Config;
            }
        }
        _ => {}
    }

    let p = path_slash(file);
    let name = sym.node.name.to_ascii_lowercase();
    let base = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let is_tsx = matches!(ext.as_str(), "tsx" | "jsx");
    if is_tsx
        && (p.contains("/components/")
            || p.contains("/pages/")
            || p.contains("/views/")
            || p.contains("/frontend/")
            || p.contains("/webview/")
            || p.contains("/ui/")
            || p.contains("/app/"))
        && !p.contains("/api/")
        && !p.contains("/server/")
    {
        return CodeLayer::Ui;
    }

    if p.contains("/controller")
        || p.contains("/controllers")
        || p.contains("/handler")
        || p.contains("/handlers")
        || p.contains("/routes/")
        || p.contains("/router")
        || p.contains("/endpoint")
        || p.contains("/servlet")
        || p.contains("/pages/api/")
        || p.contains("/app/api/")
        || name.contains("controller")
        || name.ends_with("handler")
        || name.ends_with("servlet")
        || base.ends_with("controller")
        || base.ends_with("handler")
        || base.ends_with("_routes")
        || base == "urls"
        || base == "views"
    {
        return CodeLayer::Http;
    }
    if p.contains("/impl/")
        || p.contains("\\impl\\")
        || name.ends_with("impl")
        || base.ends_with("impl")
        || base.ends_with("_impl")
    {
        return CodeLayer::Impl;
    }
    if p.contains("/mapper")
        || p.contains("/repository")
        || p.contains("/repositories")
        || p.contains("/dao/")
        || p.contains("/store/")
        || p.contains("/stores/")
        || name.contains("mapper")
        || name.ends_with("repository")
        || name.ends_with("dao")
        || base.ends_with("mapper")
        || base.ends_with("repository")
        || base.ends_with("_repo")
        || base.ends_with("_dao")
    {
        return CodeLayer::Data;
    }
    if p.contains("/service")
        || p.contains("/services")
        || p.contains("/usecase")
        || p.contains("/use-case")
        || p.contains("/application/")
        || name.ends_with("service")
        || name.ends_with("usecase")
        || base.ends_with("service")
        || base.ends_with("_service")
    {
        return CodeLayer::Service;
    }
    if p.contains("/frontend/")
        || p.contains("/webview")
        || p.contains("/components/")
        || p.contains("/pages/")
        || p.contains("/views/")
    {
        return CodeLayer::Ui;
    }
    CodeLayer::Core
}

/// Std / collection / formatter names that drown a business spine (iter→map→collect).
fn is_spine_noise(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n == "unknown" || n.is_empty() {
        return false;
    }
    if n.len() <= 2 {
        return true;
    }
    matches!(
        n.as_str(),
        "new"
            | "default"
            | "clone"
            | "drop"
            | "fmt"
            | "from"
            | "into"
            | "ok"
            | "err"
            | "unwrap"
            | "expect"
            | "len"
            | "is_empty"
            | "push"
            | "push_str"
            | "insert"
            | "get"
            | "set"
            | "iter"
            | "into_iter"
            | "map"
            | "filter"
            | "collect"
            | "count"
            | "take"
            | "skip"
            | "next"
            | "nth"
            | "as_str"
            | "as_ref"
            | "to_string"
            | "to_owned"
            | "contains"
            | "starts_with"
            | "ends_with"
            | "split"
            | "join"
            | "format"
            | "write"
            | "writeln"
            | "lock"
            | "read"
            | "and_then"
            | "or_else"
            | "unwrap_or"
            | "unwrap_or_else"
            | "unwrap_or_default"
            | "min"
            | "max"
            | "cmp"
            | "eq"
            | "partial_cmp"
            | "bytes"
            | "chars"
            | "lines"
            | "trim"
            | "parse"
            | "encode"
            | "decode"
            | "execute"
            | "call"
            | "apply"
            | "update"
            | "build"
            | "create"
            | "init"
    )
}

fn rel_disp(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn top_seg(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn same_repo_as_hits(dir: &Path, hit_files: &[&Path], root: &Path) -> bool {
    let d = top_seg(dir, root);
    if d.is_empty() {
        return true;
    }
    hit_files.iter().any(|f| top_seg(f, root) == d)
}

struct EvidencePick<'a> {
    fc: &'a FileCandidate,
    sym: &'a ScoredSymbol,
    layer: CodeLayer,
}

/// Layer-diverse auction: fill primary layers first (HTTP/UI/SVC/IMPL/DATA/SQL),
/// then highest remaining scores. Config/Doc stay catalog-only unless nothing else
/// is available. Runs over ALL candidates (not just max_files) so a 10th-place
/// exact hit can beat nine noisy `render` files.
fn auction_evidence<'a>(
    candidates: &'a [FileCandidate],
    max_spans: usize,
) -> Vec<EvidencePick<'a>> {
    struct Item<'a> {
        fc: &'a FileCandidate,
        sym: &'a ScoredSymbol,
        layer: CodeLayer,
        score: f64,
    }
    let mut items: Vec<Item<'a>> = Vec::new();
    for fc in candidates {
        let mut added = 0usize;
        for s in &fc.symbols {
            if s.total_score >= 15.0 || s.name_score >= 25.0 || added == 0 {
                items.push(Item {
                    layer: classify_layer(&fc.file, s),
                    score: s.total_score,
                    fc,
                    sym: s,
                });
                added += 1;
            }
            if added >= 4 {
                break;
            }
        }
    }
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut picked: Vec<EvidencePick<'a>> = Vec::new();
    let mut used_layers: HashSet<CodeLayer> = HashSet::new();
    let has_primary = items.iter().any(|i| i.layer.is_primary());

    let already = |picked: &[EvidencePick<'a>], it: &Item<'a>| {
        picked
            .iter()
            .any(|p| p.fc.file == it.fc.file && p.sym.node.id == it.sym.node.id)
    };

    for it in &items {
        if picked.len() >= max_spans {
            break;
        }
        if !it.layer.is_primary() {
            continue;
        }
        if used_layers.contains(&it.layer) {
            continue;
        }
        picked.push(EvidencePick {
            fc: it.fc,
            sym: it.sym,
            layer: it.layer,
        });
        used_layers.insert(it.layer);
    }
    for it in &items {
        if picked.len() >= max_spans {
            break;
        }
        if already(&picked, it) {
            continue;
        }
        if !it.layer.is_primary() && has_primary && picked.len() < 3 {
            continue;
        }
        picked.push(EvidencePick {
            fc: it.fc,
            sym: it.sym,
            layer: it.layer,
        });
    }
    picked
}

fn grep_query_terms(tokens: &SearchTokens) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for id in &tokens.code_identifiers {
        if id.len() >= 3 && !terms.contains(id) {
            terms.push(id.clone());
        }
    }
    for w in &tokens.words {
        if w.len() >= 4 && !terms.contains(w) {
            terms.push(w.clone());
        }
    }
    terms.truncate(6);
    terms
}

fn fold_snippet(
    lines: &[&str],
    start_line: usize,
    end_line: usize,
    rel_path: &str,
) -> (Vec<String>, bool, usize) {
    let total_span = end_line.saturating_sub(start_line) + 1;
    let mut snippet = Vec::new();
    if total_span > FOLD_AFTER_LINES {
        let head_end = (start_line + FOLD_HEAD_LINES).min(lines.len());
        for l in start_line..=head_end {
            if l.saturating_sub(1) < lines.len() {
                snippet.push(format!("{l}\t{}", lines[l - 1]));
            }
        }
        let tail_start = end_line.saturating_sub(FOLD_TAIL_LINES).max(head_end + 1);
        let folded_count = tail_start.saturating_sub(head_end + 1);
        if folded_count > 0 {
            snippet.push(format!(
                "...\t// ... [{folded_count} lines folded; use read_file(\"{rel_path}\", start_line={}, end_line={}) to view full body] ...",
                head_end + 1,
                tail_start.saturating_sub(1)
            ));
        }
        for l in tail_start..=end_line {
            if l.saturating_sub(1) < lines.len() {
                snippet.push(format!("{l}\t{}", lines[l - 1]));
            }
        }
        (snippet, true, folded_count)
    } else {
        for l in start_line..=end_line {
            if l.saturating_sub(1) < lines.len() {
                snippet.push(format!("{l}\t{}", lines[l - 1]));
            }
        }
        (snippet, false, 0)
    }
}

/// Render formatted explore output: catalog is exhaustive and cheap; evidence is
/// layer-diverse and hard-capped. Omissions are ready-to-fire tool calls.
fn render_explore_output(
    graph: &CodeGraph,
    root: &Path,
    query: &str,
    candidates: &[FileCandidate],
    flow_spine: &[(SymbolId, Option<SymbolId>, EdgeKind)],
    connected: bool,
    max_files: usize,
    scope: Option<&Path>,
    tokens: &SearchTokens,
    dirindex: Option<&super::retrieval::DirIndex>,
    cache_status: Option<super::index::RefreshStats>,
    cost_breakdown: Option<(Duration, Duration, Duration, Duration)>,
) -> String {
    let mut out = Vec::new();
    let top_files = &candidates[..candidates.len().min(max_files)];
    let evidence = auction_evidence(candidates, MAX_EVIDENCE_SPANS);
    let evidence_file_set: HashSet<&Path> = evidence.iter().map(|e| e.fc.file.as_path()).collect();
    let hit_paths: Vec<&Path> = top_files.iter().map(|fc| fc.file.as_path()).collect();

    if let Some((total, index_t, ret_t, ren_t)) = cost_breakdown {
        out.push(format!(
            "> ⚡ **Performance**: ⏱️ **Cost Time**: {}ms (Index: {}ms | Retrieval: {}ms | Render: {}ms)\n",
            total.as_millis(),
            index_t.as_millis(),
            ret_t.as_millis(),
            ren_t.as_millis()
        ));
    }

    if let Some(stats) = cache_status {
        out.push(index_status_line(&stats));
    } else {
        out.push("> ⚡ **Index Status**: [Cache HIT] 内存索引已就绪\n".to_string());
    }

    let has_contract = top_files.iter().take(4).any(|fc| {
        fc.symbols.iter().take(4).any(|s| {
            matches!(
                s.node.kind,
                SymbolKind::Trait | SymbolKind::Interface | SymbolKind::TypeAlias
            )
        })
    });
    let low_hit = !candidates.is_empty() && candidates.len() < 3;
    let grep_terms = grep_query_terms(tokens);
    let grep_pat = if grep_terms.is_empty() {
        query.split_whitespace().next().unwrap_or(query).to_string()
    } else {
        grep_terms.join("|")
    };
    let scope_disp = scope
        .map(|sc| rel_disp(sc, root))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".".to_string());

    let workspace_files = graph.file_symbols.len();
    let workspace_syms = graph.nodes.len();
    let (scope_files, scope_syms) = match scope {
        Some(sc) => {
            let mut scope_file_set: HashSet<&PathBuf> = HashSet::new();
            let mut syms = 0usize;
            for node in graph.nodes.values() {
                if path_matches_scope(&node.file, sc) {
                    scope_file_set.insert(&node.file);
                    syms += 1;
                }
            }
            (scope_file_set.len(), syms)
        }
        None => (workspace_files, workspace_syms),
    };
    let remaining = candidates.len().saturating_sub(
        evidence_file_set
            .len()
            .max(top_files.len().min(candidates.len())),
    );
    let omitted_symbols: usize = top_files
        .iter()
        .map(|fc| fc.symbols.len().saturating_sub(4))
        .sum();

    let mut business_hops: Vec<(String, String, &'static str)> = Vec::new();
    for (from, to, kind) in flow_spine {
        let Some(target) = to else { continue };
        let from_name = graph
            .node(*from)
            .map(|n| n.name.as_str())
            .unwrap_or("unknown");
        let to_name = graph
            .node(*target)
            .map(|n| n.name.as_str())
            .unwrap_or("unknown");
        if is_spine_noise(from_name) || is_spine_noise(to_name) {
            continue;
        }
        let label = match kind {
            EdgeKind::Calls => "calls",
            EdgeKind::HttpDispatches => "http",
            EdgeKind::MapperBinds => "sql",
            EdgeKind::ConfigBinds => "cfg",
            _ => "refs",
        };
        business_hops.push((from_name.to_string(), to_name.to_string(), label));
        if business_hops.len() >= MAX_SPINE_HOPS {
            break;
        }
    }
    let spine_shown = business_hops.len();
    let spine_total = flow_spine.len();

    let mut warn: Vec<String> = Vec::new();
    warn.push("⛔ DO NOT conclude absence. Ranked ≠ exhaustive.".to_string());
    if let Some(sc) = scope {
        let scope_disp_warn = rel_disp(sc, root);
        let outside_syms = workspace_syms.saturating_sub(scope_syms);
        warn.push(format!(
            "🔭 scope `{scope_disp_warn}`: in-scope {scope_files} of {workspace_files} indexed files · {scope_syms}/{workspace_syms} symbols — {outside_syms} OUTSIDE not ranked (SIBLING layers)"
        ));
    }
    if has_contract {
        warn.push("🧩 Contract hit (trait/interface/type-alias) = DECLARATION, not behavior. Query `impl <name>` / grep `impl <name>`.".to_string());
    }
    if low_hit {
        warn.push("🎯 Low hit count (<3 files): WEAK signal. Retry with synonyms / English / wider path before judging.".to_string());
    }

    let mut next: Vec<String> = Vec::new();
    let mut n = 1usize;
    if let Some(first) = evidence.first() {
        let rel = rel_disp(&first.fc.file, root);
        let parent = first
            .fc
            .file
            .parent()
            .map(|p| rel_disp(p, root))
            .unwrap_or_else(|| scope_disp.clone());
        next.push(format!(
            "{n}. code_explore  path={parent}  query={}",
            first.sym.node.name
        ));
        n += 1;
        if first
            .sym
            .node
            .end_line
            .saturating_sub(first.sym.node.start_line)
            + 1
            > FOLD_AFTER_LINES
        {
            next.push(format!(
                "{n}. read_file  {rel}  offset={}  (omit limit; call again with the footer offset to finish the file)",
                first.sym.node.start_line
            ));
            n += 1;
        }
    } else if !candidates.is_empty() {
        next.push(format!(
            "{n}. code_explore  path={scope_disp}  query={query}  (retry synonyms / broader path — CATALOG is not absence)"
        ));
        n += 1;
    }
    if has_contract {
        if let Some(name) = top_files.iter().find_map(|fc| {
            fc.symbols.iter().find_map(|s| {
                matches!(
                    s.node.kind,
                    SymbolKind::Trait | SymbolKind::Interface | SymbolKind::TypeAlias
                )
                .then_some(s.node.name.as_str())
            })
        }) {
            next.push(format!(
                "{n}. code_explore  path={scope_disp}  query=impl {name}"
            ));
            n += 1;
        }
    }
    let _ = n;

    let mut card = Vec::new();
    card.push(format!(
        "> 📊 **Coverage** evidence {}/{} files · catalog {}/{} · omitted-sym {omitted_symbols} · spine {spine_shown}/{spine_total} hops",
        evidence_file_set.len(),
        candidates.len(),
        candidates.len().min(max_files.saturating_add(MAX_CATALOG_REMAINING)),
        candidates.len(),
    ));
    if let Some(sc) = scope {
        let sd = rel_disp(sc, root);
        card.push(format!(
            ">    scope `{sd}`: in-scope {scope_files} of {workspace_files} indexed files · {scope_syms}/{workspace_syms} symbols"
        ));
    } else {
        card.push(format!(
            ">    workspace: {workspace_files} indexed files · {workspace_syms} symbols"
        ));
    }
    for w in &warn {
        card.push(format!("> {w}"));
    }
    card.push("> **NEXT** (copy as tool calls, in order):".to_string());
    if next.is_empty() {
        card.push(format!(
            "> 1. code_explore  path={scope_disp}  query={query}  (retry synonyms / English / broader path)"
        ));
        card.push(format!(
            "> 2. grep  pattern={grep_pat}  path={scope_disp}  (literals only)"
        ));
    } else {
        for step in &next {
            card.push(format!("> {step}"));
        }
    }
    let coverage_idx = out.len();
    out.extend(card);
    out.push("".to_string());

    out.push(format!("### 🔗 LAYERS: \"{query}\""));
    if connected && !business_hops.is_empty() {
        for (from, to, label) in &business_hops {
            out.push(format!("  {from} --{label}--> {to}"));
        }
        if spine_total > spine_shown {
            out.push(format!(
                "  … {omitted} more edges omitted (noise filtered: iter/map/collect/len/new)",
                omitted = spine_total.saturating_sub(spine_shown)
            ));
        }
    } else {
        out.push("> ℹ️ No multi-hop business flow. Ranked symbols below.".to_string());
    }
    if !evidence.is_empty() {
        let mut seen_l: HashSet<CodeLayer> = HashSet::new();
        for e in &evidence {
            if seen_l.insert(e.layer) {
                out.push(format!(
                    "  {:<4}  {}  `{}`:L{}",
                    e.layer.label(),
                    rel_disp(&e.fc.file, root),
                    e.sym.node.name,
                    e.sym.node.start_line
                ));
            }
        }
    }
    out.push("".to_string());

    out.push("#### 📋 Matched Symbol Candidates:".to_string());
    out.push("| Score | File | Layer | Symbols & Lines |".to_string());
    out.push("| :--- | :--- | :--- | :--- |".to_string());
    for fc in top_files {
        let rel_path = rel_disp(&fc.file, root);
        let layer = fc
            .symbols
            .first()
            .map(|s| classify_layer(&fc.file, s).label())
            .unwrap_or("CORE");
        let sym_summary: Vec<String> = fc
            .symbols
            .iter()
            .take(4)
            .map(|s| {
                format!(
                    "`{}`:L{} {}",
                    s.node.name,
                    s.node.start_line,
                    symbol_match_signal(s)
                )
            })
            .collect();
        out.push(format!(
            "| **{:.1}** | `{rel_path}` | {layer} | {} |",
            fc.top_score,
            sym_summary.join(", ")
        ));
    }
    out.push("".to_string());

    let mut folded_spans: usize = 0;
    let mut folded_lines: usize = 0;
    let mut evidence_chars: usize = 0;
    let evidence_files: Vec<PathBuf> = {
        let mut seen = HashSet::new();
        evidence
            .iter()
            .filter_map(|e| {
                if seen.insert(&e.fc.file) {
                    Some(e.fc.file.clone())
                } else {
                    None
                }
            })
            .collect()
    };
    let evidence_text: HashMap<PathBuf, String> = evidence_files
        .into_par_iter()
        .filter_map(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .map(|c| (p, super::strip_utf8_bom(&c).to_string()))
        })
        .collect();
    let mut session_spans = SESSION_SENT_SPANS.write().unwrap();

    out.push("### EVIDENCE (budgeted, layer-diverse)".to_string());
    for e in &evidence {
        if evidence_chars >= EVIDENCE_BUDGET_CHARS {
            let rel = rel_disp(&e.fc.file, root);
            out.push(format!(
                "> budget full — skipped `{}` in `{rel}`. NEXT: read_file  {rel}  offset={}",
                e.sym.node.name, e.sym.node.start_line
            ));
            continue;
        }
        let rel_path = rel_disp(&e.fc.file, root);
        out.push(format!(
            "**{}** `{rel_path}` — `{}`:L{}",
            e.layer.label(),
            e.sym.node.name,
            e.sym.node.start_line
        ));
        let capsule = graph.file_capsule(&e.fc.file);
        if !capsule.is_empty() {
            out.push("> 📋 **File Capability Capsule**:".to_string());
            for cap_line in capsule.iter().take(3) {
                out.push(format!("> - {cap_line}"));
            }
            out.push("".to_string());
        }
        if let Some(content) = evidence_text.get(&e.fc.file) {
            let lines: Vec<&str> = content.lines().collect();
            let start = e.sym.node.start_line.saturating_sub(2).max(1);
            let end = (e.sym.node.end_line + 3).min(lines.len());
            let sent_list = session_spans
                .entry(format!("{}|{rel_path}", root.display()))
                .or_default();
            let already_sent = sent_list.iter().any(|(s, en)| *s <= start && end <= *en);
            if already_sent {
                out.push(format!(
                    "> `[Same range already returned earlier this session (dedup): {rel_path} L{start}-L{end} (`{}`) — use read_file  {rel_path}  offset={start}]`\n",
                    e.sym.node.name
                ));
            } else {
                sent_list.push((start, end));
                let (snippet, folded, folded_count) = fold_snippet(&lines, start, end, &rel_path);
                if folded {
                    folded_spans += 1;
                    folded_lines += folded_count;
                }
                let body = snippet.join("\n");
                evidence_chars += body.len();
                let ext = e.fc.file.extension().and_then(|x| x.to_str()).unwrap_or("");
                out.push(format!(
                    "// Symbols: {}:L{}\n```{ext}\n{body}\n```\n",
                    e.sym.node.name, e.sym.node.start_line
                ));
            }
        }
    }

    let catalog_rest: Vec<&FileCandidate> = candidates
        .iter()
        .filter(|c| !evidence_file_set.contains(c.file.as_path()))
        .collect();
    if !catalog_rest.is_empty() {
        out.push("\n**CATALOG (not rendered — `read_file` a listed FILE; `code_explore` the PARENT DIRECTORY with the symbol as query, never the file path):**".to_string());
        for (i, c) in catalog_rest.iter().take(MAX_CATALOG_REMAINING).enumerate() {
            let rel = rel_disp(&c.file, root);
            let layer = c
                .symbols
                .first()
                .map(|s| classify_layer(&c.file, s).label())
                .unwrap_or("CORE");
            let sym_summary: Vec<String> = c
                .symbols
                .iter()
                .take(3)
                .map(|s| format!("{}:L{}", s.node.name, s.node.start_line))
                .collect();
            let id = format!("F{}", i + 1 + evidence_file_set.len());
            if sym_summary.is_empty() {
                out.push(format!("- {id} `{rel}` [{layer}]"));
            } else {
                out.push(format!(
                    "- {id} `{rel}` [{layer}] — {}",
                    sym_summary.join(", ")
                ));
            }
        }
        let extra = catalog_rest.len().saturating_sub(MAX_CATALOG_REMAINING);
        if extra > 0 {
            out.push(format!(
                "- … +{extra} more files (page with a narrower `path:` / `query:`)"
            ));
        }
    }

    let dirs = collect_directory_panorama(graph, top_files, tokens, scope, dirindex);
    let mut shown_anchor = 0usize;
    let mut shown_graph = 0usize;
    let mut skipped_foreign = 0usize;
    let mut other_groups: HashMap<DirGroup, usize> = HashMap::new();
    let mut dir_block: Vec<String> = Vec::new();
    for d in &dirs {
        match d.group {
            DirGroup::Anchor if shown_anchor < MAX_ANCHOR_DIRS => {
                if shown_anchor == 0 {
                    dir_block.push("> **① 锚定目录**".to_string());
                }
                shown_anchor += 1;
                let rel = rel_disp(&d.path, root);
                dir_block.push(format!(
                    "> | {:.1} | `{rel}/` | {}/{} files · peak {:.1}",
                    d.score, d.anchored_files, d.total_files, d.peak_file_score
                ));
            }
            DirGroup::GraphLinked if shown_graph < MAX_GRAPH_DIRS => {
                if !same_repo_as_hits(&d.path, &hit_paths, root) {
                    skipped_foreign += 1;
                    continue;
                }
                if shown_graph == 0 {
                    dir_block.push("> **⑤ 图连通（同仓 callee/caller）**".to_string());
                }
                shown_graph += 1;
                let rel = rel_disp(&d.path, root);
                dir_block.push(format!("> | {:.1} | `{rel}/`", d.score));
            }
            DirGroup::GraphLinked => {
                if same_repo_as_hits(&d.path, &hit_paths, root) {
                    *other_groups.entry(DirGroup::GraphLinked).or_insert(0) += 1;
                } else {
                    skipped_foreign += 1;
                }
            }
            DirGroup::Anchor => {
                *other_groups.entry(DirGroup::Anchor).or_insert(0) += 1;
            }
            g => {
                *other_groups.entry(g).or_insert(0) += 1;
            }
        }
    }
    if !dir_block.is_empty() || skipped_foreign > 0 || !other_groups.is_empty() {
        out.push(
            "\n> 📁 **Directory Panorama** (anchor + same-repo graph; other groups counted):"
                .to_string(),
        );
        out.extend(dir_block);
        if !other_groups.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            for (g, n) in [
                (
                    DirGroup::Subtree,
                    other_groups.get(&DirGroup::Subtree).copied().unwrap_or(0),
                ),
                (
                    DirGroup::ParentChain,
                    other_groups
                        .get(&DirGroup::ParentChain)
                        .copied()
                        .unwrap_or(0),
                ),
                (
                    DirGroup::Sibling,
                    other_groups.get(&DirGroup::Sibling).copied().unwrap_or(0),
                ),
                (
                    DirGroup::PathHit,
                    other_groups.get(&DirGroup::PathHit).copied().unwrap_or(0),
                ),
                (
                    DirGroup::GraphLinked,
                    other_groups
                        .get(&DirGroup::GraphLinked)
                        .copied()
                        .unwrap_or(0),
                ),
                (
                    DirGroup::Anchor,
                    other_groups.get(&DirGroup::Anchor).copied().unwrap_or(0),
                ),
            ] {
                if n > 0 {
                    parts.push(format!("{}×{n}", g.label()));
                }
            }
            if !parts.is_empty() {
                out.push(format!(">   … other groups: {}", parts.join(", ")));
            }
        }
        if skipped_foreign > 0 {
            out.push(format!(
                ">   SIBLING/foreign graph-links ignored (same-name noise): {skipped_foreign} dirs — do not treat as searched"
            ));
        }
    }

    let remaining_listed = catalog_rest.len().min(MAX_CATALOG_REMAINING);
    let coverage = format!(
        "> 📊 **Coverage** evidence {ev}/{total} files · catalog listed {listed} remaining · omitted-sym {omitted_symbols} · {folded} span(s) folded ({folded_lines} lines) · spine {spine_shown}/{spine_total} hops · {adjacent} adjacent dir(s) in 📁",
        ev = evidence_file_set.len(),
        total = candidates.len(),
        listed = remaining_listed,
        folded = folded_spans,
        adjacent = dirs.len(),
    );
    out[coverage_idx] = coverage;
    let _ = remaining;

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_qualified_query_parser() {
        let q =
            parse_field_qualified_query("kind:trait path:session name:ToolMiddleware agent loop");
        assert_eq!(q.kind_filters, vec!["trait"]);
        assert_eq!(q.path_filters, vec!["session"]);
        assert_eq!(q.name_filters, vec!["ToolMiddleware"]);
        assert_eq!(q.clean_text, "agent loop");
    }

    #[test]
    fn test_file_capsule_generation() {
        let mut graph = CodeGraph::new();
        let file = PathBuf::from("crates/kernel/src/agent.rs");

        let node1 = SymbolNode {
            id: 1,
            name: "Agent".to_string(),
            kind: SymbolKind::Struct,
            visibility: super::super::graph::Visibility::Public,
            file: file.clone(),
            start_line: 10,
            end_line: 50,
            signature: None,
            ..Default::default()
        };
        let node2 = SymbolNode {
            id: 2,
            name: "ToolMiddleware".to_string(),
            kind: SymbolKind::Trait,
            visibility: super::super::graph::Visibility::Public,
            file: file.clone(),
            start_line: 60,
            end_line: 80,
            signature: None,
            ..Default::default()
        };
        let node3 = SymbolNode {
            id: 3,
            name: "run_turn".to_string(),
            kind: SymbolKind::Function,
            visibility: super::super::graph::Visibility::Public,
            file: file.clone(),
            start_line: 100,
            end_line: 200,
            signature: None,
            ..Default::default()
        };

        graph.add_symbol(node1);
        graph.add_symbol(node2);
        graph.add_symbol(node3);

        let capsule = graph.file_capsule(&file);
        assert!(!capsule.is_empty());
        let capsule_joined = capsule.join(" | ");
        assert!(capsule_joined.contains("Agent:L10"));
        assert!(capsule_joined.contains("ToolMiddleware:L60"));
        assert!(capsule_joined.contains("run_turn:L100"));
    }

    #[test]
    fn render_explore_output_includes_coverage_summary() {
        let mut graph = CodeGraph::new();
        // Unique per-test path avoids cross-test SESSION_SENT_SPANS dedup pollution.
        let file = PathBuf::from("crates/kernel/src/coverage_probe.rs");
        let mut nodes = Vec::new();
        for i in 0..6u64 {
            let node = SymbolNode {
                id: i + 1,
                name: format!("sym_{i}"),
                kind: super::super::graph::SymbolKind::Function,
                visibility: super::super::graph::Visibility::Public,
                file: file.clone(),
                start_line: (10 + i * 10) as usize,
                end_line: (20 + i * 10) as usize,
                signature: None,
                ..Default::default()
            };
            graph.add_symbol(node.clone());
            nodes.push(node);
        }
        let symbols: Vec<ScoredSymbol> = nodes
            .into_iter()
            .map(|node| ScoredSymbol {
                node,
                total_score: 30.0,
                name_score: 25.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 1.0,
            })
            .collect();
        // 6 symbols in one file → per-file cap (4) omits 2.
        let candidates = vec![FileCandidate {
            file,
            top_score: 50.0,
            symbols,
        }];
        let root = PathBuf::from(".");
        let out = render_explore_output(
            &graph,
            &root,
            "coverage probe",
            &candidates,
            &[],
            false,
            8,
            None,
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        assert!(
            out.contains("📊 **Coverage**"),
            "coverage summary missing:\n{out}"
        );
        assert!(
            out.contains("evidence 1/1 files"),
            "shown/total missing:\n{out}"
        );
        assert!(
            out.contains("omitted-sym 2"),
            "omitted count missing:\n{out}"
        );
        assert!(
            out.contains("spine 0/0 hops"),
            "spine counts missing:\n{out}"
        );
        assert!(out.contains("**NEXT**"), "work-order NEXT missing:\n{out}");
        assert!(out.contains("EVIDENCE"), "evidence section missing:\n{out}");
    }

    #[test]
    fn render_explore_output_reports_hidden_and_spine_overflow() {
        let graph = CodeGraph::new();
        let root = PathBuf::from(".");
        // 15 files > max_files(8) + 6 trailing-pointer cap(14) → exactly 1 is never listed.
        let mut candidates = Vec::new();
        for f in 0..15usize {
            // Real candidates always carry >= 1 scored symbol (only score>=12 symbols enter);
            // replicate that invariant so the table's `symbols[0]` lookup is valid.
            let node = SymbolNode {
                id: (f + 100) as u64,
                name: format!("hidden_sym_{f}"),
                kind: super::super::graph::SymbolKind::Function,
                visibility: super::super::graph::Visibility::Public,
                file: PathBuf::from(format!("crates/kernel/src/hidden_{f}.rs")),
                start_line: 1,
                end_line: 10,
                signature: None,
                ..Default::default()
            };
            candidates.push(FileCandidate {
                file: node.file.clone(),
                top_score: 50.0,
                symbols: vec![ScoredSymbol {
                    node,
                    total_score: 30.0,
                    name_score: 25.0,
                    doc_score: 0.0,
                    inline_score: 0.0,
                    graph_mass: 1.0,
                }],
            });
        }
        // 40 anonymous edges → business-hop cap (12), rest counted not drawn.
        let flow_spine: Vec<(u64, Option<u64>, super::super::graph::EdgeKind)> = (0..40u64)
            .map(|i| (i, Some(i + 1), super::super::graph::EdgeKind::Calls))
            .collect();
        let out = render_explore_output(
            &graph,
            &root,
            "q",
            &candidates,
            &flow_spine,
            true,
            8,
            None,
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        assert!(
            out.contains("evidence 5/15 files"),
            "evidence auction cap:\n{out}"
        );
        assert!(out.contains("CATALOG"), "catalog section missing:\n{out}");
        assert!(
            out.contains("PARENT DIRECTORY")
                && !out.contains("copy path into read_file / code_explore"),
            "catalog must not tell the model to pass a file path to code_explore:\n{out}"
        );
        assert!(
            out.contains("hidden_8.rs"),
            "remaining candidate listing missing:\n{out}"
        );
        assert!(
            out.contains("hidden_14.rs"),
            "last remaining candidate missing:\n{out}"
        );
        assert!(
            out.contains("spine 12/40 hops"),
            "spine overflow count:\n{out}"
        );
        assert!(
            out.contains("more edges omitted"),
            "omitted-hop note missing:\n{out}"
        );
        assert!(
            !out.contains("```mermaid"),
            "mermaid dump must not appear:\n{out}"
        );
    }

    #[test]
    fn extract_flow_spine_traces_all_callers_and_callees() {
        use super::super::graph::{Edge, EdgeKind, SymbolKind, Visibility};

        let mut graph = CodeGraph::new();
        // Target symbol with 6 callers + 2 callees. The old implementation capped
        // callers at 3 and callees at 4 per seed — the find-all contract must
        // surface every one of them.
        let target_file = PathBuf::from("crates/tools/src/repair.rs");
        let target = SymbolNode {
            id: 1,
            name: "repair_tool_args".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: target_file.clone(),
            start_line: 18,
            end_line: 70,
            signature: None,
            ..Default::default()
        };
        let target_id = target.id;
        graph.add_symbol(target.clone());

        let mut caller_ids = Vec::new();
        for i in 0..6u64 {
            let node = SymbolNode {
                id: 100 + i,
                name: format!("caller_{i}"),
                kind: SymbolKind::Function,
                visibility: Visibility::Public,
                file: PathBuf::from(format!("crates/tools/src/caller_{i}.rs")),
                start_line: 1,
                end_line: 5,
                signature: None,
                ..Default::default()
            };
            graph.add_symbol(node.clone());
            caller_ids.push(node.id);
            graph.add_edge(
                node.id,
                Edge {
                    to: target_id,
                    kind: EdgeKind::Calls,
                    line: 3,
                },
            );
        }

        let mut callee_ids = Vec::new();
        for i in 0..2u64 {
            let node = SymbolNode {
                id: 200 + i,
                name: format!("callee_{i}"),
                kind: SymbolKind::Function,
                visibility: Visibility::Public,
                file: target_file.clone(),
                start_line: 80 + i as usize * 10,
                end_line: 90 + i as usize * 10,
                signature: None,
                ..Default::default()
            };
            graph.add_symbol(node.clone());
            callee_ids.push(node.id);
            graph.add_edge(
                target_id,
                Edge {
                    to: node.id,
                    kind: EdgeKind::Calls,
                    line: 30,
                },
            );
        }

        let candidates = vec![FileCandidate {
            file: target_file,
            top_score: 240.0,
            symbols: vec![ScoredSymbol {
                node: target,
                total_score: 240.0,
                name_score: 240.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 10.0,
            }],
        }];

        let (spine, connected) = extract_flow_spine(&graph, &candidates);
        assert!(connected, "spine should be connected");
        let spine_froms: Vec<u64> = spine.iter().map(|(f, _, _)| *f).collect();
        let spine_tos: Vec<u64> = spine.iter().filter_map(|(_, t, _)| *t).collect();

        // Find-all contract: every caller appears as an incoming edge (its id is
        // the FROM of a caller edge) and every callee as an outgoing TO.
        for id in &caller_ids {
            assert!(
                spine_froms.contains(id),
                "caller {id} missing from spine:\n{spine:?}"
            );
        }
        for id in &callee_ids {
            assert!(
                spine_tos.contains(id),
                "callee {id} missing from spine:\n{spine:?}"
            );
        }
        assert!(
            spine_tos.contains(&target_id),
            "target missing as edge target:\n{spine:?}"
        );
    }

    #[test]
    fn render_explore_output_surfaces_scope_contract_and_low_hit_hints() {
        use super::super::graph::{SymbolKind, Visibility};

        let mut graph = CodeGraph::new();
        // One trait symbol in one file → 1 candidate (low hit) + contract hit.
        let node = SymbolNode {
            id: 1,
            name: "Tool".into(),
            kind: SymbolKind::Trait,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/kernel/src/tool.rs"),
            start_line: 182,
            end_line: 210,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(node.clone());
        let candidates = vec![FileCandidate {
            file: node.file.clone(),
            top_score: 240.0,
            symbols: vec![ScoredSymbol {
                node,
                total_score: 240.0,
                name_score: 240.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 10.0,
            }],
        }];
        let root = PathBuf::from(".");
        let scope = PathBuf::from("crates/kernel/src");
        let out = render_explore_output(
            &graph,
            &root,
            "Tool execute error handling",
            &candidates,
            &[],
            false,
            8,
            Some(&scope),
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        assert!(out.contains("🔭 scope"), "scope hint missing:\n{out}");
        assert!(
            out.contains("SIBLING layers"),
            "sibling-layers hint missing:\n{out}"
        );
        assert!(
            out.contains("🧩 Contract hit"),
            "contract hint missing:\n{out}"
        );
        assert!(
            out.contains("`impl <name>`"),
            "impl-follow-up hint missing:\n{out}"
        );
        assert!(
            out.contains("🎯 Low hit count"),
            "low-hit hint missing:\n{out}"
        );
        assert!(out.contains("**NEXT**"), "work-order NEXT missing:\n{out}");
        // Contract hint must NOT fire when the top symbol is not a contract (function here),
        // while the low-hit hint still fires (1 candidate).
        let func_node = SymbolNode {
            id: 2,
            name: "run_turn".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/kernel/src/agent.rs"),
            start_line: 10,
            end_line: 40,
            signature: None,
            ..Default::default()
        };
        let func_candidates = vec![FileCandidate {
            file: func_node.file.clone(),
            top_score: 240.0,
            symbols: vec![ScoredSymbol {
                node: func_node,
                total_score: 240.0,
                name_score: 240.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 10.0,
            }],
        }];
        let no_contract = render_explore_output(
            &graph,
            &root,
            "q",
            &func_candidates,
            &[],
            false,
            8,
            None,
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        assert!(
            !no_contract.contains("🧩 Contract hit"),
            "contract hint should not fire for a function:\n{no_contract}"
        );
        assert!(
            no_contract.contains("🎯 Low hit count"),
            "low-hit should still fire:\n{no_contract}"
        );
    }

    #[test]
    fn adjacent_files_surface_siblings_not_matched_by_query() {
        use super::super::graph::{SymbolKind, Visibility};

        let mut graph = CodeGraph::new();
        // The hit: repair.rs in tools/. The query matched THIS file only.
        let hit_node = SymbolNode {
            id: 1,
            name: "repair_tool_args".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/capabilities/src/tools/repair.rs"),
            start_line: 18,
            end_line: 70,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(hit_node.clone());
        // A sibling in the SAME dir that the query never surfaced (no shared term):
        // tool_feedback.rs — the real-world case that "path: kernel" + ranked query
        // originally missed. MUST appear in 📁.
        let sibling_node = SymbolNode {
            id: 2,
            name: "parse_tool_args".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/capabilities/src/tools/tool_feedback.rs"),
            start_line: 98,
            end_line: 130,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(sibling_node.clone());
        // An unrelated file in a FAR directory: MUST NOT appear.
        let far_node = SymbolNode {
            id: 3,
            name: "unrelated".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/kernel/src/agent.rs"),
            start_line: 10,
            end_line: 40,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(far_node.clone());

        let candidates = vec![FileCandidate {
            file: hit_node.file.clone(),
            top_score: 240.0,
            symbols: vec![ScoredSymbol {
                node: hit_node,
                total_score: 240.0,
                name_score: 240.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 10.0,
            }],
        }];
        let root = PathBuf::from(".");
        let out = render_explore_output(
            &graph,
            &root,
            "repair_tool_args",
            &candidates,
            &[],
            false,
            8,
            None,
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        // 📁 now lists the hit's DIRECTORY (not sibling filenames): the tools/
        // dir must appear; the far kernel/src dir must not.
        assert!(
            out.contains("crates/capabilities/src/tools/"),
            "hit dir must be listed in 📁:\n{out}"
        );
        assert!(
            !out.contains("kernel/src/"),
            "far dir must NOT be listed:\n{out}"
        );
        assert!(
            out.contains("adjacent dir(s) in 📁"),
            "adjacent dir count missing in Coverage:\n{out}"
        );
    }

    #[test]
    fn coverage_reports_scope_vs_workspace_contrast() {
        use super::super::graph::{SymbolKind, Visibility};

        let mut graph = CodeGraph::new();
        // File INSIDE the scope: a kernel trait contract.
        let in_scope_node = SymbolNode {
            id: 1,
            name: "Tool".into(),
            kind: SymbolKind::Trait,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/kernel/src/tool.rs"),
            start_line: 182,
            end_line: 210,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(in_scope_node.clone());
        // File OUTSIDE the scope: the capabilities-layer repair implementation that a
        // kernel-scoped query would entirely miss (the exact first-round blind spot).
        let out_of_scope_node = SymbolNode {
            id: 2,
            name: "repair_tool_args".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("crates/capabilities/src/tools/repair.rs"),
            start_line: 18,
            end_line: 70,
            signature: None,
            ..Default::default()
        };
        graph.add_symbol(out_of_scope_node.clone());

        let candidates = vec![FileCandidate {
            file: in_scope_node.file.clone(),
            top_score: 240.0,
            symbols: vec![ScoredSymbol {
                node: in_scope_node,
                total_score: 240.0,
                name_score: 240.0,
                doc_score: 0.0,
                inline_score: 0.0,
                graph_mass: 10.0,
            }],
        }];
        let root = PathBuf::from(".");
        let scope = PathBuf::from("crates/kernel/src");
        let out = render_explore_output(
            &graph,
            &root,
            "Tool execute error handling",
            &candidates,
            &[],
            false,
            8,
            Some(&scope),
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        // The Coverage line must surface the workspace contrast: only 1 of 2 files /
        // 1 of 2 symbols were searched, and 1 symbol is OUTSIDE the scope — so a
        // scope-confined "no mechanism" conclusion is invalid.
        assert!(
            out.contains("in-scope 1 of 2 indexed files"),
            "scope-vs-workspace file counts missing:\n{out}"
        );
        assert!(
            out.contains("1/2 symbols"),
            "scope-vs-workspace symbol counts missing:\n{out}"
        );
        assert!(
            out.contains("1 OUTSIDE not ranked"),
            "outside-scope warning missing:\n{out}"
        );
    }

    #[test]
    fn path_matches_scope_multiformat_and_boundary_resilience() {
        // 1. Cross-format slash & prefix match
        assert!(path_matches_scope(
            Path::new("atomcode/crates/atomcode-tuix/src/event_loop.rs"),
            Path::new("atomcode/crates/atomcode-tuix")
        ));
        assert!(path_matches_scope(
            Path::new("atomcode\\crates\\atomcode-tuix\\src\\event_loop.rs"),
            Path::new("atomcode/crates/atomcode-tuix")
        ));
        assert!(path_matches_scope(
            Path::new("atomcode/crates/atomcode-tuix/src/event_loop.rs"),
            Path::new("crates/atomcode-tuix")
        ));
        assert!(path_matches_scope(
            Path::new(r"\\?\E:\code\agents\atomcode\crates\atomcode-tuix\src\event_loop.rs"),
            Path::new("atomcode/crates/atomcode-tuix")
        ));

        // 2. Exact file match
        assert!(path_matches_scope(
            Path::new(
                "coupon-mall-demo/backend/src/main/java/com/demo/coupon/service/CouponService.java"
            ),
            Path::new(
                "coupon-mall-demo/backend/src/main/java/com/demo/coupon/service/CouponService.java"
            )
        ));

        // 3. Segment boundary mismatch (prevent substring false positives)
        assert!(!path_matches_scope(
            Path::new("atomcode/crates/atomcode-tuix-demo/src/main.rs"),
            Path::new("atomcode/crates/atomcode-tuix")
        ));
        assert!(!path_matches_scope(
            Path::new("atomcode/crates/atomcode-tuix/src/event_loop.rs"),
            Path::new("atomcode/crates/atomcode-tui")
        ));

        // 4. Absolute canonicalized scope vs relative indexed file (the live Zero-Hit case)
        assert!(path_matches_scope(
            Path::new("coupon-mall-demo/backend/src/main/java/com/demo/coupon/service/CouponBatchIssueService.java"),
            Path::new(r"E:\code\agents\coupon-mall-demo\backend\src\main\java\com\demo\coupon\service")
        ));
        assert!(path_matches_scope(
            Path::new(
                r"E:\code\agents\coupon-mall-demo\backend\src\main\java\com\demo\coupon\service\CouponService.java"
            ),
            Path::new("coupon-mall-demo/backend/src/main/java/com/demo/coupon/service")
        ));
    }

    #[test]
    fn strip_diagnostic_headers_drops_perf_and_index_status() {
        let raw = concat!(
            "> ⚡ **Performance**: ⏱️ **Cost Time**: 13150ms (Index: 7386ms | Retrieval: 5763ms | Render: 0ms)\n",
            "\n",
            "> ⚡ **Index Status**: [Incremental] 增量补丁（重解析 2 个文件，保留 12166 个，移除 0 个）\n",
            "\n",
            "body line\n",
        );
        let stripped = strip_diagnostic_headers(raw);
        assert!(!stripped.contains("Cost Time"), "{stripped}");
        assert!(!stripped.contains("Index Status"), "{stripped}");
        assert!(!stripped.contains("增量补丁"), "{stripped}");
        assert!(stripped.contains("body line"), "{stripped}");
    }

    #[test]
    fn fresh_headers_use_this_request_stats_not_cached_snapshot() {
        let body = "cached body\n";
        let hit = super::super::index::RefreshStats {
            reparsed: 0,
            removed: 0,
            kept: 12168,
            cache_hit: true,
            ..Default::default()
        };
        let out = with_fresh_diagnostic_headers(
            body,
            (
                Duration::from_millis(40),
                Duration::from_millis(30),
                Duration::from_millis(0),
                Duration::from_millis(0),
            ),
            Some(&hit),
        );
        assert!(out.contains("Cost Time**: 40ms"), "{out}");
        assert!(out.contains("Index: 30ms"), "{out}");
        assert!(out.contains("Retrieval: 0ms"), "{out}");
        assert!(out.contains("[Cache HIT]"), "{out}");
        assert!(out.contains("复用 12168 个文件单元"), "{out}");
        assert!(!out.contains("增量补丁"), "{out}");
        assert!(out.contains("cached body"), "{out}");
    }

    #[test]
    fn index_status_64_reparsed_with_kept_is_incremental_not_miss() {
        let stats = super::super::index::RefreshStats {
            reparsed: 64,
            removed: 0,
            kept: 12619,
            cache_hit: false,
            ..Default::default()
        };
        let line = index_status_line(&stats);
        assert!(line.contains("[Incremental]"), "{line}");
        assert!(!line.contains("[Cache MISS]"), "{line}");
        assert!(line.contains("保留 12619"), "{line}");
    }

    #[test]
    fn index_status_full_rebuild_is_miss() {
        let stats = super::super::index::RefreshStats {
            reparsed: 12000,
            removed: 0,
            kept: 0,
            cache_hit: false,
            ..Default::default()
        };
        let line = index_status_line(&stats);
        assert!(line.contains("[Cache MISS]"), "{line}");
        assert!(line.contains("全量重建"), "{line}");
    }

    #[tokio::test]
    async fn identical_query_rewrites_frozen_incremental_headers() {
        use atomcode_kernel::tool::{ProgressSink, Tool, ToolContext};
        use tokio_util::sync::CancellationToken;

        query_cache_clear();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/hot.rs"), "pub fn cached_symbol() {}\n").unwrap();
        let idx = Arc::new(CodeIndex::new());
        let _ = idx.get(d.path());
        let tool = CodeExploreTool::new(idx);
        let ctx = ToolContext {
            working_dir: d.path().to_path_buf(),
            cancel: CancellationToken::new(),
            progress: ProgressSink::noop(),
            requester: None,
        };
        let args = r#"{"query":"cached_symbol","path":"src"}"#;
        let first = tool.execute(args, &ctx).await;
        assert!(!first.is_error, "{}", first.content);
        assert!(
            first.content.contains("cached_symbol"),
            "first hit must render the symbol:\n{}",
            first.content
        );

        let second = tool.execute(args, &ctx).await;
        assert!(!second.is_error, "{}", second.content);
        assert!(
            second.content.contains("[Cache HIT]"),
            "second identical query must rewrite Index Status from this restat, not freeze 增量补丁:\n{}",
            second.content
        );
        assert!(
            !second.content.contains("增量补丁"),
            "cached first-hit incremental header must not leak:\n{}",
            second.content
        );
        assert!(
            second.content.contains("Retrieval: 0ms"),
            "query-cache HIT must skip scoring:\n{}",
            second.content
        );
        query_cache_clear();
    }

    #[tokio::test]
    async fn accepts_workspace_root_paths() {
        use atomcode_kernel::tool::{ProgressSink, Tool, ToolContext};
        use tokio_util::sync::CancellationToken;

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/hot.rs"), "pub fn cached_symbol() {}\n").unwrap();
        let idx = Arc::new(CodeIndex::new());
        let _ = idx.get(d.path());
        let tool = CodeExploreTool::new(idx);
        let ctx = ToolContext {
            working_dir: d.path().to_path_buf(),
            cancel: CancellationToken::new(),
            progress: ProgressSink::noop(),
            requester: None,
        };
        for path in [".", "./", "~", d.path().to_str().unwrap()] {
            let args = serde_json::json!({"query": "cached_symbol", "path": path}).to_string();
            let r = tool.execute(&args, &ctx).await;
            assert!(
                !r.is_error,
                "workspace root must be accepted: {args}\n{}",
                r.content
            );
        }
        let ok = tool
            .execute(r#"{"query":"cached_symbol","path":"src"}"#, &ctx)
            .await;
        assert!(
            !ok.is_error,
            "concrete subdirectory must still work:\n{}",
            ok.content
        );
    }

    #[tokio::test]
    async fn rejects_single_file_path() {
        use atomcode_kernel::tool::{ProgressSink, Tool, ToolContext};
        use tokio_util::sync::CancellationToken;

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/hot.rs"), "pub fn cached_symbol() {}\n").unwrap();
        let idx = Arc::new(CodeIndex::new());
        let tool = CodeExploreTool::new(idx);
        let ctx = ToolContext {
            working_dir: d.path().to_path_buf(),
            cancel: CancellationToken::new(),
            progress: ProgressSink::noop(),
            requester: None,
        };
        let r = tool
            .execute(r#"{"query":"cached_symbol","path":"src/hot.rs"}"#, &ctx)
            .await;
        assert!(r.is_error, "{}", r.content);
        assert!(
            r.content.contains("not a single file") && r.content.contains("read_file"),
            "{}",
            r.content
        );
        assert!(r.content.contains("src"), "{}", r.content);
        assert!(
            r.content.contains("query=")
                || r.content.contains("query<")
                || r.content.contains("`query`"),
            "error must tell the model to put the symbol in query:\n{}",
            r.content
        );
        let toml = tool
            .execute(r#"{"query":"name","path":"Cargo.toml"}"#, &ctx)
            .await;
        assert!(
            toml.is_error && toml.content.contains("not a single file"),
            "{}",
            toml.content
        );
    }

    #[test]
    fn description_forbids_file_path_and_allows_nl_or_symbol() {
        let tool = CodeExploreTool::new(Arc::new(CodeIndex::new()));
        let d = tool.description();
        assert!(
            d.contains("crates/atomcode-coding") && d.contains("src/auth"),
            "description must show directory examples:\n{d}"
        );
        assert!(
            d.contains("src/auth.rs") && d.contains("read_file"),
            "description must show a file as BAD before the first call:\n{d}"
        );
        assert!(
            d.contains("CodeExploreTool") && (d.contains("鉴权") || d.contains("Chinese")),
            "description must allow a precise symbol or natural Chinese/English:\n{d}"
        );
        let schema = tool.parameters_schema();
        let path_d = schema["properties"]["path"]["description"]
            .as_str()
            .unwrap_or("");
        let query_d = schema["properties"]["query"]["description"]
            .as_str()
            .unwrap_or("");
        assert!(
            path_d.contains("src/auth.rs") && path_d.contains("NEVER a file"),
            "path schema must reject files up front:\n{path_d}"
        );
        assert!(
            query_d.contains("CodeExploreTool") && query_d.contains("Chinese"),
            "query schema must allow symbol or natural language:\n{query_d}"
        );
    }

    fn scored(node: SymbolNode, score: f64) -> ScoredSymbol {
        ScoredSymbol {
            node,
            total_score: score,
            name_score: score,
            doc_score: 0.0,
            inline_score: 0.0,
            graph_mass: 1.0,
        }
    }

    fn node_at(id: u64, name: &str, kind: SymbolKind, file: &str, line: usize) -> SymbolNode {
        SymbolNode {
            id,
            name: name.into(),
            kind,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from(file),
            start_line: line,
            end_line: line + 4,
            signature: None,
            ..Default::default()
        }
    }

    #[test]
    fn classify_layer_covers_backend_frontend_docs_and_config() {
        let java_ctl = scored(
            node_at(
                1,
                "acquireCoupon",
                SymbolKind::Method,
                "backend/controller/CouponController.java",
                26,
            ),
            80.0,
        );
        assert_eq!(
            classify_layer(&java_ctl.node.file, &java_ctl),
            CodeLayer::Http
        );
        let java_svc = scored(
            node_at(
                2,
                "CouponService",
                SymbolKind::Interface,
                "backend/service/CouponService.java",
                12,
            ),
            70.0,
        );
        assert_eq!(
            classify_layer(&java_svc.node.file, &java_svc),
            CodeLayer::Service
        );
        let java_impl = scored(
            node_at(
                3,
                "poupou",
                SymbolKind::Method,
                "backend/service/impl/CouponServiceImpl.java",
                21,
            ),
            90.0,
        );
        assert_eq!(
            classify_layer(&java_impl.node.file, &java_impl),
            CodeLayer::Impl
        );
        let mapper = scored(
            node_at(
                4,
                "selectAvailableCouponsByUserId",
                SymbolKind::Method,
                "backend/mapper/CouponMapper.java",
                20,
            ),
            60.0,
        );
        assert_eq!(classify_layer(&mapper.node.file, &mapper), CodeLayer::Data);
        let xml = scored(
            node_at(
                5,
                "com.demo.coupon.mapper.CouponMapper::consumeUserCoupon",
                SymbolKind::SqlStatement,
                "backend/resources/mapper/CouponMapper.xml",
                30,
            ),
            55.0,
        );
        assert_eq!(classify_layer(&xml.node.file, &xml), CodeLayer::Sql);
        let yml = scored(
            node_at(
                6,
                "spring",
                SymbolKind::ConfigProperty,
                "backend/resources/application.yml",
                1,
            ),
            20.0,
        );
        assert_eq!(classify_layer(&yml.node.file, &yml), CodeLayer::Config);
        let md = scored(
            node_at(7, "优惠券领取", SymbolKind::Module, "README.md", 1),
            15.0,
        );
        assert_eq!(classify_layer(&md.node.file, &md), CodeLayer::Doc);
        let vue = scored(
            node_at(
                8,
                "CouponCard",
                SymbolKind::UiElement,
                "frontend/src/components/CouponCard.vue",
                1,
            ),
            40.0,
        );
        assert_eq!(classify_layer(&vue.node.file, &vue), CodeLayer::Ui);
        let tsx = scored(
            node_at(
                9,
                "HomePage",
                SymbolKind::Function,
                "packages/app/src/pages/HomePage.tsx",
                1,
            ),
            40.0,
        );
        assert_eq!(classify_layer(&tsx.node.file, &tsx), CodeLayer::Ui);
        let go_handler = scored(
            node_at(
                10,
                "Acquire",
                SymbolKind::Function,
                "internal/httpapi/coupon_handler.go",
                12,
            ),
            50.0,
        );
        assert_eq!(
            classify_layer(&go_handler.node.file, &go_handler),
            CodeLayer::Http
        );
        let py_view = scored(
            node_at(11, "acquire", SymbolKind::Function, "shop/views.py", 8),
            50.0,
        );
        assert_eq!(
            classify_layer(&py_view.node.file, &py_view),
            CodeLayer::Http
        );
        let rs_core = scored(
            node_at(
                12,
                "cycle_reasoning_effort",
                SymbolKind::Function,
                "crates/atomcode-tuix/src/state.rs",
                1981,
            ),
            136.0,
        );
        assert_eq!(
            classify_layer(&rs_core.node.file, &rs_core),
            CodeLayer::Core
        );
    }

    #[test]
    fn auction_buys_one_span_per_primary_layer_not_top_n_files() {
        let mut cands = Vec::new();
        for i in 0..9u64 {
            let n = node_at(
                i + 1,
                "render",
                SymbolKind::Function,
                &format!("src/render/plain_{i}.rs"),
                10,
            );
            cands.push(FileCandidate {
                file: n.file.clone(),
                top_score: 150.0,
                symbols: vec![scored(n, 150.0)],
            });
        }
        let ctl = node_at(
            100,
            "acquireCoupon",
            SymbolKind::Method,
            "backend/controller/CouponController.java",
            26,
        );
        cands.push(FileCandidate {
            file: ctl.file.clone(),
            top_score: 80.0,
            symbols: vec![scored(ctl, 80.0)],
        });
        let xml = node_at(
            101,
            "consumeUserCoupon",
            SymbolKind::SqlStatement,
            "backend/resources/mapper/CouponMapper.xml",
            30,
        );
        cands.push(FileCandidate {
            file: xml.file.clone(),
            top_score: 55.0,
            symbols: vec![scored(xml, 55.0)],
        });
        let picked = auction_evidence(&cands, 5);
        let layers: Vec<CodeLayer> = picked.iter().map(|p| p.layer).collect();
        assert!(
            layers.contains(&CodeLayer::Http),
            "HTTP layer must be bought despite lower score: {layers:?}"
        );
        assert!(
            layers.contains(&CodeLayer::Sql),
            "SQL/xml layer must be bought: {layers:?}"
        );
        assert!(
            picked.len() <= 5,
            "auction must respect max_spans: {}",
            picked.len()
        );
    }

    #[test]
    fn panorama_drops_foreign_graph_dirs() {
        use super::super::graph::{Edge, EdgeKind, Visibility};

        let mut graph = CodeGraph::new();
        let hit = node_at(
            1,
            "acquireCoupon",
            SymbolKind::Method,
            "coupon-mall-demo/controller/CouponController.java",
            26,
        );
        graph.add_symbol(hit.clone());
        let foreign = node_at(
            2,
            "render",
            SymbolKind::Function,
            "grok-build/crates/pager/src/render.rs",
            10,
        );
        graph.add_symbol(foreign.clone());
        graph.add_edge(
            hit.id,
            Edge {
                to: foreign.id,
                kind: EdgeKind::Calls,
                line: 28,
            },
        );
        let candidates = vec![FileCandidate {
            file: hit.file.clone(),
            top_score: 200.0,
            symbols: vec![scored(hit, 200.0)],
        }];
        let root = PathBuf::from(".");
        let out = render_explore_output(
            &graph,
            &root,
            "acquireCoupon",
            &candidates,
            &[(1, Some(2), EdgeKind::Calls)],
            true,
            8,
            None,
            &SearchTokens::default(),
            None,
            None,
            None,
        );
        assert!(
            out.contains("coupon-mall-demo/controller"),
            "hit dir must appear:\n{out}"
        );
        assert!(
            !out.contains("grok-build/crates/pager"),
            "foreign same-name graph dir must not be listed as searched:\n{out}"
        );
        assert!(
            out.contains("SIBLING/foreign graph-links ignored")
                || out.contains("foreign graph-links ignored"),
            "foreign skip must be explicit:\n{out}"
        );
        let _ = Visibility::Public;
    }

    #[test]
    fn test_codeintel_2_0_branch_comment_and_sql_boost() {
        let mut graph = CodeGraph::new();

        // 1. Core SQL class with branch inline comment and SQL predicate
        let mut core_sql_node = SymbolNode {
            id: 1,
            name: "RelationOrderNo".to_string(),
            kind: SymbolKind::Method,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from("sources/ERP.API/Apis/Ailai.Order/Models/Data/WhereModel/DailyPerformanceStatSqlStr.cs"),
            start_line: 45,
            end_line: 85,
            signature: Some("private string RelationOrderNo(DailyPerformanceRequest req)".to_string()),
            docstring: None,
            inline_comments: vec!["//总业绩(基本盘)".to_string()],
            comments: vec![super::super::graph::StructuredComment {
                text: "//总业绩(基本盘)".to_string(),
                scope: super::super::graph::CommentScope::BranchInline {
                    branch_kind: "switch_case".to_string(),
                },
                line: 58,
            }],
            sql_predicates: vec![super::super::graph::SqlPredicate {
                raw_clause: "AND po.BKOrderType IN (0,2)".to_string(),
                target_fields: vec!["BKOrderType".to_string()],
                line: 59,
            }],
            string_literals: vec!["AND po.BKOrderType IN (0,2)".to_string()],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 5,
                branch_count: 4,
                has_sql_or_qs: true,
                is_pure_dto: false,
                is_active_logic: true,
            },
        };
        graph.add_symbol(core_sql_node.clone());

        // 2. Pure DTO containing "Performance" in name but no logic
        let mut dto_node = SymbolNode {
            id: 2,
            name: "PerformanceIndexData".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from("sources/ERP.API/Apis/Ailai.Statistics/Models/Data/Workbenchs/PerformanceIndexData.cs"),
            start_line: 10,
            end_line: 40,
            signature: Some("public class PerformanceIndexData".to_string()),
            docstring: Some("/// 工作台考评数据".to_string()),
            inline_comments: vec![],
            comments: vec![],
            sql_predicates: vec![],
            string_literals: vec![],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 1,
                branch_count: 0,
                has_sql_or_qs: false,
                is_pure_dto: true,
                is_active_logic: false,
            },
        };
        graph.add_symbol(dto_node.clone());

        let dt = super::super::bilingual_nlp::DynamicThesaurus::default();
        let tokens = super::super::bilingual_nlp::parse_bilingual_query_with_thesaurus(
            "基本盘业绩 计算",
            &dt,
        );
        let parsed_query =
            super::super::bilingual_nlp::parse_field_qualified_query("基本盘业绩 计算");
        let project_tokens = HashSet::new();
        let bm25_scores = HashMap::new();

        let candidates = score_workspace_symbols(
            &graph,
            &tokens,
            &parsed_query,
            &project_tokens,
            None,
            &bm25_scores,
            &tokens.dense_vector,
            None,
        );

        assert!(!candidates.is_empty(), "Must score candidates");
        assert_eq!(
            candidates[0].file,
            core_sql_node.file,
            "DailyPerformanceStatSqlStr must rank #1 ahead of DTO noise. Actual order: {:?}",
            candidates
                .iter()
                .map(|c| (&c.file, c.top_score))
                .collect::<Vec<_>>()
        );
        assert!(
            candidates[0].top_score > 60.0,
            "Core SQL score must be > 60, was {}",
            candidates[0].top_score
        );
    }

    #[test]
    fn test_codeintel_2_0_daily_ailai_performance_search() {
        let mut graph = CodeGraph::new();

        // 1. 核心 SQL 组装类：DailyPerformanceStatSqlStr.cs (负责每日美莱业绩统计的核心 SQL 生成)
        let core_sql_node = SymbolNode {
            id: 101,
            name: "DailyPerformanceStatSqlStr".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from("sources/ERP.API/Apis/Ailai.Order/Models/Data/WhereModel/DailyPerformanceStatSqlStr.cs"),
            start_line: 15,
            end_line: 120,
            signature: Some("public class DailyPerformanceStatSqlStr".to_string()),
            docstring: Some("/// 每日业绩统计SQL组装策略".to_string()),
            inline_comments: vec!["// 美莱每日业绩汇总计算".to_string(), "// 总业绩(基本盘)".to_string()],
            comments: vec![
                super::super::graph::StructuredComment {
                    text: "/// 每日业绩统计SQL组装策略".to_string(),
                    scope: super::super::graph::CommentScope::Docstring,
                    line: 14,
                },
                super::super::graph::StructuredComment {
                    text: "// 美莱每日业绩汇总计算".to_string(),
                    scope: super::super::graph::CommentScope::BranchInline {
                        branch_kind: "switch_case".to_string(),
                    },
                    line: 48,
                },
            ],
            sql_predicates: vec![
                super::super::graph::SqlPredicate {
                    raw_clause: "AND po.BKOrderType IN (0,2) AND po.OrderDate >= @StartDate".to_string(),
                    target_fields: vec!["BKOrderType".to_string(), "OrderDate".to_string()],
                    line: 50,
                },
            ],
            string_literals: vec!["AND po.BKOrderType IN (0,2)".to_string(), "美莱业绩统计".to_string()],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 8,
                branch_count: 6,
                has_sql_or_qs: true,
                is_pure_dto: false,
                is_active_logic: true,
            },
        };
        graph.add_symbol(core_sql_node.clone());

        // 2. 实体/DTO 类：DailyPerformanceEntity.cs
        let entity_node = SymbolNode {
            id: 102,
            name: "DailyPerformanceEntity".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from(
                "sources/ERP.API/Apis/Ailai.Order/Models/Data/DailyPerformanceEntity.cs",
            ),
            start_line: 10,
            end_line: 60,
            signature: Some("public class DailyPerformanceEntity".to_string()),
            docstring: Some("/// 美莱每日业绩数据实体".to_string()),
            inline_comments: vec![],
            comments: vec![super::super::graph::StructuredComment {
                text: "/// 美莱每日业绩数据实体".to_string(),
                scope: super::super::graph::CommentScope::Docstring,
                line: 9,
            }],
            sql_predicates: vec![],
            string_literals: vec![],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 1,
                branch_count: 0,
                has_sql_or_qs: false,
                is_pure_dto: true,
                is_active_logic: false,
            },
        };
        graph.add_symbol(entity_node.clone());

        // 3. 噪音干扰项：无关工作台考评 DTO
        let noise_node = SymbolNode {
            id: 103,
            name: "PerformanceIndexData".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from("sources/ERP.API/Apis/Ailai.Statistics/Models/Data/Workbenchs/PerformanceIndexData.cs"),
            start_line: 10,
            end_line: 50,
            signature: Some("public class PerformanceIndexData".to_string()),
            docstring: Some("/// 工作台考评数据".to_string()),
            inline_comments: vec![],
            comments: vec![],
            sql_predicates: vec![],
            string_literals: vec![],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 1,
                branch_count: 0,
                has_sql_or_qs: false,
                is_pure_dto: true,
                is_active_logic: false,
            },
        };
        graph.add_symbol(noise_node.clone());

        let dt = super::super::bilingual_nlp::DynamicThesaurus::new();
        let query_text = "每日美莱业绩";
        let tokens =
            super::super::bilingual_nlp::parse_bilingual_query_with_thesaurus(query_text, &dt);
        let parsed_query = super::super::bilingual_nlp::parse_field_qualified_query(query_text);
        let project_tokens = HashSet::new();
        let bm25_scores = HashMap::new();

        let candidates = score_workspace_symbols(
            &graph,
            &tokens,
            &parsed_query,
            &project_tokens,
            None,
            &bm25_scores,
            &tokens.dense_vector,
            None,
        );

        assert!(!candidates.is_empty(), "必须召回相关文件");

        // 核心 SQL 类必须稳居第一
        assert_eq!(
            candidates[0].file, core_sql_node.file,
            "核心 SQL 组装类 DailyPerformanceStatSqlStr.cs 必须排在第 1 位！实际第一位: {:?}",
            candidates[0].file
        );
        assert!(
            candidates[0].top_score >= 80.0,
            "核心类得分必须高权（>= 80.0），实际得分: {:.2}",
            candidates[0].top_score
        );
    }

    fn stat_core(id: u64, name: &str, file: &str, extra_sql: &str) -> SymbolNode {
        SymbolNode {
            id,
            name: name.into(),
            kind: SymbolKind::Method,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from(file),
            start_line: 40,
            end_line: 120,
            docstring: Some("/// 每日业绩 / 基本盘".into()),
            inline_comments: vec!["// 总业绩(基本盘)".into()],
            sql_predicates: vec![super::super::graph::SqlPredicate {
                raw_clause: extra_sql.into(),
                target_fields: vec!["BKOrderType".into(), "MaylifeAmount".into()],
                line: 80,
            }],
            string_literals: vec!["基本盘".into(), extra_sql.into()],
            metrics: super::super::graph::AstMetrics {
                has_sql_or_qs: true,
                is_active_logic: true,
                branch_count: 4,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn order_noise(id: u64, name: &str, file: &str) -> SymbolNode {
        SymbolNode {
            id,
            name: name.into(),
            kind: SymbolKind::Method,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from(file),
            start_line: 10,
            end_line: 80,
            docstring: Some("/// 订单服务".into()),
            sql_predicates: vec![super::super::graph::SqlPredicate {
                raw_clause: "SELECT * FROM PurchaseOrder WHERE OrderId = @id".into(),
                target_fields: vec!["OrderId".into()],
                line: 20,
            }],
            metrics: super::super::graph::AstMetrics {
                is_active_logic: true,
                has_sql_or_qs: true,
                branch_count: 8,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn bkordertype_must_not_rank_order_services_above_stat_sql() {
        let mut graph = CodeGraph::new();
        graph.add_symbol(stat_core(
            201,
            "DailyMaylife",
            "sources/ERP.API/Apis/Ailai.Statistics/Models/Data/StatCustomerModuleData.cs",
            "AND po.BKOrderType IN (0,2) AND MaylifeAmount > 0",
        ));
        graph.add_symbol(stat_core(
            202,
            "DailyPerformanceStatSqlStr",
            "sources/ERP.API/Apis/Ailai.Order/Models/Data/WhereModel/DailyPerformanceStatSqlStr.cs",
            "AND po.BKOrderType IN (0,2)",
        ));
        graph.add_symbol(stat_core(
            203,
            "HomeStatisticsSqlStr",
            "sources/ERP.API/Apis/Ailai.Statistics/Models/Data/HomeStatisticsSqlStr.cs",
            "AND po.BKOrderType IN (0,2) /* 基本盘 */",
        ));
        graph.add_symbol(stat_core(
            204,
            "NewPersonNewPerformanceData",
            "sources/ERP.API/Apis/Ailai.Statistics/Models/Data/NewPersonNewPerformanceData.cs",
            "MaylifeAmount + BKOrderType",
        ));
        graph.add_symbol(stat_core(
            205,
            "AllPerformanceStatSqlStr",
            "sources/ERP.API/Apis/Ailai.Order/Models/Data/WhereModel/AllPerformanceStatSqlStr.cs",
            "AND po.BKOrderType IN (0,2) AND MaylifeAmount > 0",
        ));
        for (id, name, file) in [
            (
                301,
                "DeleteOrderItem",
                "sources/ERP.API/Apis/Ailai.Order/Models/Service/OrderItemService.cs",
            ),
            (
                302,
                "PurchaseWorkOrder",
                "sources/ERP.API/Apis/Ailai.Order/Models/Service/PurchaseWorkOrderService.cs",
            ),
            (
                303,
                "PromotionOrder",
                "sources/ERP.API/Apis/Ailai.Order/Models/Service/PromotionOrderService.cs",
            ),
            (
                304,
                "SyncDBOrder",
                "sources/ERP.API/Apis/Ailai.Order/Models/Service/SyncDBOrderService.cs",
            ),
            (
                305,
                "CanDelivery",
                "sources/ERP.API/Apis/Ailai.Order/Models/Service/CanDeliveryService.cs",
            ),
        ] {
            graph.add_symbol(order_noise(id, name, file));
        }

        let dt = super::super::bilingual_nlp::DynamicThesaurus::new();
        let query = "DailyMaylife 基本盘 BKOrderType MaylifeAmount";
        let tokens = super::super::bilingual_nlp::parse_bilingual_query_with_thesaurus(query, &dt);
        let parsed = super::super::bilingual_nlp::parse_field_qualified_query(query);
        let candidates = score_workspace_symbols(
            &graph,
            &tokens,
            &parsed,
            &HashSet::new(),
            None,
            &HashMap::new(),
            &[],
            None,
        );
        assert!(!candidates.is_empty());
        let files: Vec<String> = candidates
            .iter()
            .map(|c| c.file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let cores = [
            "StatCustomerModuleData.cs",
            "DailyPerformanceStatSqlStr.cs",
            "HomeStatisticsSqlStr.cs",
            "NewPersonNewPerformanceData.cs",
            "AllPerformanceStatSqlStr.cs",
        ];
        let noise = [
            "OrderItemService.cs",
            "PurchaseWorkOrderService.cs",
            "PromotionOrderService.cs",
            "SyncDBOrderService.cs",
            "CanDeliveryService.cs",
        ];
        let core_hits: Vec<&str> = cores
            .iter()
            .copied()
            .filter(|f| files.iter().any(|x| x == *f))
            .collect();
        assert!(
            core_hits.len() >= 4,
            "STAT/SQL cores must be recalled, got {core_hits:?} in {files:?}"
        );
        let first_noise = noise
            .iter()
            .filter_map(|n| files.iter().position(|f| f == *n))
            .min();
        let last_core = cores
            .iter()
            .filter_map(|c| files.iter().position(|f| f == *c))
            .max();
        if let (Some(good), Some(bad)) = (last_core, first_noise) {
            assert!(
                good < bad,
                "ORDER services must not outrank STAT/SQL cores: {files:?}"
            );
        }
        let top6: Vec<&str> = files.iter().take(6).map(|s| s.as_str()).collect();
        let top6_cores = cores.iter().filter(|c| top6.contains(c)).count();
        assert!(
            top6_cores >= 4,
            "top 6 must be STAT/SQL cores, got {top6:?}"
        );
    }

    #[test]
    fn test_codeintel_2_0_jinke_and_sanshui_search() {
        let mut graph = CodeGraph::new();

        // 1. 金客业务控制器/服务
        let jinke_node = SymbolNode {
            id: 201,
            name: "CustomerIntroAttachmentController".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from("sources/ERP.API/Apis/Ailai.Customer/Controllers/CustomerIntroAttachmentController.cs"),
            start_line: 20,
            end_line: 200,
            signature: Some("public class CustomerIntroAttachmentController : ControllerBase".to_string()),
            docstring: Some("/// 金客赠品跟进考核所需客户侧数据".to_string()),
            inline_comments: vec!["// 获取金客赠品跟进考核所需客户侧数据".to_string()],
            comments: vec![
                super::super::graph::StructuredComment {
                    text: "/// 金客赠品跟进考核所需客户侧数据".to_string(),
                    scope: super::super::graph::CommentScope::Docstring,
                    line: 19,
                },
                super::super::graph::StructuredComment {
                    text: "// 获取金客赠品跟进考核所需客户侧数据".to_string(),
                    scope: super::super::graph::CommentScope::BranchInline {
                        branch_kind: "if_condition".to_string(),
                    },
                    line: 175,
                },
            ],
            sql_predicates: vec![],
            string_literals: vec!["JinkeGiftFollowAssessment".to_string()],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 5,
                branch_count: 4,
                has_sql_or_qs: false,
                is_pure_dto: false,
                is_active_logic: true,
            },
        };
        graph.add_symbol(jinke_node.clone());

        // 2. 三水项目分配服务
        let sanshui_node = SymbolNode {
            id: 202,
            name: "CustomerBaseData".to_string(),
            kind: SymbolKind::Class,
            visibility: super::super::graph::Visibility::Public,
            file: PathBuf::from(
                "sources/ERP.API/Apis/Ailai.Customer/Models/Data/CustomerBaseData.cs",
            ),
            start_line: 50,
            end_line: 4000,
            signature: Some("public class CustomerBaseData : ICustomerBaseData".to_string()),
            docstring: Some("/// 客户资源分配与三水项目二调统计".to_string()),
            inline_comments: vec![
                "// 三水项目工号分配与二调流转".to_string(),
                "case ProjectTraceRequest.PageTypeEnum.三水一调:".to_string(),
            ],
            comments: vec![super::super::graph::StructuredComment {
                text: "// 三水项目工号分配与二调流转".to_string(),
                scope: super::super::graph::CommentScope::BranchInline {
                    branch_kind: "switch_case".to_string(),
                },
                line: 2756,
            }],
            sql_predicates: vec![super::super::graph::SqlPredicate {
                raw_clause: "WHERE o.CustomerId = cb.CustomerId AND o.OrderSource <> 9".to_string(),
                target_fields: vec!["CustomerId".to_string(), "OrderSource".to_string()],
                line: 677,
            }],
            string_literals: vec!["三水一调".to_string(), "三水二调".to_string()],
            metrics: super::super::graph::AstMetrics {
                cyclomatic_complexity: 12,
                branch_count: 10,
                has_sql_or_qs: true,
                is_pure_dto: false,
                is_active_logic: true,
            },
        };
        graph.add_symbol(sanshui_node.clone());

        let dt = super::super::bilingual_nlp::DynamicThesaurus::new();
        let project_tokens = HashSet::new();
        let bm25_scores = HashMap::new();

        // 搜索 "金客赠品跟进"
        let q1 = "金客赠品跟进";
        let tokens1 = super::super::bilingual_nlp::parse_bilingual_query_with_thesaurus(q1, &dt);
        let parsed_q1 = super::super::bilingual_nlp::parse_field_qualified_query(q1);
        let cands1 = score_workspace_symbols(
            &graph,
            &tokens1,
            &parsed_q1,
            &project_tokens,
            None,
            &bm25_scores,
            &tokens1.dense_vector,
            None,
        );
        assert!(!cands1.is_empty());
        assert_eq!(
            cands1[0].file, jinke_node.file,
            "搜索金客必须命中 CustomerIntroAttachmentController"
        );

        // 搜索 "三水一调 分配"
        let q2 = "三水一调 分配";
        let tokens2 = super::super::bilingual_nlp::parse_bilingual_query_with_thesaurus(q2, &dt);
        let parsed_q2 = super::super::bilingual_nlp::parse_field_qualified_query(q2);
        let cands2 = score_workspace_symbols(
            &graph,
            &tokens2,
            &parsed_q2,
            &project_tokens,
            None,
            &bm25_scores,
            &tokens2.dense_vector,
            None,
        );
        assert!(!cands2.is_empty());
        assert_eq!(
            cands2[0].file, sanshui_node.file,
            "搜索三水一调必须命中 CustomerBaseData"
        );
    }
}
