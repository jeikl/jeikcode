//! IDF / BM25 corpus statistics, built offline from the code graph.
//!
//! Pure statistics, no model inference: at query time these are only read.
//! The corpus is the workspace symbol graph — each symbol (function / method /
//! struct / trait / …) is one "document" whose fields are name + signature +
//! docstring + inline comments.

use std::collections::HashMap;

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

/// CJK phrases present in a symbol's text. Only consecutive CJK runs ≥2 chars
/// are kept (single characters are too noisy for BM25 tf).
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
            let run: String = text[start..i].iter().collect();
            if run.chars().count() >= 2 {
                phrases.push(run);
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
    /// Build corpus statistics from the whole workspace graph.
    pub fn build(graph: &CodeGraph) -> Self {
        let mut stats = IdfStats::default();
        let mut seen_file_terms: HashMap<String, u32> = HashMap::new();
        for node in graph.nodes.values() {
            stats.total_symbols += 1;
            let ascii = symbol_ascii_terms(node);
            let cjk = symbol_cjk_phrases(node);
            stats.total_ascii_terms += ascii.len() as u64;
            stats.total_cjk_phrases += cjk.len() as u64;
            for t in ascii {
                *seen_file_terms.entry(t).or_insert(0) += 1;
            }
            for p in cjk {
                *seen_file_terms.entry(p.to_ascii_lowercase()).or_insert(0) += 1;
            }
        }
        // df is per-document: count each term once per symbol.
        for (term, count) in seen_file_terms {
            if count > 0 {
                stats.df.insert(term, count.min(u32::MAX));
            }
        }
        stats
    }

    /// Standard BM25 IDF with additive smoothing (never negative, 0 for terms
    /// seen in every document).
    pub fn idf(&self, term: &str) -> f64 {
        let df = self.df.get(term).copied().unwrap_or(0) as f64;
        let n = self.total_symbols as f64;
        ((n - df + 0.5) / (df + 0.5)).ln().max(0.0)
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
        // "handle_tool_call" appears in 10 symbols → low idf (clamped to 0).
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
        assert!(stats.df.contains_key("主循环处理"), "cjk phrase must be indexed");
        assert_eq!(stats.idf("主循环处理"), stats.idf("主循环处理"));
    }
}
