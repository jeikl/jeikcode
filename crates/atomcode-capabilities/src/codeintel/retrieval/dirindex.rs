//! Directory aggregate index (`dirindex.v1.json`): per-directory file/symbol
//! counts plus the directory tree, precomputed at index-build time.
//!
//! Lets the code_explore directory panorama resolve "which directories exist,
//! how many files/symbols each holds" from a compact sidecar instead of
//! re-walking the whole `file_symbols` map on every query. Pure statistics,
//! no model inference. Missing / stale sidecar degrades gracefully: the
//! panorama falls back to the live graph walk (see `explore.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::graph::CodeGraph;

/// On-disk sidecar name next to `units.v3.json`.
pub const DIRINDEX_REL: &str = ".atomcode/codegraph/dirindex.v1.json";

const DIRINDEX_VERSION: u32 = 1;

/// Per-directory aggregate statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirEntry {
    /// Number of indexed source files directly inside this directory.
    pub file_count: usize,
    /// Number of symbols directly inside this directory's files.
    pub symbol_count: usize,
}

/// Directory aggregate index. Paths are stored as lossy absolute strings
/// (matching how `CodeGraph` keys `file_symbols`), so a query can use it
/// without a root join; a moved workspace simply misses the sidecar and
/// falls back to the graph walk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirIndex {
    pub version: u32,
    /// normalized absolute dir path (lowercased, `/`-separated) → stats.
    pub dirs: HashMap<String, DirEntry>,
    /// normalized absolute dir path → direct child dir paths (sorted).
    pub children: HashMap<String, Vec<String>>,
}

/// Normalize a directory path for stable sidecar keys. Keeps the ORIGINAL
/// casing (only strips the `\\?\` prefix and trailing separators) so keys match
/// `CodeGraph::file_symbols` parent paths exactly on the same machine —
/// lowercasing would break lookups against real `PathBuf`s.
pub fn normalize_dir_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let norm = stripped.replace('/', "\\");
    norm.trim_end_matches('\\').to_string()
}

impl DirIndex {
    /// Build the aggregate index from the workspace graph.
    pub fn build(graph: &CodeGraph) -> Self {
        let mut dirs: HashMap<String, DirEntry> = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();

        for (file, ids) in &graph.file_symbols {
            let Some(dir) = file.parent() else { continue };
            let key = normalize_dir_path(dir);
            let e = dirs.entry(key.clone()).or_default();
            e.file_count += 1;
            e.symbol_count += ids.len();
            // Parent chain: ensure every ancestor exists (for subtree/parent
            // lookups without walking the tree).
            let mut cur = dir.to_path_buf();
            while let Some(parent) = cur.parent() {
                if parent.as_os_str().is_empty() {
                    break;
                }
                let pk = normalize_dir_path(parent);
                let pkey = normalize_dir_path(&cur);
                dirs.entry(pk.clone()).or_default();
                let ch = children.entry(pk).or_default();
                if !ch.contains(&pkey) {
                    ch.push(pkey.clone());
                }
                cur = parent.to_path_buf();
            }
        }

        for v in children.values_mut() {
            v.sort();
            v.dedup();
        }

        Self {
            version: DIRINDEX_VERSION,
            dirs,
            children,
        }
    }

    /// Whether this index can be consumed (version match, non-empty).
    pub fn is_usable(&self) -> bool {
        self.version == DIRINDEX_VERSION
    }

    /// Look up per-directory stats by a real directory path.
    pub fn entry(&self, dir: &Path) -> Option<&DirEntry> {
        self.dirs.get(&normalize_dir_path(dir))
    }

    /// Direct children of a directory, sorted.
    pub fn children_of(&self, dir: &Path) -> Option<&[String]> {
        self.children
            .get(&normalize_dir_path(dir))
            .map(|v| v.as_slice())
    }

    /// All indexed directory keys (normalized absolute paths).
    pub fn all_dirs(&self) -> impl Iterator<Item = &str> {
        self.dirs.keys().map(|s| s.as_str())
    }

    /// Resolve a normalized key back to a `PathBuf` (re-hydrating the drive
    /// letter case is impossible after lowercasing; callers use it only for
    /// path-matching, not for disk access).
    pub fn key_to_path(key: &str) -> PathBuf {
        PathBuf::from(key.replace('\\', "/"))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
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

    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let idx: DirIndex = serde_json::from_slice(&bytes).ok()?;
        if idx.is_usable() {
            Some(idx)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::graph::{SymbolKind, SymbolNode, Visibility};

    fn graph_with_dirs() -> CodeGraph {
        let mut g = CodeGraph::new();
        let mut mk = |id: u64, name: &str, file: &str| {
            let n = SymbolNode {
                id,
                name: name.into(),
                kind: SymbolKind::Function,
                visibility: Visibility::Public,
                file: PathBuf::from(file),
                start_line: 1,
                end_line: 5,
                signature: None,
                ..Default::default()
            };
            g.add_symbol(n);
        };
        mk(1, "run_loop", "crates/x/src/session/run_loop.rs");
        mk(2, "turn", "crates/x/src/session/turn.rs");
        mk(3, "mcp", "crates/x/src/extensions/mcp.rs");
        g
    }

    #[test]
    fn build_counts_files_and_symbols_per_dir() {
        let g = graph_with_dirs();
        let idx = DirIndex::build(&g);
        let session = idx.entry(Path::new("crates/x/src/session"));
        assert!(session.is_some());
        assert_eq!(session.unwrap().file_count, 2);
        assert_eq!(session.unwrap().symbol_count, 2);
        let extensions = idx.entry(Path::new("crates/x/src/extensions"));
        assert_eq!(extensions.unwrap().file_count, 1);
    }

    #[test]
    fn build_records_parent_chain() {
        let g = graph_with_dirs();
        let idx = DirIndex::build(&g);
        // Ancestors exist even though no file lives directly there.
        for ancestor in ["crates/x/src", "crates/x", "crates"] {
            assert!(
                idx.entry(Path::new(ancestor)).is_some(),
                "ancestor {ancestor} must be indexed"
            );
        }
        // children of src include session and extensions.
        let kids = idx.children_of(Path::new("crates/x/src")).unwrap();
        assert!(kids.iter().any(|k| k.ends_with("session")));
        assert!(kids.iter().any(|k| k.ends_with("extensions")));
    }

    #[test]
    fn save_load_roundtrip_and_version_gate() {
        let g = graph_with_dirs();
        let idx = DirIndex::build(&g);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dirindex.v1.json");
        idx.save(&path).unwrap();
        let loaded = DirIndex::load(&path).unwrap();
        assert_eq!(loaded.dirs.len(), idx.dirs.len());
        assert!(loaded.is_usable());
        // Version mismatch → unusable.
        let mut bad = idx.clone();
        bad.version = 999;
        let bad_path = dir.path().join("bad.json");
        bad.save(&bad_path).unwrap();
        assert!(DirIndex::load(&bad_path).is_none());
    }
}
