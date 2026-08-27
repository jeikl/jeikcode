//! SQLite-backed incremental index storage for `CodeGraph` & `FileUnit` cache.
//!
//! On-disk layout is `.atomcode/codegraph/index.v1.db` with row-level upserts.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{params, Connection, OpenFlags};

use super::graph::CodeGraph;
use super::index::FileUnit;

pub const DISK_CACHE_REL_DB: &str = ".atomcode/codegraph/index.v1.db";

pub fn disk_cache_path_db(root: &Path) -> PathBuf {
    super::canonical(root).join(DISK_CACHE_REL_DB)
}

static SHARED_DBS: OnceLock<Mutex<HashMap<PathBuf, Arc<IndexDb>>>> = OnceLock::new();

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

        // Performance & concurrency tunings. Incremental agent edits must not
        // stall on fsync or a cold page cache. mmap/cache used to be 512MB/128MB,
        // which on a 16GB Linux box competing with a 15k-file first index left
        // nothing for sshd. 128MB mmap + 32MB cache is enough for single-row
        // upserts; journal_size_limit caps WAL growth between checkpoints.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = OFF;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;
             PRAGMA cache_size = -32768;
             PRAGMA mmap_size = 134217728;
             PRAGMA journal_size_limit = 67108864;
             PRAGMA wal_autocheckpoint = 1000;
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
             );
             CREATE TABLE IF NOT EXISTS meta_blobs (
                 key TEXT PRIMARY KEY,
                 data BLOB NOT NULL
             );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
        })
    }

    /// Process-wide connection cache: opening SQLite + applying PRAGMAs on every
    /// edit/query is a real cost, and a fresh handle also fights the WAL lock.
    pub fn open_shared(root: &Path) -> Result<Arc<Self>, rusqlite::Error> {
        let key = disk_cache_path_db(root);
        let cache = SHARED_DBS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = map.get(&key) {
            return Ok(existing.clone());
        }
        let db = Arc::new(Self::open(&key)?);
        map.insert(key, db.clone());
        Ok(db)
    }

    /// Drop the process-wide handle for `root` so `--force` can delete the db
    /// file (Windows refuses `remove_file` while this connection is open).
    pub fn drop_shared(root: &Path) {
        let key = disk_cache_path_db(root);
        if let Some(cache) = SHARED_DBS.get() {
            let mut map = cache.lock().unwrap_or_else(|p| p.into_inner());
            map.remove(&key);
        }
    }

    /// Loads the stored walk fingerprint, if present.
    pub fn get_walk_fp(&self) -> Option<u64> {
        let conn = self.conn.lock().ok()?;
        if let Ok(s) = conn.query_row("SELECT value FROM meta WHERE key = 'walk_fp'", [], |row| {
            row.get::<_, String>(0)
        }) {
            if let Ok(fp) = s.parse::<u64>() {
                return Some(fp);
            }
        }
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

    /// Loads all cached `FileUnit` entries from SQLite in parallel.
    pub fn load_units(&self) -> HashMap<PathBuf, FileUnit> {
        let mut map = HashMap::new();
        let raw_items: Vec<(String, Vec<u8>)> = {
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

            rows.flatten().collect()
        };

        use rayon::prelude::*;
        let decompressed_units: Vec<(PathBuf, FileUnit)> = raw_items
            .into_par_iter()
            .filter_map(|(p, blob)| {
                let decompressed = zstd::stream::decode_all(&blob[..]).ok()?;
                let unit = bincode::deserialize::<FileUnit>(&decompressed).ok()?;
                let norm_p = super::index::normalize_index_path(&PathBuf::from(p));
                Some((norm_p, unit))
            })
            .collect();

        map.reserve(decompressed_units.len());
        for (p, u) in decompressed_units {
            map.insert(p, u);
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

    /// Row-level unit upsert/delete. Does **not** rewrite the graph blob — that
    /// serialize+zstd of the whole `CodeGraph` is what turned a 1-file edit into
    /// a multi-second stall. Cold start recomposes the graph from units.
    pub fn upsert_units(
        &self,
        upsert_units: &[(PathBuf, FileUnit)],
        deleted_paths: &[PathBuf],
    ) -> Result<(), rusqlite::Error> {
        let mut conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;

        let tx = conn.transaction()?;
        apply_unit_writes(&tx, upsert_units, deleted_paths)?;
        tx.commit()?;
        drop(conn);
        self.checkpoint();
        Ok(())
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
        apply_unit_writes(&tx, upsert_units, deleted_paths)?;

        // Full graph snapshot — only for cold-start / first-build callers.
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
        drop(conn);
        self.checkpoint();
        Ok(())
    }

    /// Upsert prepared unit writes (pre-compressed) and delete obsolete records in a single transaction.
    pub fn upsert_units_prepared(
        &self,
        prepared: &[PreparedUnitWrite],
        deleted: &[PathBuf],
    ) -> Result<(), rusqlite::Error> {
        let mut conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;

        let tx = conn.transaction()?;
        apply_prepared_unit_writes(&tx, prepared, deleted)?;
        tx.commit()?;
        // No checkpoint here. Streaming init used to PASSIVE-checkpoint every
        // 256-file batch; around ~6k files the 64MB WAL limit stalled parse
        // for 10s+. Callers checkpoint once at the end of a bulk ingest.
        Ok(())
    }

    /// Bulk-ingest pragmas for a large `init --force`.
    ///
    /// An empty db uses `journal_mode=OFF` so 15k inserts never grow a WAL that
    /// later stalls parse (the ~6144-file checkpoint). A populated db keeps WAL
    /// but disables auto-checkpoint. `on = false` switches back to WAL for
    /// incremental edits — no PASSIVE fold of a giant journal.
    pub fn set_bulk_ingest(&self, on: bool) {
        if let Ok(conn) = self.conn.lock() {
            if on {
                let empty = conn
                    .query_row("SELECT COUNT(*) FROM file_units", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap_or(1)
                    == 0;
                if empty {
                    let _ = conn.execute_batch(
                        "PRAGMA journal_mode = OFF;
                         PRAGMA synchronous = OFF;
                         PRAGMA locking_mode = EXCLUSIVE;",
                    );
                } else {
                    let _ = conn.execute_batch(
                        "PRAGMA wal_autocheckpoint = 0;
                         PRAGMA journal_size_limit = -1;",
                    );
                }
            } else {
                let _ = conn.execute_batch(
                    "PRAGMA locking_mode = NORMAL;
                     PRAGMA journal_mode = WAL;
                     PRAGMA synchronous = OFF;
                     PRAGMA wal_autocheckpoint = 1000;
                     PRAGMA journal_size_limit = 67108864;",
                );
            }
        }
    }

    /// Optimized version using pre-compressed unit blobs produced in parallel workers.
    pub fn sync_incremental_prepared(
        &self,
        walk_fp: u64,
        upsert_units: &[PreparedUnitWrite],
        deleted_paths: &[PathBuf],
        graph: &CodeGraph,
    ) -> Result<(), rusqlite::Error> {
        // Parallel encode graph snapshot outside of SQLite lock
        let compressed_graph = bincode::serialize(graph)
            .ok()
            .and_then(|bytes| zstd::stream::encode_all(&bytes[..], 1).ok());

        let mut conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;

        let tx = conn.transaction()?;
        apply_prepared_unit_writes(&tx, upsert_units, deleted_paths)?;

        if let Some(compressed) = compressed_graph {
            tx.execute(
                "INSERT INTO graph_meta (id, walk_fp, data)
                 VALUES (1, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                     walk_fp = excluded.walk_fp,
                     data = excluded.data",
                params![walk_fp as i64, compressed],
            )?;
        }

        tx.commit()?;
        drop(conn);
        self.checkpoint();
        Ok(())
    }

    /// Persist only the composed graph snapshot (no unit rewrite). Used after a
    /// one-time compose from existing SQLite units so the next process start
    /// can skip both the tree walk and call-graph recomposition.
    pub fn save_graph_only(&self, walk_fp: u64, graph: &CodeGraph) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;
        if let Ok(graph_bytes) = bincode::serialize(graph) {
            if let Ok(compressed_graph) = zstd::stream::encode_all(&graph_bytes[..], 1) {
                conn.execute(
                    "INSERT INTO graph_meta (id, walk_fp, data)
                     VALUES (1, ?, ?)
                     ON CONFLICT(id) DO UPDATE SET
                         walk_fp = excluded.walk_fp,
                         data = excluded.data",
                    params![walk_fp as i64, compressed_graph],
                )?;
            }
        }
        drop(conn);
        let _ = self.set_walk_fp(walk_fp);
        self.checkpoint();
        Ok(())
    }

    /// Persist walk fingerprint without rewriting a graph blob.
    pub fn set_walk_fp(&self, walk_fp: u64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('walk_fp', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![walk_fp.to_string()],
        )?;
        Ok(())
    }

    /// Store a small derived blob (dirindex / idf stats) inside SQLite so we
    /// do not emit sibling `.json` files next to the database.
    pub fn put_meta_blob(&self, key: &str, blob: &[u8]) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("lock poisoned".into()),
            )
        })?;
        let encoded = zstd::stream::encode_all(blob, 1).unwrap_or_else(|_| blob.to_vec());
        conn.execute(
            "INSERT INTO meta_blobs (key, data) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET data = excluded.data",
            params![key, encoded],
        )?;
        Ok(())
    }

    pub fn get_meta_blob(&self, key: &str) -> Option<Vec<u8>> {
        let conn = self.conn.lock().ok()?;
        let raw: Vec<u8> = conn
            .query_row(
                "SELECT data FROM meta_blobs WHERE key = ?",
                params![key],
                |row| row.get(0),
            )
            .ok()?;
        zstd::stream::decode_all(&raw[..]).ok().or(Some(raw))
    }

    /// Fold the WAL back into the main db without forcing a full truncate stall.
    pub fn checkpoint(&self) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedUnitWrite {
    pub path: PathBuf,
    pub mtime_ns: u128,
    pub len: u64,
    pub compressed_blob: Vec<u8>,
}

impl PreparedUnitWrite {
    pub fn from_unit(path: PathBuf, unit: &FileUnit) -> Option<Self> {
        let serialized = bincode::serialize(unit).ok()?;
        let compressed_blob = compress_unit_blob(&serialized)?;
        Some(Self {
            path,
            mtime_ns: unit.mtime_ns,
            len: unit.len,
            compressed_blob,
        })
    }
}

/// Reuse a per-thread zstd compressor. `encode_all` rebuilds CDict/context on
/// every 1–10KB unit; 15k files pay that setup cost more than the deflate.
fn compress_unit_blob(bytes: &[u8]) -> Option<Vec<u8>> {
    thread_local! {
        static COMPRESSOR: RefCell<Option<zstd::bulk::Compressor<'static>>> = RefCell::new(None);
    }
    COMPRESSOR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = zstd::bulk::Compressor::new(1).ok();
        }
        match slot.as_mut() {
            Some(c) => c.compress(bytes).ok(),
            None => zstd::bulk::compress(bytes, 1).ok(),
        }
    })
}

fn apply_unit_writes(
    tx: &rusqlite::Transaction<'_>,
    upsert_units: &[(PathBuf, FileUnit)],
    deleted_paths: &[PathBuf],
) -> Result<(), rusqlite::Error> {
    let prepared: Vec<PreparedUnitWrite> = upsert_units
        .iter()
        .filter_map(|(p, u)| PreparedUnitWrite::from_unit(p.clone(), u))
        .collect();
    apply_prepared_unit_writes(tx, &prepared, deleted_paths)
}

pub fn apply_prepared_unit_writes(
    tx: &rusqlite::Transaction<'_>,
    upsert_units: &[PreparedUnitWrite],
    deleted_paths: &[PathBuf],
) -> Result<(), rusqlite::Error> {
    {
        let mut del_stmt = tx.prepare("DELETE FROM file_units WHERE path = ?")?;
        for p in deleted_paths {
            let norm_str = super::index::normalize_index_path(p)
                .to_string_lossy()
                .into_owned();
            del_stmt.execute(params![norm_str])?;
        }
    }

    if !upsert_units.is_empty() {
        let mut ins_stmt = tx.prepare_cached(
            "INSERT INTO file_units (path, mtime_ns, len, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 mtime_ns = excluded.mtime_ns,
                 len = excluded.len,
                 data = excluded.data;",
        )?;

        for item in upsert_units {
            let norm_str = item.path.to_string_lossy();
            ins_stmt.execute(params![
                norm_str.as_ref(),
                item.mtime_ns as i64,
                item.len as i64,
                item.compressed_blob
            ])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeintel::graph::{CodeGraph, SymbolKind, SymbolNode, Visibility};
    use crate::codeintel::index::FileUnit;

    #[test]
    fn test_index_db_roundtrip() {
        let temp_dir =
            std::env::temp_dir().join(format!("atomcode_test_db_{}", std::process::id()));
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
            ..Default::default()
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

        let updated = FileUnit {
            mtime_ns: 222,
            len: 50,
            nodes: unit.nodes.clone(),
            calls: Vec::new(),
        };
        db.upsert_units(&[(test_file.clone(), updated)], &[])
            .expect("upsert_units");
        let after = db.load_units();
        assert_eq!(after.get(&test_file).unwrap().mtime_ns, 222);
        // Incremental upsert must not require rewriting the graph blob.
        assert_eq!(db.load_graph().unwrap().node_count(), 1);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
