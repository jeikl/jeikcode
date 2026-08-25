//! IDF / BM25 corpus statistics, built offline from the code graph.
//!
//! Pure statistics, no model inference: at query time these are only read.
//! The corpus is the workspace symbol graph — each symbol (function / method /
//! struct / trait / …) is one "document" whose fields are name + signature +
//! docstring + inline comments.

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::super::bilingual_nlp::is_cjk;
use super::super::graph::{CodeGraph, SymbolNode};

/// On-disk sidecar name next to `units.v3.json` for corpus statistics.
pub const STATS_REL: &str = ".atomcode/codegraph/stats.v1.json";

const STATS_VERSION: u32 = 1;

/// Tokenization helpers shared by corpus stats and query scoring.
pub fn symbol_ascii_terms(node: &SymbolNode) -> Vec<String> {
    let lower = symbol_text(node).to_ascii_lowercase();
    let mut terms: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            if cur.len() >= 2 {
                terms.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        terms.push(cur);
    }
    terms
}

/// CJK phrases present in a symbol's text with 2-gram and 3-gram sliding windows.
pub fn symbol_cjk_phrases(node: &SymbolNode) -> Vec<String> {
    let text: Vec<char> = symbol_text(node).chars().collect();
    let mut phrases = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if is_cjk(text[i]) {
            let start = i;
            while i < text.len() && is_cjk(text[i]) {
                i += 1;
            }
            let run_chars = &text[start..i];
            let run_len = run_chars.len();
            if run_len >= 2 {
                let full_run: String = run_chars.iter().collect();
                phrases.push(full_run);

                // 2-gram sliding window
                for w in 0..run_len - 1 {
                    let bi: String = run_chars[w..=w + 1].iter().collect();
                    phrases.push(bi);
                }
                // 3-gram sliding window
                if run_len >= 3 {
                    for w in 0..run_len - 2 {
                        let tri: String = run_chars[w..=w + 2].iter().collect();
                        phrases.push(tri);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    phrases
}

/// Lowercased concatenation of every indexed field of a symbol.
pub fn symbol_text(node: &SymbolNode) -> String {
    let mut s = node.name.clone();
    if let Some(sig) = &node.signature {
        s.push(' ');
        s.push_str(sig);
    }
    if let Some(doc) = &node.docstring {
        s.push(' ');
        s.push_str(doc);
    }
    for c in &node.inline_comments {
        s.push(' ');
        s.push_str(c);
    }
    for sc in &node.comments {
        s.push(' ');
        s.push_str(&sc.text);
    }
    for lit in &node.string_literals {
        s.push(' ');
        s.push_str(lit);
    }
    for sql in &node.sql_predicates {
        s.push(' ');
        s.push_str(&sql.raw_clause);
    }
    s
}

/// Corpus statistics for BM25: document frequency per term + corpus size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdfStats {
    /// term (lowercased ascii word or CJK phrase) → number of symbols containing it.
    pub df: HashMap<String, u32>,
    /// total number of symbol documents.
    pub total_symbols: u32,
    /// total ascii term occurrences across the corpus (for avgdl).
    pub total_ascii_terms: u64,
    /// total CJK phrase occurrences across the corpus (for avgdl).
    pub total_cjk_phrases: u64,
    /// sidecar format version (gate for loading).
    pub version: u32,
}

impl Default for IdfStats {
    fn default() -> Self {
        Self {
            df: HashMap::new(),
            total_symbols: 0,
            total_ascii_terms: 0,
            total_cjk_phrases: 0,
            version: STATS_VERSION,
        }
    }
}

impl IdfStats {
    /// Build corpus statistics from the whole workspace graph in parallel.
    pub fn build(graph: &CodeGraph) -> Self {
        let total_symbols = graph.nodes.len() as u32;
        let nodes: Vec<&SymbolNode> = graph.nodes.values().collect();
        let (total_ascii, total_cjk, merged_df) = nodes
            .into_par_iter()
            .fold(
                || (0u64, 0u64, HashMap::<String, u32>::new()),
                |mut acc, node| {
                    let ascii = symbol_ascii_terms(node);
                    let cjk = symbol_cjk_phrases(node);
                    acc.0 += ascii.len() as u64;
                    acc.1 += cjk.len() as u64;
                    let mut local_seen = HashSet::new();
                    for t in ascii {
                        if local_seen.insert(t.clone()) {
                            *acc.2.entry(t).or_insert(0) += 1;
                        }
                    }
                    for p in cjk {
                        let pl = p.to_ascii_lowercase();
                        if local_seen.insert(pl.clone()) {
                            *acc.2.entry(pl).or_insert(0) += 1;
                        }
                    }
                    acc
                },
            )
            .reduce(
                || (0, 0, HashMap::new()),
                |mut a, b| {
                    a.0 += b.0;
                    a.1 += b.1;
                    for (k, v) in b.2 {
                        *a.2.entry(k).or_insert(0) += v;
                    }
                    a
                },
            );

        IdfStats {
            df: merged_df,
            total_symbols,
            total_ascii_terms: total_ascii,
            total_cjk_phrases: total_cjk,
            version: STATS_VERSION,
        }
    }

    /// Standard BM25 IDF with additive smoothing and high-frequency noise damping.
    pub fn idf(&self, term: &str) -> f64 {
        let df = self.df.get(term).copied().unwrap_or(0) as f64;
        let n = self.total_symbols as f64;
        if n <= 1.0 {
            return 1.0;
        }
        let ratio = df / n;
        let damping = if ratio > 0.20 {
            (1.0 - ratio).max(0.15)
        } else {
            1.0
        };
        let raw_idf = ((n - df + 0.5) / (df + 0.5)).ln().max(0.0);
        raw_idf * damping
    }

    /// Average document length (ascii terms + cjk phrases).
    pub fn avgdl(&self) -> f64 {
        let total = (self.total_ascii_terms + self.total_cjk_phrases) as f64;
        let n = self.total_symbols.max(1) as f64;
        (total / n).max(1.0)
    }

    /// Persist to disk (atomic tmp+rename, mirrors units.v3.json).
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load from disk. Returns `None` on missing/corrupt/version-mismatch so
    /// the caller falls back to `IdfStats::build`.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let stats: IdfStats = serde_json::from_slice(&bytes).ok()?;
        if stats.version == STATS_VERSION {
            Some(stats)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::graph::{SymbolKind, Visibility};

    fn node(name: &str, doc: Option<&str>) -> SymbolNode {
        SymbolNode {
            id: 1,
            name: name.into(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: "crates/x/src/y.rs".into(),
            start_line: 1,
            end_line: 5,
            signature: None,
            docstring: doc.map(|s| s.into()),
            inline_comments: vec![],
            comments: vec![],
            sql_predicates: vec![],
            string_literals: vec![],
            metrics: Default::default(),
        }
    }

    #[test]
    fn idf_penalizes_common_terms_and_boosts_rare_ones() {
        let mut graph = CodeGraph::new();
        for i in 0..10u64 {
            let mut n = node("handle_tool_call", Some("执行工具调用"));
            n.id = i + 1;
            graph.add_symbol(n);
        }
        let mut rare = node("sampler_turn", None);
        rare.id = 100;
        graph.add_symbol(rare);

        let stats = IdfStats::build(&graph);
        assert_eq!(stats.total_symbols, 11);
        // "handle_tool_call" appears in 10 symbols → low idf.
        let common_idf = stats.idf("handle_tool_call");
        // "sampler_turn" appears in 1 symbol → high idf.
        let rare_idf = stats.idf("sampler_turn");
        assert!(rare_idf > common_idf, "rare term should have higher idf: {rare_idf} vs {common_idf}");
        // A term absent from the corpus gets the highest idf (df=0).
        let absent_idf = stats.idf("sampler_turn_xyz");
        assert!(absent_idf > rare_idf, "absent term should have the highest idf");
    }

    #[test]
    fn cjk_phrase_detection_and_df() {
        let mut graph = CodeGraph::new();
        let mut n = node("run_loop", Some("主循环处理"));
        n.id = 1;
        graph.add_symbol(n);
        let stats = IdfStats::build(&graph);
        assert!(stats.df.contains_key("主循环处理"), "full cjk phrase must be indexed");
        assert!(stats.df.contains_key("主循环"), "2-gram cjk sub-phrase must be indexed");
        assert!(stats.df.contains_key("循环"), "2-gram cjk sub-phrase must be indexed");
    }
}
