//! Offline retrieval statistics (IDF/BM25) for the code-intelligence index.
//!
//! Built purely from the workspace code graph — no model inference. The heavy
//! work (`IdfStats::build`) is intended to run once at index build / refresh;
//! query time only reads the statistics and scores the symbol corpus.

pub mod bm25;
pub mod concepts;
pub mod dirindex;
pub mod stats;

pub use bm25::{bm25_search, Bm25Hit};
pub use concepts::{concept_cosine, concept_projection, contains_cjk, CONCEPT_DIM};
pub use dirindex::DirIndex;
pub use stats::{symbol_ascii_terms, symbol_cjk_phrases, IdfStats};
