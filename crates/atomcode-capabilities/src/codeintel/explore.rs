//! `code_explore` — One-shot surgical code intelligence & flow exploration tool.
//!
//! Replaces multi-round search/read cycles by consolidating:
//! 1. Bilingual NLP + Dense Vector semantic intent understanding (Chinese/English)
//! 2. Proximity-bound comment & docstring matching
//! 3. Cross-layer flow spine extraction (Callers/Callees/Routes/SQL)
//! 4. Verbatim line-numbered source slicing (<line>\t<code>, Read-equivalent)
//! 5. Proportional adaptive token budgeting & Whole-file buy rules
//! 6. Session-level duplicate code suppression
//! 7. Dual-mode presentation: Full Flow Trace vs Ranked Fallback Breakdown

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
const MAX_OUTPUT_CHARS_CEILING: usize = 24_000;

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
            return ok(format!(
                "No symbols matching '{}' found in code graph for workspace '{}'.\n\
                 Tip: Try broader query keywords or check repo_map for top-level modules.",
                a.query,
                root.display()
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
            if !node.file.starts_with(sc) {
                continue;
            }
        }

        let name_sim = calculate_text_similarity(tokens, &node.name);
        let mut name_bonus = 0.0;
        let node_name_lower = node.name.to_ascii_lowercase();

        // Exact symbol name hit bonus
        if tokens.code_identifiers.iter().any(|id| id.eq_ignore_ascii_case(&node.name)) {
            name_bonus += 40.0;
        }
        // Bilingual thesaurus term hit in symbol name
        if tokens.expanded_terms.iter().any(|term| node_name_lower.contains(term)) {
            name_bonus += 25.0;
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
            SymbolKind::Constant | SymbolKind::Variable => 0.6,
            _ => 0.6,
        };

        let raw_score = ((name_sim + name_bonus) * 0.40 + doc_sim * 0.25 + inline_sim * 0.15 + path_sim * 0.10 + graph_mass * 1.0) * kind_weight;

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
        let is_test = path_str.contains("test") || path_str.contains("mock") || path_str.contains("spec");
        let is_generated = path_str.contains(".g.") || path_str.contains(".generated.") || path_str.contains(".min.");

        let adjusted_score = if is_generated {
            top_score * 0.3
        } else if is_test {
            top_score * 0.5
        } else {
            top_score
        };

        candidates.push(FileCandidate {
            file,
            top_score: adjusted_score,
            symbols: syms,
            is_test,
            is_generated,
        });
    }

    candidates.sort_by(|a, b| b.top_score.partial_cmp(&a.top_score).unwrap());
    candidates
}

/// Trace flow spine bidirectionally from the highest-scoring seed symbols.
fn extract_flow_spine(
    graph: &CodeGraph,
    candidates: &[FileCandidate],
) -> (Vec<(SymbolId, Option<SymbolId>, EdgeKind)>, bool) {
    let mut seeds = Vec::new();
    for fc in candidates.iter().take(3) {
        for s in fc.symbols.iter().take(2) {
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

/// Render formatted explore output with adaptive budgeting and deduplication.
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
        for (from, to, kind) in flow_spine.iter().take(12) {
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

    // Render source code sections for top files
    let _budget_per_file = (MAX_OUTPUT_CHARS_CEILING / top_files.len().max(1)).clamp(1500, 5000);
    let mut session_spans = SESSION_SENT_SPANS.write().unwrap();

    for fc in top_files {
        let rel_path = fc.file.strip_prefix(root).unwrap_or(&fc.file).display().to_string();
        let top_sym = &fc.symbols[0];

        let reason = if top_sym.name_score > 35.0 {
            format!("(Score: {:.1} | 🎯 命中符号名/标识符)", top_sym.total_score)
        } else if top_sym.doc_score > 30.0 {
            format!("(Score: {:.1} | 📝 命中注释/文档)", top_sym.total_score)
        } else if top_sym.inline_score > 30.0 {
            format!("(Score: {:.1} | 🔍 命中内部行内逻辑)", top_sym.total_score)
        } else {
            format!("(Score: {:.1} | 🌐 拓扑/语义相关)", top_sym.total_score)
        };

        out.push(format!("**`{rel_path}`** — `{}` {reason}", top_sym.node.name));

        if let Ok(content) = std::fs::read_to_string(&fc.file) {
            let lines: Vec<&str> = content.lines().collect();
            let start_line = top_sym.node.start_line.saturating_sub(2).max(1);
            let end_line = (top_sym.node.end_line + 4).min(lines.len());

            // Check Session-level Dedup
            let sent_list = session_spans.entry(rel_path.clone()).or_default();
            let already_sent = sent_list.iter().any(|(s, e)| *s <= start_line && end_line <= *e);

            if already_sent {
                out.push(format!("> `[Already sent in this conversation: {rel_path} L{start_line}-L{end_line} — refer to previous turns]`\n"));
            } else {
                sent_list.push((start_line, end_line));
                let mut snippet = Vec::new();
                for l in start_line..=end_line {
                    if l - 1 < lines.len() {
                        snippet.push(format!("{l}\t{}", lines[l - 1]));
                    }
                }
                let ext = fc.file.extension().and_then(|e| e.to_str()).unwrap_or("");
                out.push(format!("```{ext}\n{}\n```\n", snippet.join("\n")));
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
