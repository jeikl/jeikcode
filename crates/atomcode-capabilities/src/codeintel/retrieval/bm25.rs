//! BM25 lexical scoring over the symbol corpus.
//!
//! Classic Okapi BM25 (k1=1.2, b=0.75) over two term channels: ascii
//! identifier words and CJK phrases. Pure statistics — no model inference.

use std::collections::HashSet;

use super::super::bilingual_nlp::SearchTokens;
use super::super::graph::{CodeGraph, SymbolNode};
use super::stats::{symbol_ascii_terms, symbol_cjk_phrases, IdfStats};

pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64 = 0.75;

/// One symbol's BM25 score against the query tokens.
#[derive(Debug, Clone)]
pub struct Bm25Hit {
    pub node_id: u64,
    pub score: f64,
}

/// Ranked BM25 hits across the workspace (respecting an optional scope), best
/// first, capped at `top_k`.
pub fn bm25_search(
    graph: &CodeGraph,
    stats: &IdfStats,
    tokens: &SearchTokens,
    scope: Option<&std::path::Path>,
    top_k: usize,
) -> Vec<Bm25Hit> {
    let mut hits: Vec<Bm25Hit> = Vec::new();
    let avgdl = stats.avgdl();
    let query_ascii: HashSet<&str> = tokens.words.iter().map(|s| s.as_str()).collect();
    let query_cjk: HashSet<&str> = tokens
        .cjk_phrases
        .iter()
        .filter(|p| p.chars().count() >= 2)
        .map(|s| s.as_str())
        .collect();
    if query_ascii.is_empty() && query_cjk.is_empty() {
        return hits;
    }

    for node in graph.nodes.values() {
        if let Some(sc) = scope {
            if !super::super::path_matches_scope(&node.file, sc) {
                continue;
            }
        }
        let score = bm25_symbol(node, stats, avgdl, &query_ascii, &query_cjk);
        if score > 0.0 {
            hits.push(Bm25Hit {
                node_id: node.id,
                score,
            });
        }
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(top_k);
    hits
}

/// BM25 for one symbol: sum over matched ascii terms and CJK phrases.
fn bm25_symbol(
    node: &SymbolNode,
    stats: &IdfStats,
    avgdl: f64,
    query_ascii: &HashSet<&str>,
    query_cjk: &HashSet<&str>,
) -> f64 {
    let mut score = 0.0;

    let ascii_terms = symbol_ascii_terms(node);
    let mut seen_ascii: HashSet<&str> = HashSet::new();
    for t in &ascii_terms {
        if seen_ascii.contains(t.as_str()) {
            continue;
        }
        seen_ascii.insert(t.as_str());
        if !query_ascii.contains(t.as_str()) {
            continue;
        }
        let tf = ascii_terms.iter().filter(|x| *x == t).count() as f64;
        score += bm25_term(tf, stats.idf(t), ascii_terms.len() as f64, avgdl);
    }

    let cjk_phrases = symbol_cjk_phrases(node);
    let mut seen_cjk: HashSet<&str> = HashSet::new();
    for p in &cjk_phrases {
        if seen_cjk.contains(p.as_str()) {
            continue;
        }
        seen_cjk.insert(p.as_str());
        let p_lower = p.to_ascii_lowercase();
        if !query_cjk.contains(p_lower.as_str()) {
            continue;
        }
        let tf = cjk_phrases
            .iter()
            .filter(|x| x.eq_ignore_ascii_case(p))
            .count() as f64;
        let len = (ascii_terms.len() + cjk_phrases.len()) as f64;
        score += bm25_term(tf, stats.idf(&p_lower), len, avgdl);
    }

    score
}

fn bm25_term(tf: f64, idf: f64, doc_len: f64, avgdl: f64) -> f64 {
    let denom = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * (doc_len / avgdl));
    idf * (tf * (BM25_K1 + 1.0)) / denom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::graph::{SymbolKind, Visibility};
    use std::path::PathBuf;

    #[test]
    fn bm25_ranks_sampler_turn_above_handle_tool_call_for_sampler_query() {
        let mut graph = CodeGraph::new();
        let a = SymbolNode {
            id: 1,
            name: "sampler_turn".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("src/session/acp_session_impl/sampler_turn.rs"),
            start_line: 10,
            end_line: 20,
            signature: None,
            docstring: Some("run a turn via the sampler".into()),
            ..Default::default()
        };
        graph.add_symbol(a.clone());
        let b = SymbolNode {
            id: 2,
            name: "handle_tool_call".into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: PathBuf::from("src/session/acp_session_impl/tool_calls.rs"),
            start_line: 30,
            end_line: 40,
            signature: None,
            docstring: Some("dispatch a tool call".into()),
            ..Default::default()
        };
        graph.add_symbol(b.clone());
        // Make the corpus larger so idf of "sampler" is not 0.
        for i in 0..20u64 {
            let n = SymbolNode {
                id: 100 + i,
                name: "handle_tool_call".into(),
                kind: SymbolKind::Function,
                visibility: Visibility::Public,
                file: PathBuf::from(format!("src/tools/tool_{i}.rs")),
                start_line: 1,
                end_line: 5,
                signature: None,
                ..Default::default()
            };
            graph.add_symbol(n);
        }
        let stats = IdfStats::build(&graph);
        let mut tokens = SearchTokens::default();
        tokens.words.push("sampler".into());
        tokens.words.push("turn".into());
        let hits = bm25_search(&graph, &stats, &tokens, None, 5);
        assert!(!hits.is_empty());
        assert_eq!(
            hits[0].node_id, a.id,
            "sampler_turn must rank first:\n{hits:?}"
        );
    }
}
