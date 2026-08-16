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
    calculate_text_similarity, derive_project_name_tokens, parse_bilingual_query_with_thesaurus,
    parse_field_qualified_query, DynamicThesaurus, ParsedQuery, SearchTokens,
};
use super::graph::{CodeGraph, EdgeKind, SymbolId, SymbolKind, SymbolNode};
use super::index::CodeIndex;
use super::{canonical, err, ok};
use async_trait::async_trait;
use atomcode_kernel::tool::{Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const DEFAULT_MAX_FILES: usize = 12;
const MAX_ALLOWED_FILES: usize = 30;

/// How many flow-spine edges the mermaid diagram draws before falling back to a
/// compact omitted-name list. The spine is ALWAYS computed in full (no per-edge
/// truncation in `extract_flow_spine`); this only caps the drawing, and the
/// Coverage summary reports shown/total so nothing is silently dropped.
const MAX_SPINE_EDGES: usize = 32;

/// Thread-safe session-level sent code ranges to avoid duplicate context bloat.
static SESSION_SENT_SPANS: std::sync::LazyLock<RwLock<HashMap<String, Vec<(usize, usize)>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

pub struct CodeExploreTool {
    index: Arc<CodeIndex>,
    thesaurus: Arc<RwLock<DynamicThesaurus>>,
}

/// Process-wide query-result cache, keyed by (graph fingerprint, query, scope,
/// max_files). `max_files` is part of the key because it directly changes the
/// rendered output (how many candidate files are expanded vs. listed in
/// Remaining) — omitting it would serve a 30-file render to a 12-file request.
/// Reuses the rendered output verbatim when the same query runs against the
/// same graph snapshot with the same rendering params (30-40 concurrent
/// read-heavy sessions asking similar questions). The fingerprint changes when
/// files change, so results never go stale. Bounded by a max-entry cap.
static QUERY_RESULT_CACHE: std::sync::LazyLock<RwLock<std::collections::HashMap<(u64, String, String, usize), String>>> =
    std::sync::LazyLock::new(|| RwLock::new(std::collections::HashMap::new()));
const QUERY_CACHE_MAX_ENTRIES: usize = 128;

fn query_cache_get(fingerprint: u64, query: &str, scope: &str, max_files: usize) -> Option<String> {
    let guard = QUERY_RESULT_CACHE.read().unwrap();
    guard
        .get(&(fingerprint, query.to_string(), scope.to_string(), max_files))
        .cloned()
}

fn query_cache_insert(fingerprint: u64, query: &str, scope: &str, max_files: usize, output: String) {
    let mut guard = QUERY_RESULT_CACHE.write().unwrap();
    if guard.len() >= QUERY_CACHE_MAX_ENTRIES {
        // Simple FIFO eviction: drop the oldest entry.
        if let Some(oldest) = guard.keys().next().cloned() {
            guard.remove(&oldest);
        }
    }
    guard.insert((fingerprint, query.to_string(), scope.to_string(), max_files), output);
}

impl CodeExploreTool {
    pub fn new(index: Arc<CodeIndex>) -> Self {
        let mut dt = DynamicThesaurus::new();
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

#[derive(Deserialize)]
struct Args {
    query: String,
    #[serde(default)]
    max_files: Option<usize>,
    #[serde(default)]
    path: Option<String>,
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

fn normalize_path_for_match(p: &Path) -> String {
    let s = p.to_string_lossy();
    let stripped = if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest
    } else {
        &s
    };
    stripped.replace('/', "\\").to_ascii_lowercase()
}

pub(crate) fn path_matches_scope(file_path: &Path, scope: &Path) -> bool {
    let f_norm = normalize_path_for_match(file_path);
    let sc_norm = normalize_path_for_match(scope);
    f_norm.starts_with(&sc_norm)
        || f_norm.ends_with(&sc_norm)
        || f_norm.contains(&sc_norm)
        || sc_norm.contains(&f_norm)
}

#[async_trait]
impl Tool for CodeExploreTool {
    fn name(&self) -> &str {
        "code_explore"
    }

    fn description(&self) -> &str {
        "WHEN TO USE — after a grep (or several parallel greps) surfaces promising symbol names, or \
         directly for 'how does X work' / 'trace flow X→Y' / 'find the bug in Z' / 'what calls F'. \
         One call returns the top-ranked files with verbatim line-numbered source (safe to edit from) \
         plus each symbol's callers/callees — the full call-graph panorama around a symbol.\n\
         \n\
         ALTERNATE PARALLEL BATCHES — never a one-by-one crawl (several calls per phase, then advance):\n\
         1. Structure first: list_directory + repo_map (never skip on an unfamiliar repo).\n\
         2. Hunt: fire SEVERAL greps IN PARALLEL for candidate symbols.\n\
         3. Panorama: feed those names to SEVERAL code_explore calls IN PARALLEL (one per symbol).\n\
         4. Zoom: read_file only the specific hot spans (folded/large bodies) you now know matter.\n\
         5. If the panorama is incomplete (Coverage shows omissions, or a sibling layer is likely), \
         loop back to step 2 with new greps — alternate grep-batches and code-batches until covered.\n\
         \n\
         CAUTION: this is a ranked & budgeted view (max_files cap, per-file symbol cap, folded spans, \
         session dedup) — NOT the complete file set. A 0-hit or thin result is INCONCLUSIVE: retry with \
         synonyms / English terms / a broader path, or grep more — never conclude a feature is absent \
         from one call alone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language question in Chinese or English (e.g. '扣减库存并加锁防超卖', 'how does auth middleware verify JWT') OR symbol/file names (e.g. 'OrderService verifyToken')."
                },
                "max_files": {
                    "type": "integer",
                    "description": "Maximum number of files to render source code from (default: 12, max: 30)"
                },
                "path": {
                    "type": "string",
                    "description": "Optional subdirectory or file scope to narrow search"
                }
            },
            "required": ["query"]
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

        let root = canonical(&ctx.working_dir);
        let max_files = a.max_files.unwrap_or(DEFAULT_MAX_FILES).clamp(1, MAX_ALLOWED_FILES);

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

        let graph = self.index.get(&root);

        let scope_path = if !parsed_query.path_filters.is_empty() {
            let p = &parsed_query.path_filters[0];
            if Path::new(p).is_absolute() {
                Some(PathBuf::from(p))
            } else {
                Some(root.join(p))
            }
        } else {
            a.path.as_deref().map(|p| {
                if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    root.join(p)
                }
            })
        };

        // Step 1: Score all symbols in the workspace.
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
        let scored_files = score_workspace_symbols(
            &graph,
            &query_tokens,
            &parsed_query,
            &project_tokens,
            scope_path.as_deref(),
            &bm25_scores,
            &query_concept_vec,
            self.index.get_concept_vectors(&root).as_deref(),
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
                        format!("* Disk Inspection: {} source file(s) on disk {:?}.\n", disk_files, ext_list)
                    }
                } else {
                    format!("* ⚠️ Disk Inspection: Specified path `{}` does not exist on disk.\n", sc.display())
                }
            } else {
                String::new()
            };

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

        // Step 3: Render output (Mode A: Connected Flow vs Mode B: Ranked Fallback)
        // Query-result cache: identical (fingerprint, query, scope) hits reuse
        // the rendered output verbatim — concurrent read-heavy sessions asking
        // the same question skip re-scoring entirely. Fingerprint changes on
        // file edits, so results never go stale.
        let fp = self.index.fingerprint(&root).unwrap_or(0);
        let scope_key = scope_path.as_deref().map(|s| s.display().to_string()).unwrap_or_default();
        if let Some(cached) = query_cache_get(fp, &a.query, &scope_key, max_files) {
            return ok(cached);
        }
        let dirindex = self.index.get_dirindex(&root);
        let output = render_explore_output(
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
        );
        query_cache_insert(fp, &a.query, &scope_key, max_files, output.clone());

        ok(output)
    }
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
            if node_name_lower == id_lower || (node_name_lower.contains(&id_lower) && id_lower.len() >= 4) {
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

    false
}

/// Score all symbols across files using multi-field similarity & graph topology.
///
/// `bm25_scores` is the opt-in BM25 lexical recall (symbol id → raw BM25 score).
/// Two effects:
/// 1. SOFT-ANCHOR: a symbol that fails the semantic-anchor gate is NOT dropped —
///    it gets a 0.3 decay instead, so naming-plain core files (run_loop.rs /
///    turn.rs / tool_calls.rs) still reach the corpus; pure noise is filtered
///    by the `text_match` threshold below (relevance floor, separate from the
///    semantic gate).
/// 2. RRF-FUSION: the BM25 score contributes a bounded bonus (0..25) to the
///    final raw score, so lexical recall is a real ranking signal, not just a
///    rescue set.
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

    // Parallel scoring: the graph is read-only, so every symbol scores
    // independently (rayon work-stealing across cores); collected hits are
    // merged into per-file buckets afterwards — the 34万-symbol scan drops
    // from serial-seconds to parallel-milliseconds on multi-core hosts.
    let scored: Vec<(PathBuf, ScoredSymbol)> = graph
        .nodes
        .par_iter()
        .filter_map(|(_id, node)| {
        if let Some(sc) = scope {
            if !path_matches_scope(&node.file, sc) {
                return None;
            }
        }

        // Apply field-qualified filters
        if !parsed_query.kind_filters.is_empty() {
            let kind_str = format!("{:?}", node.kind).to_ascii_lowercase();
            if !parsed_query.kind_filters.iter().any(|k| kind_str.contains(k)) {
                return None;
            }
        }
        if !parsed_query.name_filters.is_empty() {
            if !parsed_query.name_filters.iter().any(|n| node.name.to_ascii_lowercase().contains(&n.to_ascii_lowercase())) {
                return None;
            }
        }
        if !parsed_query.path_filters.is_empty() {
            let f_lower = node.file.to_string_lossy().to_ascii_lowercase();
            if !parsed_query.path_filters.iter().any(|p| f_lower.contains(&p.to_ascii_lowercase())) {
                return None;
            }
        }

        // SOFT SEMANTIC ANCHOR: symbols failing the genuine-anchor gate are NOT
        // dropped — they get a 0.3 decay so naming-plain core files (run_loop.rs /
        // turn.rs / tool_calls.rs) still reach the corpus. BM25 lexical hits skip
        // the decay (a word-level match is genuine relevance).
        let anchor_decay = if bm25_scores.contains_key(&node.id) {
            1.0
        } else if has_genuine_match_anchor(tokens, node) {
            1.0
        } else {
            0.3
        };

        let name_sim = calculate_text_similarity(tokens, &node.name);
        let mut name_bonus = 0.0;
        let node_name_lower = node.name.to_ascii_lowercase();

        // Exact query match (highest priority)
        if tokens.raw_query.eq_ignore_ascii_case(&node.name) {
            name_bonus += 100.0;
        }

        // Exact identifier token match
        for id in &tokens.code_identifiers {
            if id.eq_ignore_ascii_case(&node.name) {
                name_bonus += if *id == node.name { 70.0 } else { 50.0 };
            }
        }

        // Bilingual thesaurus term hit in symbol name
        if tokens.expanded_terms.iter().any(|term| node_name_lower == *term || node_name_lower.contains(term)) {
            name_bonus += 30.0;
        }

        // Project name de-inflation: if symbol name is just the repo name itself, de-prioritize
        if project_tokens.contains(&node_name_lower) && !tokens.raw_query.eq_ignore_ascii_case(&node.name) {
            name_bonus *= 0.2;
        }

        let doc_sim = node
            .docstring
            .as_ref()
            .map(|d| calculate_text_similarity(tokens, d))
            .unwrap_or(0.0);
        let inline_sim = node
            .inline_comments
            .iter()
            .map(|c| calculate_text_similarity(tokens, c))
            .fold(0.0f64, f64::max);

        let path_sim = calculate_text_similarity(tokens, &node.file.to_string_lossy());

        let text_match = (name_sim + name_bonus) * 0.50 + doc_sim * 0.25 + inline_sim * 0.15 + path_sim * 0.10;
        if text_match < 8.0 {
            return None;
        }

        let callers_cnt = graph.callers(node.id).map(|v| v.len()).unwrap_or(0);
        let callees_cnt = graph.callees(node.id).map(|v| v.len()).unwrap_or(0);
        let graph_mass = ((callers_cnt + callees_cnt) as f64).min(10.0);

        let kind_weight = match node.kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Middleware | SymbolKind::RouteEndpoint => 1.0,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait => 0.95,
            SymbolKind::PluginDeclaration => 1.05,
            SymbolKind::SqlStatement => 0.95,
            SymbolKind::ConfigProperty | SymbolKind::UiElement => 0.85,
            SymbolKind::Constant | SymbolKind::Variable => 0.75,
            _ => 0.7,
        };

        // RRF-style BM25 fusion: normalized BM25 score contributes a bounded 0..25
        // bonus (position-based weighting, so lexical recall is a real ranking
        // signal rather than a rescue set).
        let bm25_bonus = if bm25_max > 0.0 {
            let norm = bm25_scores.get(&node.id).copied().unwrap_or(0.0) / bm25_max;
            norm * 25.0
        } else {
            0.0
        };

        // Semantic concept-vector path (opt-in, ATOMCODE_EXPLORE_CONCEPT=1):
        // Chinese query ↔ English code cosine similarity, bounded 0..20 bonus.
        // Symbol vectors come from the CodeIndex-shared cache when available
        // (built once for the whole graph), else computed per-symbol.
        let concept_bonus = if !query_concept_vec.is_empty() {
            let sim = match concept_vectors.and_then(|m| m.get(&node.id)) {
                Some(v) => super::retrieval::concept_cosine(query_concept_vec, v),
                None => {
                    let node_vec =
                        super::retrieval::concept_projection(&node.name, &HashSet::new());
                    super::retrieval::concept_cosine(query_concept_vec, &node_vec)
                }
            };
            sim * 20.0
        } else {
            0.0
        };

        // anchor_decay × text_match, plus BM25/概念向量 bonus, times kind weight.
        let raw_score =
            (text_match * anchor_decay + graph_mass * 1.0 + bm25_bonus + concept_bonus) * kind_weight;

        if raw_score >= 12.0 {
            Some((
                node.file.clone(),
                ScoredSymbol {
                    node: node.clone(),
                    total_score: raw_score,
                    name_score: name_sim + name_bonus,
                    doc_score: doc_sim,
                    inline_score: inline_sim,
                    graph_mass,
                },
            ))
        } else {
            None
        }
        })
        .collect();

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

    // Sort production files ahead of test files and peripheral scripts
    candidates.sort_by(|a, b| {
        b.top_score.partial_cmp(&a.top_score).unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Trace flow spine bidirectionally from the scored seed symbols.
///
/// COMPLETENESS CONTRACT: the spine is a true panorama, not a sample —
/// every scored candidate symbol is a seed, and for each seed EVERY caller /
/// callee within the hop budget is traced (no `.take()` truncation). The
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

/// renderer may still cap the mermaid drawing, but it reports the omitted
/// count/names via the Coverage line and the omitted-edge list.
fn extract_flow_spine(
    graph: &CodeGraph,
    candidates: &[FileCandidate],
) -> (Vec<(SymbolId, Option<SymbolId>, EdgeKind)>, bool) {
    // Seeds: every scored symbol in every candidate file (all already passed the
    // >= 12 workspace bar), so a hit in a 5th-place symbol of a file is not
    // starved of its own caller/callee panorama.
    let mut seeds = Vec::new();
    for fc in candidates {
        for s in &fc.symbols {
            if s.total_score >= 12.0 {
                seeds.push(s.node.id);
            }
        }
    }

    if seeds.is_empty() {
        return (Vec::new(), false);
    }

    let mut spine_edges = Vec::new();
    let mut visited = HashSet::new();

    for &seed in &seeds {
        visited.insert(seed);
        // Trace Callees (forward 2 hops) — ALL workspace-internal ones.
        if let Some(callees) = graph.callees(seed) {
            for e in callees {
                if visited.insert(e.to) && is_workspace_symbol(graph, e.to) {
                    spine_edges.push((seed, Some(e.to), e.kind.clone()));
                    if let Some(sub_callees) = graph.callees(e.to) {
                        for sub_e in sub_callees {
                            if visited.insert(sub_e.to) && is_workspace_symbol(graph, sub_e.to) {
                                spine_edges.push((e.to, Some(sub_e.to), sub_e.kind.clone()));
                            }
                        }
                    }
                }
            }
        }
        // Trace Callers (backward 1 hop) — ALL workspace-internal ones.
        if let Some(callers) = graph.callers(seed) {
            for e in callers {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

/// 每组输出的目录行上限;超出折叠成 `… 及 N 个`(带计数,不丢弃)。
const MAX_DIRS_PER_GROUP: usize = 15;

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
        let Some(dir) = fc.file.parent() else { continue };
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
                    let related = query_terms
                        .iter()
                        .any(|t| t.len() >= 3 && (name_lower.contains(t) || t.contains(&name_lower)));
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
                let ratio = if total > 0 { anchored as f64 / total as f64 } else { 0.0 };
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
        a.group
            .cmp(&b.group)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
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
    let active = if symbols_in_dir(graph, dir).is_empty() { 0.8 } else { 1.0 };

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

/// Render formatted explore output with multi-symbol extraction and merged spans.
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
) -> String {
    let mut out = Vec::new();
    let top_files = &candidates[..candidates.len().min(max_files)];

    out.push("> 💡 **Surgical Flow & Capsule Notice**: Primary execution flow, candidate spans, and file capability capsules are rendered below. To ensure complete coverage of edge cases or declarative configs, examine the **File Capability Capsule** or adjacent files in the same directory.\n".to_string());

    // Scope + layered-architecture hint: a path-limited search is CONFINED to that
    // scope — an interface may live here while its implementations / middleware
    // registration live in sibling layers (crates/packages). Never conclude "the
    // project lacks X" from a scope-confined hit set.
    if let Some(sc) = scope {
        let scope_disp = sc.strip_prefix(root).unwrap_or(sc).display().to_string();
        out.push(format!(
            "> 🔭 **Search scope**: `{scope_disp}` (path-limited). Results are confined to this \
             scope — related code frequently lives in SIBLING layers (other crates/packages/dirs). \
             If the top hits are interfaces/traits (or you see only a bare contract), the \
             implementations are at `impl <name>` in other files: follow up with `code_explore` or \
             grep before concluding the mechanism is absent.\n"
        ));
    }

    // Trait/interface-hit hint: the top symbol being a contract is an ANCHOR-STRENGTHENING trap
    // (a bare `Tool::execute(&str) -> ToolResult{is_error}` can look like "weak error handling"
    // while the real logic lives in its impls). Surface the follow-up explicitly.
    {
        let has_contract = top_files.iter().take(4).any(|fc| {
            fc.symbols.iter().take(4).any(|s| {
                matches!(
                    s.node.kind,
                    SymbolKind::Trait | SymbolKind::Interface | SymbolKind::TypeAlias
                )
            })
        });
        if has_contract {
            out.push(
                "> 🧩 **Contract hit**: top results include a trait/interface/type-alias. This is the \
                 DECLARATION, not the behavior — implementations live at `impl <name>` elsewhere. \
                 Query `code_explore(\"impl <name>\")` or grep `impl <name>` to find every \
                 implementation before judging the mechanism.\n".to_string(),
            );
        }
    }

    // Low-hit hint: a thin hit set is a WEAK signal — never let 1-2 files look conclusive.
    // (The zero-hit branch already returned early with its own diagnostic; this covers 1..2.)
    if !candidates.is_empty() && candidates.len() < 3 {
        out.push(
            "> 🎯 **Low hit count** (<3 candidate files): this is a WEAK match signal, not a \
             confirmation. Run 2-3 more `code_explore` queries IN PARALLEL with synonyms / \
             English terms / a narrower `name:` filter / a wider `path:` scope before drawing \
             any conclusion — ranked retrieval can hide relevant code below its thresholds.\n"
                .to_string(),
        );
    }

    if connected {
        out.push(format!("### 🔗 Flow Trace: \"{query}\"\n"));
        out.push("```mermaid".to_string());
        out.push("graph TD".to_string());
        for (from, to, kind) in flow_spine.iter().take(MAX_SPINE_EDGES) {
            let from_name = graph.node(*from).map(|n| n.name.as_str()).unwrap_or("unknown");
            if let Some(target) = to {
                let to_name = graph.node(*target).map(|n| n.name.as_str()).unwrap_or("unknown");
                let label = match kind {
                    EdgeKind::Calls => "calls",
                    EdgeKind::HttpDispatches => "http_post",
                    EdgeKind::MapperBinds => "mapper_sql",
                    EdgeKind::ConfigBinds => "config_bind",
                    _ => "refs",
                };
                out.push(format!("    {from_name} -->|{label}| {to_name}"));
            }
        }
        if flow_spine.len() > MAX_SPINE_EDGES {
            let omitted_count = flow_spine.len() - MAX_SPINE_EDGES;
            let mut omitted_names: Vec<String> = Vec::new();
            for (from, to, _kind) in flow_spine.iter().skip(MAX_SPINE_EDGES) {
                let from_name = graph.node(*from).map(|n| n.name.as_str()).unwrap_or("unknown");
                if let Some(target) = to {
                    let to_name = graph.node(*target).map(|n| n.name.as_str()).unwrap_or("unknown");
                    omitted_names.push(format!("{from_name} → {to_name}"));
                }
            }
            let shown: Vec<String> = omitted_names.iter().take(10).cloned().collect();
            let more = omitted_names.len().saturating_sub(10);
            let mut line = format!(
                "    %% ... {} additional edge(s) omitted: {}",
                omitted_count,
                shown.join(", ")
            );
            if more > 0 {
                line.push_str(&format!(", … and {more} more"));
            }
            out.push(line);
        }
        out.push("```\n".to_string());
    } else {
        out.push(format!("### 🏆 Top Ranked Relevant Files & Symbols for: \"{query}\"\n"));
        out.push("> ℹ️ No multi-hop continuous flow detected. Displaying top-scored weighted symbols & code blocks below:\n".to_string());
    }

    // Top Matched Candidates Overview Table
    out.push("#### 📋 Matched Symbol Candidates:".to_string());
    out.push("| Score | File | Symbols & Lines |".to_string());
    out.push("| :--- | :--- | :--- |".to_string());

    for fc in top_files {
        let rel_path = fc.file.strip_prefix(root).unwrap_or(&fc.file).display().to_string();
        let sym_summary: Vec<String> = fc
            .symbols
            .iter()
            .take(4)
            .map(|s| format!("`{}`:L{} {}", s.node.name, s.node.start_line, symbol_match_signal(s)))
            .collect();
        out.push(format!(
            "| **{:.1}** | `{rel_path}` | {} |",
            fc.top_score,
            sym_summary.join(", ")
        ));
    }
    out.push("".to_string());

    // Coverage accounting — surfaced in the leading 📊 summary so the model knows this is a
    // ranked/budgeted view, not an exhaustive listing.
    let mut folded_spans: usize = 0;
    let mut folded_lines: usize = 0;
    let mut omitted_symbols: usize = 0;
    for fc in top_files {
        omitted_symbols += fc.symbols.len().saturating_sub(4);
    }

    let mut session_spans = SESSION_SENT_SPANS.write().unwrap();

    for fc in top_files {
        let rel_path = fc.file.strip_prefix(root).unwrap_or(&fc.file).display().to_string();

        // Collect all high-scoring symbols for this file (up to 4 per file)
        let relevant_syms: Vec<&ScoredSymbol> = fc
            .symbols
            .iter()
            .take(4)
            .filter(|s| s.total_score >= 15.0 || s.name_score >= 25.0)
            .collect();
        let relevant_syms = if relevant_syms.is_empty() {
            vec![&fc.symbols[0]]
        } else {
            relevant_syms
        };

        let sym_names: Vec<String> = relevant_syms.iter().map(|s| format!("`{}`", s.node.name)).collect();
        out.push(format!("**`{rel_path}`** — {}", sym_names.join(", ")));

        // Output File Capability Capsule
        let capsule = graph.file_capsule(&fc.file);
        if !capsule.is_empty() {
            out.push("> 📋 **File Capability Capsule**:".to_string());
            for cap_line in capsule {
                out.push(format!("> - {cap_line}"));
            }
            out.push("".to_string());
        }

        if let Ok(content) = std::fs::read_to_string(&fc.file) {
            let lines: Vec<&str> = content.lines().collect();

            // Build line spans for each relevant symbol
            let mut spans: Vec<(usize, usize, String)> = Vec::new();
            for s in &relevant_syms {
                let start = s.node.start_line.saturating_sub(2).max(1);
                let end = (s.node.end_line + 3).min(lines.len());
                spans.push((start, end, format!("{}:L{}", s.node.name, s.node.start_line)));
            }

            // Merge overlapping or adjacent spans
            spans.sort_by_key(|s| s.0);
            let mut merged_spans: Vec<(usize, usize, Vec<String>)> = Vec::new();
            for (st, en, label) in spans {
                if let Some(last) = merged_spans.last_mut() {
                    if st <= last.1 + 4 {
                        last.1 = last.1.max(en);
                        last.2.push(label);
                        continue;
                    }
                }
                merged_spans.push((st, en, vec![label]));
            }

            // Dedup key must be root-scoped: the same relative path (e.g.
            // `src/main.rs`) exists in many projects — without the root prefix
            // a multi-repo workspace would wrongly suppress spans across
            // projects (project A's sent span hides project B's identical path).
            let sent_list = session_spans
                .entry(format!("{}|{rel_path}", root.display()))
                .or_default();

            for (start_line, end_line, labels) in merged_spans {
                let already_sent = sent_list.iter().any(|(s, e)| *s <= start_line && end_line <= *e);

                if already_sent {
                    out.push(format!("> `[Same range already returned earlier this session (dedup): {rel_path} L{start_line}-L{end_line} ({}) — content omitted here; use read_file if you need it again]`\n", labels.join(", ")));
                } else {
                    sent_list.push((start_line, end_line));
                    let total_span = end_line.saturating_sub(start_line) + 1;
                    let mut snippet = Vec::new();

                    if total_span > 70 {
                        // Smart Folding for large functions (keep head 35 lines + tail 15 lines)
                        folded_spans += 1;
                        let head_end = (start_line + 35).min(lines.len());
                        for l in start_line..=head_end {
                            if l - 1 < lines.len() {
                                snippet.push(format!("{l}\t{}", lines[l - 1]));
                            }
                        }
                        let tail_start = end_line.saturating_sub(15).max(head_end + 1);
                        let folded_count = tail_start.saturating_sub(head_end + 1);
                        folded_lines += folded_count;
                        if folded_count > 0 {
                            snippet.push(format!("...\t// ... [{} lines folded; use read_file(\"{rel_path}\", start_line={}, end_line={}) to view full body] ...", folded_count, head_end + 1, tail_start - 1));
                        }
                        for l in tail_start..=end_line {
                            if l - 1 < lines.len() {
                                snippet.push(format!("{l}\t{}", lines[l - 1]));
                            }
                        }
                    } else {
                        for l in start_line..=end_line {
                            if l - 1 < lines.len() {
                                snippet.push(format!("{l}\t{}", lines[l - 1]));
                            }
                        }
                    }

                    let ext = fc.file.extension().and_then(|e| e.to_str()).unwrap_or("");
                    out.push(format!("// Symbols: {}\n```{ext}\n{}\n```\n", labels.join(", "), snippet.join("\n")));
                }
            }
        }
    }

    // Remaining candidate files — FULL PATH LISTING (no truncation): every
    // scored-but-not-rendered candidate path is listed (with its top symbols so
    // the model can pick which to open without a blind read), so the model can
    // see the complete hit set, not just the top-ranked slice.
    if candidates.len() > max_files {
        out.push("\n**All Remaining Candidate Files (full paths, not rendered):**".to_string());
        for c in &candidates[max_files..] {
            let rel = c.file.strip_prefix(root).unwrap_or(&c.file).display().to_string();
            let sym_summary: Vec<String> = c
                .symbols
                .iter()
                .take(3)
                .map(|s| format!("{}:L{}", s.node.name, s.node.start_line))
                .collect();
            if sym_summary.is_empty() {
                out.push(format!("- `{rel}`"));
            } else {
                out.push(format!("- `{rel}` — {}", sym_summary.join(", ")));
            }
        }
    }

    // 📁 相邻目录全景:六类分组(锚定/子树/父链/兄弟/图连通/路径词命中),带分 + grep 提示。
    let dirs = collect_directory_panorama(graph, top_files, tokens, scope, dirindex);
    if !dirs.is_empty() {
        out.push("\n> 📁 **Directory Panorama** (six groups, scored + grep fallback):".to_string());
        let mut last_group: Option<DirGroup> = None;
        let mut group_count = 0usize;
        let mut group_overflow = 0usize;
        for d in &dirs {
            if last_group != Some(d.group) {
                if let Some(_) = last_group {
                    if group_overflow > 0 {
                        out.push(format!(">   … 及 {group_overflow} 个同级目录(带计数,不丢弃)"));
                    }
                    group_overflow = 0;
                }
                out.push(format!("> **{}**", d.group.label()));
                last_group = Some(d.group);
                group_count = 0;
            }
            if group_count >= MAX_DIRS_PER_GROUP {
                group_overflow += 1;
                continue;
            }
            group_count += 1;
            let rel = d.path.strip_prefix(root).unwrap_or(&d.path).display().to_string();
            let hits_desc = if d.hits.is_empty() {
                "(未命中,结构相关)".to_string()
            } else {
                let hs: Vec<String> = d
                    .hits
                    .iter()
                    .take(3)
                    .map(|(f, s)| {
                        let rf = f.strip_prefix(root).unwrap_or(f).display().to_string();
                        format!("{}({:.1})", rf, s)
                    })
                    .collect();
                hs.join(", ")
            };
            let grep = if d.grep_terms.is_empty() {
                String::new()
            } else {
                format!(" └ grep: `grep -rn \"{}\" {rel}/`", d.grep_terms.join("|"))
            };
            out.push(format!(
                "> | {:.1} | `{rel}/` | {}/{} 文件锚定 · peak {:.1} | {hits_desc} |{grep}",
                d.score, d.anchored_files, d.total_files, d.peak_file_score
            ));
        }
        if group_overflow > 0 {
            out.push(format!(">   … 及 {group_overflow} 个同级目录(带计数,不丢弃)"));
        }
    }

    out.push("\n> 🔍 **Deep Investigation Tip**: To inspect any folded function body, use `read_file` with the line numbers provided above. To trace cross-module callers/callees in full detail, supply the exact symbol or file path to `code_explore`.\n".to_string());

    // Leading 📊 Coverage summary — inserted right after the opening notice so the model sees the
    // shown/total split BEFORE the ranked content (ranked ≠ exhaustive). Counts are always
    // informative: a "showing 8/47" line is what stops "the project lacks X" hallucinations.
    // Remaining candidates are FULLY listed at the bottom (no hidden set anymore).
    let remaining = candidates.len().saturating_sub(max_files);
    let spine_total = flow_spine.len();
    let spine_shown = spine_total.min(MAX_SPINE_EDGES);

    // Scope-vs-workspace contrast: when a `path:` scope confines the search, the
    // model must see how much of the workspace was NOT searched — a kernel-only
    // query says nothing about sibling-layer crates (the original blind spot:
    // `path: kernel` hid `repair.rs`/`tool_feedback.rs` entirely).
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
    let scope_note = match scope {
        Some(sc) => {
            let scope_disp = sc.strip_prefix(root).unwrap_or(sc).display().to_string();
            let outside_syms = workspace_syms.saturating_sub(scope_syms);
            format!(
                "(scope `{scope_disp}`: in-scope {scope_files} of {workspace_files} indexed files · \
                 {scope_syms}/{workspace_syms} symbols — {outside_syms} symbol(s) OUTSIDE this \
                 scope were NOT ranked (the index is workspace-wide; only this scope was scored); \
                 if the top hits are contracts, their implementations likely live in sibling layers)"
            )
        }
        None => format!("(all {workspace_files} indexed files · {workspace_syms} symbols)"),
    };

    let coverage = format!(
        "> 📊 **Coverage** {scope_note}: showing {shown}/{total} candidate file(s) (max_files={max_files}) · \
         {omitted} high-scoring symbol(s) omitted by the per-file cap · {folded} span(s) folded \
         ({folded_lines} lines) · flow spine {spine_shown}/{spine_total} edge(s) · \
         {remaining} remaining candidate file(s) — ALL listed with full paths below · \
         {adjacent} adjacent dir(s) in 📁. \
         Omitted/folded content and adjacent directories can still be relevant — \
         re-query with a narrower `path:`/`name:` filter or use `read_file` to inspect specific ranges.",
        shown = top_files.len(),
        total = candidates.len(),
        max_files = max_files,
        omitted = omitted_symbols,
        folded = folded_spans,
        folded_lines = folded_lines,
        spine_shown = spine_shown,
        spine_total = spine_total,
        remaining = remaining,
        adjacent = dirs.len(),
    );
    out.insert(1, coverage);

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_qualified_query_parser() {
        let q = parse_field_qualified_query("kind:trait path:session name:ToolMiddleware agent loop");
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
            docstring: None,
            inline_comments: Vec::new(),
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
            docstring: None,
            inline_comments: Vec::new(),
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
            docstring: None,
            inline_comments: Vec::new(),
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
                docstring: None,
                inline_comments: Vec::new(),
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
        let out = render_explore_output(&graph, &root, "coverage probe", &candidates, &[], false, 8, None, &SearchTokens::default(), None);
        assert!(out.contains("📊 **Coverage**"), "coverage summary missing:\n{out}");
        assert!(out.contains("showing 1/1 candidate file(s)"), "shown/total missing:\n{out}");
        assert!(out.contains("2 high-scoring symbol(s) omitted"), "omitted count missing:\n{out}");
        assert!(out.contains("flow spine 0/0 edge(s)"), "spine counts missing:\n{out}");
        assert!(out.contains("0 remaining candidate file(s)"), "remaining count missing:\n{out}");
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
                docstring: None,
                inline_comments: Vec::new(),
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
        // Fake an over-cap flow spine: 40 edges (32 shown, 8 omitted).
        let flow_spine: Vec<(u64, Option<u64>, super::super::graph::EdgeKind)> = (0..40u64)
            .map(|i| (i, Some(i + 1), super::super::graph::EdgeKind::Calls))
            .collect();
        let out = render_explore_output(&graph, &root, "q", &candidates, &flow_spine, true, 8, None, &SearchTokens::default(), None);
        assert!(out.contains("showing 8/15 candidate file(s)"), "shown/total:\n{out}");
        assert!(
            out.contains("7 remaining candidate file(s) — ALL listed with full paths below"),
            "remaining count:\n{out}"
        );
        assert!(
            out.contains("- `crates/kernel/src/hidden_8.rs`"),
            "remaining candidate full-path listing missing:\n{out}"
        );
        assert!(
            out.contains("- `crates/kernel/src/hidden_14.rs`"),
            "last remaining candidate full path missing:\n{out}"
        );
        assert!(out.contains("flow spine 32/40 edge(s)"), "spine overflow count:\n{out}");
        assert!(out.contains("%% ... 8 additional edge(s) omitted:"), "mermaid omission note:\n{out}");
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
            docstring: None,
            inline_comments: Vec::new(),
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
                docstring: None,
                inline_comments: Vec::new(),
            };
            graph.add_symbol(node.clone());
            caller_ids.push(node.id);
            graph.add_edge(node.id, Edge { to: target_id, kind: EdgeKind::Calls, line: 3 });
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
                docstring: None,
                inline_comments: Vec::new(),
            };
            graph.add_symbol(node.clone());
            callee_ids.push(node.id);
            graph.add_edge(target_id, Edge { to: node.id, kind: EdgeKind::Calls, line: 30 });
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
            assert!(spine_froms.contains(id), "caller {id} missing from spine:\n{spine:?}");
        }
        for id in &callee_ids {
            assert!(spine_tos.contains(id), "callee {id} missing from spine:\n{spine:?}");
        }
        assert!(spine_tos.contains(&target_id), "target missing as edge target:\n{spine:?}");
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
            docstring: None,
            inline_comments: Vec::new(),
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
        );
        assert!(out.contains("🔭 **Search scope**"), "scope hint missing:\n{out}");
        assert!(out.contains("SIBLING layers"), "sibling-layers hint missing:\n{out}");
        assert!(out.contains("🧩 **Contract hit**"), "contract hint missing:\n{out}");
        assert!(out.contains("`impl <name>`"), "impl-follow-up hint missing:\n{out}");
        assert!(out.contains("🎯 **Low hit count**"), "low-hit hint missing:\n{out}");
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
            docstring: None,
            inline_comments: Vec::new(),
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
        let no_contract = render_explore_output(&graph, &root, "q", &func_candidates, &[], false, 8, None, &SearchTokens::default(), None);
        assert!(!no_contract.contains("🧩 **Contract hit**"), "contract hint should not fire for a function:\n{no_contract}");
        assert!(no_contract.contains("🎯 **Low hit count**"), "low-hit should still fire:\n{no_contract}");
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
            docstring: None,
            inline_comments: Vec::new(),
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
            docstring: None,
            inline_comments: Vec::new(),
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
            docstring: None,
            inline_comments: Vec::new(),
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
        let out = render_explore_output(&graph, &root, "repair_tool_args", &candidates, &[], false, 8, None, &SearchTokens::default(), None);
        // 📁 now lists the hit's DIRECTORY (not sibling filenames): the tools/
        // dir must appear; the far kernel/src dir must not.
        assert!(
            out.contains("crates/capabilities/src/tools/"),
            "hit dir must be listed in 📁:\n{out}"
        );
        assert!(!out.contains("kernel/src/"), "far dir must NOT be listed:\n{out}");
        assert!(
            out.contains("1 adjacent dir(s) in 📁"),
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
            docstring: None,
            inline_comments: Vec::new(),
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
            docstring: None,
            inline_comments: Vec::new(),
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
        );
        // The Coverage line must surface the workspace contrast: only 1 of 2 files /
        // 1 of 2 symbols were searched, and 1 symbol is OUTSIDE the scope — so a
        // scope-confined "no mechanism" conclusion is invalid.
        assert!(out.contains("in-scope 1 of 2 indexed files"), "scope-vs-workspace file counts missing:\n{out}");
        assert!(out.contains("1/2 symbols"), "scope-vs-workspace symbol counts missing:\n{out}");
        assert!(
            out.contains("1 symbol(s) OUTSIDE this scope were NOT ranked"),
            "outside-scope warning missing:\n{out}"
        );
    }
}


