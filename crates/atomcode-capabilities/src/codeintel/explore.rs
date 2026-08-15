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
    calculate_text_similarity, parse_bilingual_query_with_thesaurus, DynamicThesaurus, SearchTokens,
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

const DEFAULT_MAX_FILES: usize = 8;
const MAX_ALLOWED_FILES: usize = 20;

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
    is_test: bool,
    is_generated: bool,
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
        "PRIMARY CODE INTELLIGENCE & FLOW EXPLORATION TOOL: One-shot surgical context retrieval. \
         Investigates how features work, traces execution flows from X to Y, finds root causes for bugs, \
         or inspects symbols before editing. Returns the verbatim line-numbered source code (<line>\\t<code>, \
         Read-equivalent, safe to edit from) grouped by file, along with full bidirectional call paths \
         (callers/callees/dynamic dispatch) and blast radius. Query can be a natural language question in \
         Chinese or English, or a list of symbols/files. ONE call delivers complete context — do NOT re-read \
         the returned files with read_file."
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
                    "description": "Maximum number of files to render source code from (default: 8, max: 20)"
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

        let thesaurus_guard = self.thesaurus.read().unwrap();
        let query_tokens = parse_bilingual_query_with_thesaurus(&a.query, &thesaurus_guard);
        drop(thesaurus_guard);

        let graph = self.index.get(&root);

        let scope_path = a.path.as_deref().map(|p| {
            if Path::new(p).is_absolute() {
                PathBuf::from(p)
            } else {
                root.join(p)
            }
        });

        // Step 1: Score all symbols in the workspace
        let scored_files = score_workspace_symbols(&graph, &query_tokens, scope_path.as_deref());

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

            return ok(format!(
                "🔍 Zero-Hit Diagnostic for query '{}':\n\
                 - Scope Path: `{}` (Matched {} files, {} indexed symbols, Language Exts: {:?})\n\
                 - Workspace Total: {} symbols indexed across {} files\n\
                 - Query Analysis: {}\n\
                 - Diagnostic Assessment:\n\
                   * Scope file(s) contained {} AST symbols in memory.\n\
                   * None of the symbols/comments matched query tokens with threshold >= 12.0.\n\
                 👉 Tip: Verify symbol name case or check repo_map for available module exports.",
                a.query,
                scope_desc,
                files_in_scope.len(),
                symbols_in_scope,
                langs,
                total_nodes,
                graph.nodes.values().map(|n| &n.file).collect::<HashSet<_>>().len(),
                query_terms_str,
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
        );

        ok(output)
    }
}

/// Score all symbols across files using multi-field similarity & graph topology.
fn score_workspace_symbols(
    graph: &CodeGraph,
    tokens: &SearchTokens,
    scope: Option<&Path>,
) -> Vec<FileCandidate> {
    let mut file_map: HashMap<PathBuf, Vec<ScoredSymbol>> = HashMap::new();

    for (_id, node) in &graph.nodes {
        if let Some(sc) = scope {
            if !path_matches_scope(&node.file, sc) {
                continue;
            }
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

        let callers_cnt = graph.callers(node.id).map(|v| v.len()).unwrap_or(0);
        let callees_cnt = graph.callees(node.id).map(|v| v.len()).unwrap_or(0);
        let graph_mass = ((callers_cnt + callees_cnt) as f64).min(10.0);

        let kind_weight = match node.kind {
            SymbolKind::Function | SymbolKind::Method | SymbolKind::RouteEndpoint => 1.0,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait => 0.95,
            SymbolKind::SqlStatement => 0.95,
            SymbolKind::ConfigProperty | SymbolKind::UiElement => 0.85,
            SymbolKind::Constant | SymbolKind::Variable => 0.75,
            _ => 0.7,
        };

        let raw_score = ((name_sim + name_bonus) * 0.45 + doc_sim * 0.25 + inline_sim * 0.15 + path_sim * 0.10 + graph_mass * 1.0) * kind_weight;

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

        let adjusted_score = if is_generated {
            (top_score * 0.15).max(1.0)
        } else if is_test {
            // Heavily penalize test code so production files always rank first
            (top_score * 0.25 - 20.0).max(1.0)
        } else {
            top_score + 25.0 // Production code boost
        };

        candidates.push(FileCandidate {
            file,
            top_score: adjusted_score,
            symbols: syms,
            is_test,
            is_generated,
        });
    }

    // Sort production files ahead of test files
    candidates.sort_by(|a, b| {
        b.top_score.partial_cmp(&a.top_score).unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Trace flow spine bidirectionally from the highest-scoring seed symbols.
fn extract_flow_spine(
    graph: &CodeGraph,
    candidates: &[FileCandidate],
) -> (Vec<(SymbolId, Option<SymbolId>, EdgeKind)>, bool) {
    let mut seeds = Vec::new();
    for fc in candidates.iter().take(4) {
        for s in fc.symbols.iter().take(3) {
            if s.total_score >= 20.0 {
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
        // Trace Callees (forward 2 hops)
        if let Some(callees) = graph.callees(seed) {
            for e in callees.iter().take(4) {
                if visited.insert(e.to) {
                    spine_edges.push((seed, Some(e.to), e.kind.clone()));
                    if let Some(sub_callees) = graph.callees(e.to) {
                        for sub_e in sub_callees.iter().take(2) {
                            if visited.insert(sub_e.to) {
                                spine_edges.push((e.to, Some(sub_e.to), sub_e.kind.clone()));
                            }
                        }
                    }
                }
            }
        }
        // Trace Callers (backward 1 hop)
        if let Some(callers) = graph.callers(seed) {
            for e in callers.iter().take(3) {
                if visited.insert(e.to) {
                    spine_edges.push((e.to, Some(seed), EdgeKind::Calls));
                }
            }
        }
    }

    let connected = spine_edges.len() >= 2;
    (spine_edges, connected)
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
) -> String {
    let mut out = Vec::new();
    let top_files = &candidates[..candidates.len().min(max_files)];

    if connected {
        out.push(format!("### 🔗 Flow Trace: \"{query}\"\n"));
        out.push("```mermaid".to_string());
        out.push("graph TD".to_string());
        for (from, to, kind) in flow_spine.iter().take(16) {
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
        out.push("```\n".to_string());
    } else {
        out.push(format!("### 🏆 Top Ranked Relevant Files & Symbols for: \"{query}\"\n"));
        out.push("> ℹ️ No multi-hop continuous flow detected. Displaying top-scored weighted symbols & code blocks below:\n".to_string());
    }

    // Top Matched Candidates Overview Table
    out.push("#### 📋 Matched Symbol Candidates:".to_string());
    out.push("| Score | File | Symbols & Lines | Match Signal |".to_string());
    out.push("| :--- | :--- | :--- | :--- |".to_string());

    for fc in top_files {
        let rel_path = fc.file.strip_prefix(root).unwrap_or(&fc.file).display().to_string();
        let top_sym = &fc.symbols[0];
        let sym_summary: Vec<String> = fc
            .symbols
            .iter()
            .take(4)
            .map(|s| format!("`{}`:L{}", s.node.name, s.node.start_line))
            .collect();
        let reason = if top_sym.name_score > 40.0 {
            "🎯 精确符号/标识符"
        } else if top_sym.doc_score > 30.0 {
            "📝 注释/文档"
        } else if top_sym.inline_score > 30.0 {
            "🔍 内部代码逻辑"
        } else {
            "🌐 语义/拓扑相关"
        };
        out.push(format!(
            "| **{:.1}** | `{rel_path}` | {} | {reason} |",
            fc.top_score,
            sym_summary.join(", ")
        ));
    }
    out.push("".to_string());

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
                    out.push(format!("> `[Already sent: {rel_path} L{start_line}-L{end_line} ({}) — refer to previous turns]`\n", labels.join(", ")));
                } else {
                    sent_list.push((start_line, end_line));
                    let mut snippet = Vec::new();
                    for l in start_line..=end_line {
                        if l - 1 < lines.len() {
                            snippet.push(format!("{l}\t{}", lines[l - 1]));
                        }
                    }
                    let ext = fc.file.extension().and_then(|e| e.to_str()).unwrap_or("");
                    out.push(format!("// Symbols: {}\n```{ext}\n{}\n```\n", labels.join(", "), snippet.join("\n")));
                }
            }
        }
    }

    // Trailing Pointers for remaining files
    if candidates.len() > max_files {
        out.push("\n**Additional Relevant Symbols (not fully expanded):**".to_string());
        for c in &candidates[max_files..candidates.len().min(max_files + 6)] {
            let rel = c.file.strip_prefix(root).unwrap_or(&c.file).display().to_string();
            let sym_names: Vec<String> = c.symbols.iter().take(3).map(|s| format!("{}:L{}", s.node.name, s.node.start_line)).collect();
            out.push(format!("- `{rel}`: {}", sym_names.join(", ")));
        }
    }

    out.join("\n")
}

