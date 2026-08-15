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

fn path_matches_scope(file_path: &Path, scope: &Path) -> bool {
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

        // Step 1: Score all symbols in the workspace
        let scored_files = score_workspace_symbols(
            &graph,
            &query_tokens,
            &parsed_query,
            &project_tokens,
            scope_path.as_deref(),
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
        let output = render_explore_output(
            &graph,
            &root,
            &a.query,
            &scored_files,
            &flow_spine,
            connected,
            max_files,
            scope_path.as_deref(),
        );

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
fn score_workspace_symbols(
    graph: &CodeGraph,
    tokens: &SearchTokens,
    parsed_query: &ParsedQuery,
    project_tokens: &HashSet<String>,
    scope: Option<&Path>,
) -> Vec<FileCandidate> {
    let mut file_map: HashMap<PathBuf, Vec<ScoredSymbol>> = HashMap::new();

    for (_id, node) in &graph.nodes {
        if let Some(sc) = scope {
            if !path_matches_scope(&node.file, sc) {
                continue;
            }
        }

        // Apply field-qualified filters
        if !parsed_query.kind_filters.is_empty() {
            let kind_str = format!("{:?}", node.kind).to_ascii_lowercase();
            if !parsed_query.kind_filters.iter().any(|k| kind_str.contains(k)) {
                continue;
            }
        }
        if !parsed_query.name_filters.is_empty() {
            if !parsed_query.name_filters.iter().any(|n| node.name.to_ascii_lowercase().contains(&n.to_ascii_lowercase())) {
                continue;
            }
        }
        if !parsed_query.path_filters.is_empty() {
            let f_lower = node.file.to_string_lossy().to_ascii_lowercase();
            if !parsed_query.path_filters.iter().any(|p| f_lower.contains(&p.to_ascii_lowercase())) {
                continue;
            }
        }

        // HARD SEMANTIC ANCHOR: Reject false-positive background noise from Dense Vector n-grams.
        if !has_genuine_match_anchor(tokens, node) {
            continue;
        }

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
            continue;
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

        let raw_score = (text_match + graph_mass * 1.0) * kind_weight;

        if raw_score >= 12.0 {
            file_map.entry(node.file.clone()).or_default().push(ScoredSymbol {
                node: node.clone(),
                total_score: raw_score,
                name_score: name_sim + name_bonus,
                doc_score: doc_sim,
                inline_score: inline_sim,
                graph_mass,
            });
        }
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

/// Collect directories NEAR the top-ranked hits: the hit's own directory, its
/// direct subdirectories, and its parent directory — WITHOUT listing individual
/// files (a directory can hold many files; the point is to point the model at
/// where to look next, not to enumerate filenames). Only directories that
/// actually contain indexed files are reported, and a `path:` scope is respected.
fn collect_adjacent_dirs(
    graph: &CodeGraph,
    top_files: &[FileCandidate],
    scope: Option<&Path>,
    max_adjacent: usize,
) -> Vec<PathBuf> {
    // Directories that contain indexed files (respecting scope).
    let mut indexed_dirs: HashSet<PathBuf> = HashSet::new();
    for file in graph.file_symbols.keys() {
        if let Some(sc) = scope {
            if !path_matches_scope(file, sc) {
                continue;
            }
        }
        if let Some(dir) = file.parent() {
            indexed_dirs.insert(dir.to_path_buf());
        }
    }

    let mut near_dirs: HashSet<PathBuf> = HashSet::new();
    for fc in top_files {
        let Some(dir) = fc.file.parent() else { continue };
        // The hit's own directory.
        if indexed_dirs.contains(dir) {
            near_dirs.insert(dir.to_path_buf());
        }
        // Direct subdirectories of the hit's directory.
        for d in &indexed_dirs {
            if d.parent().is_some_and(|p| p == dir) {
                near_dirs.insert(d.clone());
            }
        }
        // The hit's parent directory (sibling modules one level up).
        if let Some(parent) = dir.parent() {
            if indexed_dirs.contains(parent) {
                near_dirs.insert(parent.to_path_buf());
            }
        }
    }

    let mut out: Vec<PathBuf> = near_dirs.into_iter().collect();
    out.sort();
    out.truncate(max_adjacent);
    out
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

            let sent_list = session_spans.entry(rel_path.clone()).or_default();

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

    // 📁 Adjacent DIRECTORIES near the top hits: the hit's own directory, its
    // direct subdirectories, and its parent directory. Directories only — a dir
    // can hold many files; the point is to point where to look next, not to
    // enumerate filenames.
    const MAX_ADJACENT_DIRS: usize = 16;
    let adjacent = collect_adjacent_dirs(graph, top_files, scope, MAX_ADJACENT_DIRS);
    if !adjacent.is_empty() {
        out.push("\n> 📁 **Adjacent directories** (hit's dir + nearby sub/parent dirs — likely related, NOT matched by your query; explore these next):".to_string());
        let listed_adj: Vec<String> = adjacent
            .iter()
            .map(|d| {
                let rel = d.strip_prefix(root).unwrap_or(d).display().to_string();
                format!("`{rel}/`")
            })
            .collect();
        out.push(format!("> {}", listed_adj.join(" ")));
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
        adjacent = adjacent.len(),
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
        let out = render_explore_output(&graph, &root, "coverage probe", &candidates, &[], false, 8, None);
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
        let out = render_explore_output(&graph, &root, "q", &candidates, &flow_spine, true, 8, None);
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
        let no_contract = render_explore_output(&graph, &root, "q", &func_candidates, &[], false, 8, None);
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
        let out = render_explore_output(&graph, &root, "repair_tool_args", &candidates, &[], false, 8, None);
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


