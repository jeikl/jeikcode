//! SQLite-backed incremental index storage for `CodeGraph` & `FileUnit` cache.
//!
//! Replaces giant monolithic JSON/bin rewrites with row-level atomic upserts/deletions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OpenFlags};

use super::index::FileUnit;
use super::graph::CodeGraph;

pub const DISK_CACHE_REL_DB: &str = ".atomcode/codegraph/index.v1.db";

pub fn disk_cache_path_db(root: &Path) -> PathBuf {
    super::canonical(root).join(DISK_CACHE_REL_DB)
}

/// Thread-safe SQLite connection wrapper for index operations.
pub struct IndexDb {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl IndexDb {
    /// Opens or creates the SQLite index database at `db_path`.
    pub fn open(db_path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        // Performance & concurrency tunings
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS file_units (
                 path TEXT PRIMARY KEY,
                 mtime_ns INTEGER NOT NULL,
                 len INTEGER NOT NULL,
                 data BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS graph_meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 walk_fp INTEGER NOT NULL,
                 data BLOB NOT NULL
             );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
        })
    }

    /// Loads the stored walk fingerprint, if present.
    pub fn get_walk_fp(&self) -> Option<u64> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT walk_fp FROM graph_meta WHERE id = 1")
            .ok()?;
        let mut rows = stmt.query([]).ok()?;
        if let Some(row) = rows.next().ok()? {
            let fp: i64 = row.get(0).ok()?;
            Some(fp as u64)
        } else {
            None
        }
    }

    /// Loads all cached `FileUnit` entries from SQLite.
    pub fn load_units(&self) -> HashMap<PathBuf, FileUnit> {
        let mut map = HashMap::new();
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return map,
        };
        let mut stmt = match conn.prepare("SELECT path, data FROM file_units") {
            Ok(s) => s,
            Err(_) => return map,
        };

        let rows = match stmt.query_map([], |row| {
            let path_str: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((path_str, blob))
        }) {
            Ok(r) => r,
            Err(_) => return map,
        };

        for item in rows.flatten() {
            if let Ok(decompressed) = zstd::stream::decode_all(&item.1[..]) {
                if let Ok(unit) = bincode::deserialize::<FileUnit>(&decompressed) {
                    let norm_p = super::index::normalize_index_path(&PathBuf::from(item.0));
                    map.insert(norm_p, unit);
                }
            }
        }
        map
    }

    /// Loads the cached `CodeGraph` from SQLite.
    pub fn load_graph(&self) -> Option<CodeGraph> {
        let conn = self.conn.lock().ok()?;
        let mut stmt = conn
            .prepare("SELECT data FROM graph_meta WHERE id = 1")
            .ok()?;
        let mut rows = stmt.query([]).ok()?;
        if let Some(row) = rows.next().ok()? {
            let blob: Vec<u8> = row.get(0).ok()?;
            let decompressed = zstd::stream::decode_all(&blob[..]).ok()?;
            let mut graph: CodeGraph = bincode::deserialize(&decompressed).ok()?;
            graph.rebuild_name_index();
            Some(graph)
        } else {
            None
        }
    }

    /// Incrementally persists changed/new/deleted file units and updates the graph snapshot.
    pub fn sync_incremental(
        &self,
        walk_fp: u64,
        upsert_units: &[(PathBuf, FileUnit)],
        deleted_paths: &[PathBuf],
        graph: &CodeGraph,
    ) -> Result<(), rusqlite::Error> {
        let mut conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;

        let tx = conn.transaction()?;

        // 1. Delete removed files
        {
            let mut del_stmt = tx.prepare("DELETE FROM file_units WHERE path = ?")?;
            for p in deleted_paths {
                let norm_str = super::index::normalize_index_path(p).to_string_lossy().into_owned();
                del_stmt.execute(params![norm_str])?;
            }
        }

        // 2. Upsert changed/new units
        {
            let mut ins_stmt = tx.prepare(
                "INSERT INTO file_units (path, mtime_ns, len, data)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                     mtime_ns = excluded.mtime_ns,
                     len = excluded.len,
                     data = excluded.data;",
            )?;

            for (p, u) in upsert_units {
                let norm_str = super::index::normalize_index_path(p).to_string_lossy().into_owned();
                if let Ok(serialized) = bincode::serialize(u) {
                    if let Ok(compressed) = zstd::stream::encode_all(&serialized[..], 3) {
                        ins_stmt.execute(params![
                            norm_str,
                            u.mtime_ns as i64,
                            u.len as i64,
                            compressed
                        ])?;
                    }
                }
            }
        }

        // 3. Update global graph snapshot
        if let Ok(graph_bytes) = bincode::serialize(graph) {
            if let Ok(compressed_graph) = zstd::stream::encode_all(&graph_bytes[..], 1) {
                tx.execute(
                    "INSERT INTO graph_meta (id, walk_fp, data)
                     VALUES (1, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET
                         walk_fp = excluded.walk_fp,
                         data = excluded.data",
                    params![walk_fp as i64, compressed_graph],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::graph::{CodeGraph, SymbolKind, SymbolNode, Visibility};
    use crate::codeintel::index::FileUnit;

    #[test]
    fn test_index_db_roundtrip() {
        let temp_dir = std::env::temp_dir().join(format!("atomcode_test_db_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let db_path = temp_dir.join("test_index.db");

        let db = IndexDb::open(&db_path).expect("open index db");
        let mut graph = CodeGraph::new();
        let test_file = PathBuf::from("src/test.rs");
        let node = SymbolNode {
            id: 1,
            name: "test_symbol".to_string(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            file: test_file.clone(),
            start_line: 10,
            end_line: 20,
            signature: Some("fn test_symbol()".to_string()),
            docstring: None,
            inline_comments: Vec::new(),
        };
        graph.add_symbol(node.clone());

        let unit = FileUnit {
            mtime_ns: 123456789,
            len: 100,
            nodes: vec![node],
            calls: Vec::new(),
        };

        db.sync_incremental(999, &[(test_file.clone(), unit.clone())], &[], &graph)
            .expect("sync_incremental");

        let cached_units = db.load_units();
        assert_eq!(cached_units.len(), 1);
        assert_eq!(cached_units.get(&test_file).unwrap().mtime_ns, 123456789);

        let cached_graph = db.load_graph().expect("cached graph");
        assert_eq!(cached_graph.node_count(), 1);
        assert_eq!(db.get_walk_fp(), Some(999));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
