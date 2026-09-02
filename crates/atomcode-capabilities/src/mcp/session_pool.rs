//! Session-owned MCP registries for stateful servers such as browsers.
//!
//! Schema is shared via a short-lived probe cache. The per-session process is
//! spawned only on first tool call, kept while that session exists, reaped when
//! the session is deleted, and reaped with the JeikCode process on host exit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, RwLock};

use super::registry::McpRegistry;
use super::schema_cache::{ensure_session_mcp_schema, SessionMcpSchemaSnapshot};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SessionMcpKey {
    project_dir: PathBuf,
    session_id: String,
}

struct SessionMcpEntry {
    generation: Arc<SessionMcpGeneration>,
}

struct SessionMcpGeneration {
    registry: Arc<McpRegistry>,
    owners: AtomicUsize,
}

/// Process-wide pool of isolated `scope=session` registries.
///
/// The project path is part of the key because imported/external session ids are
/// not guaranteed to be globally unique. Lifecycle transitions are serialized,
/// while MCP calls never take the lifecycle lock.
pub struct SessionMcpPool {
    entries: RwLock<HashMap<SessionMcpKey, SessionMcpEntry>>,
    lifecycle: Mutex<()>,
}

static GLOBAL_SESSION_MCP_POOL: OnceLock<Arc<SessionMcpPool>> = OnceLock::new();

impl SessionMcpPool {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            lifecycle: Mutex::new(()),
        }
    }

    pub fn global() -> Arc<Self> {
        GLOBAL_SESSION_MCP_POOL
            .get_or_init(|| Arc::new(Self::new()))
            .clone()
    }

    /// Acquire one owner lease. Concurrent prepares for the same session share
    /// exactly one isolated registry; different sessions never share transports.
    /// A zero-owner entry is reused until [`Self::retire_session`] so an already
    /// started process survives runtime handoff and host idle.
    pub async fn acquire(
        self: &Arc<Self>,
        project_dir: &Path,
        session_id: &str,
    ) -> SessionMcpLease {
        let snapshot = ensure_session_mcp_schema(project_dir).await;
        let key = SessionMcpKey {
            project_dir: project_key(project_dir),
            session_id: session_id.to_string(),
        };
        let _lifecycle = self.lifecycle.lock().await;
        let generation = {
            let mut entries = self.entries.write().await;
            if let Some(entry) = entries.get(&key) {
                entry.generation.owners.fetch_add(1, Ordering::AcqRel);
                entry.generation.clone()
            } else {
                let registry = Arc::new(McpRegistry::from_config_lazy_session(&key.project_dir));
                let generation = Arc::new(SessionMcpGeneration {
                    registry,
                    owners: AtomicUsize::new(1),
                });
                entries.insert(
                    key.clone(),
                    SessionMcpEntry {
                        generation: generation.clone(),
                    },
                );
                generation
            }
        };
        drop(_lifecycle);
        generation
            .registry
            .apply_session_config_diff(&snapshot)
            .await;
        SessionMcpLease {
            key: Some(key),
            generation,
        }
    }

    /// Apply a refreshed probe snapshot to every live session registry for this
    /// project. Unchanged transports stay running; identity-changed servers are
    /// recycled per-session.
    pub async fn hydrate_project(&self, project_dir: &Path, snapshot: &SessionMcpSchemaSnapshot) {
        let project_dir = project_key(project_dir);
        let registries: Vec<_> = {
            let entries = self.entries.read().await;
            entries
                .iter()
                .filter(|(key, _)| key.project_dir == project_dir)
                .map(|(_, entry)| entry.generation.registry.clone())
                .collect()
        };
        for registry in registries {
            registry.apply_session_config_diff(snapshot).await;
        }
    }

    /// Shut down the isolated process for one session. Called on session delete.
    pub async fn retire_session(&self, project_dir: &Path, session_id: &str) {
        let key = SessionMcpKey {
            project_dir: project_key(project_dir),
            session_id: session_id.to_string(),
        };
        self.retire_key(key).await;
    }

    /// Session ids are UUIDs; delete paths that only have the id still reap.
    pub async fn retire_session_id(&self, session_id: &str) {
        let keys: Vec<_> = {
            let entries = self.entries.read().await;
            entries
                .keys()
                .filter(|key| key.session_id == session_id)
                .cloned()
                .collect()
        };
        for key in keys {
            self.retire_key(key).await;
        }
    }

    async fn retire_key(&self, key: SessionMcpKey) {
        let _lifecycle = self.lifecycle.lock().await;
        let generation = {
            let mut entries = self.entries.write().await;
            entries.remove(&key).map(|entry| entry.generation)
        };
        if let Some(generation) = generation {
            generation.registry.shutdown().await;
        }
    }

    /// Refresh the shared schema cache without killing unchanged live processes.
    pub async fn invalidate_project(&self, project_dir: &Path) {
        super::schema_cache::refresh_session_mcp_schema(project_dir).await;
    }

    pub async fn cached_registry(
        &self,
        project_dir: &Path,
        session_id: &str,
    ) -> Option<Arc<McpRegistry>> {
        let key = SessionMcpKey {
            project_dir: project_key(project_dir),
            session_id: session_id.to_string(),
        };
        self.entries
            .read()
            .await
            .get(&key)
            .map(|entry| entry.generation.registry.clone())
    }

    /// Host shutdown reaps every session-scoped process in this pool.
    pub async fn shutdown_all(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let stale: Vec<_> = {
            let mut entries = self.entries.write().await;
            entries.drain().map(|(_, entry)| entry.generation).collect()
        };
        for generation in stale {
            generation.registry.shutdown().await;
        }
    }

    #[cfg(test)]
    async fn owner_count(&self, project_dir: &Path, session_id: &str) -> usize {
        let key = SessionMcpKey {
            project_dir: project_key(project_dir),
            session_id: session_id.to_string(),
        };
        self.entries
            .read()
            .await
            .get(&key)
            .map(|entry| entry.generation.owners.load(Ordering::Acquire))
            .unwrap_or_default()
    }
}

impl Default for SessionMcpPool {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII owner for a session registry. Provider-only reassembly reuses the same
/// CodingParts and therefore the same lease; overlapping handoffs are ref-counted.
/// Dropping the last lease does **not** kill the session process.
pub struct SessionMcpLease {
    key: Option<SessionMcpKey>,
    generation: Arc<SessionMcpGeneration>,
}

impl SessionMcpLease {
    pub fn registry(&self) -> Arc<McpRegistry> {
        self.generation.registry.clone()
    }
}

impl Drop for SessionMcpLease {
    fn drop(&mut self) {
        let Some(_key) = self.key.take() else {
            return;
        };
        self.generation.owners.fetch_sub(1, Ordering::AcqRel);
    }
}

fn project_key(project_dir: &Path) -> PathBuf {
    let path = crate::pathnorm::canonicalize(project_dir)
        .unwrap_or_else(|_| crate::pathnorm::strip_verbatim_path(project_dir));
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(target_os = "windows"))]
    {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn leases_share_within_a_session_and_isolate_other_sessions() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let first = pool.acquire(project.path(), "a").await;
        let second = pool.acquire(project.path(), "a").await;
        let other = pool.acquire(project.path(), "b").await;

        assert!(Arc::ptr_eq(&first.registry(), &second.registry()));
        assert!(!Arc::ptr_eq(&first.registry(), &other.registry()));
        assert_eq!(pool.owner_count(project.path(), "a").await, 2);

        drop(first);
        tokio::task::yield_now().await;
        assert_eq!(pool.owner_count(project.path(), "a").await, 1);
        drop(second);
        drop(other);
        tokio::task::yield_now().await;
        assert_eq!(pool.owner_count(project.path(), "a").await, 0);
        assert_eq!(pool.entries.read().await.len(), 2);
    }

    #[tokio::test]
    async fn last_lease_drop_reuses_the_same_generation() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let first = pool.acquire(project.path(), "a").await;
        let old = first.registry();
        drop(first);
        tokio::task::yield_now().await;
        let again = pool.acquire(project.path(), "a").await;
        assert!(Arc::ptr_eq(&old, &again.registry()));
        drop(again);
    }

    #[tokio::test]
    async fn retire_session_removes_the_generation() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let lease = pool.acquire(project.path(), "a").await;
        drop(lease);
        pool.retire_session(project.path(), "a").await;
        assert!(pool.cached_registry(project.path(), "a").await.is_none());
    }

    #[tokio::test]
    async fn shutdown_all_removes_every_generation() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let first = pool.acquire(project.path(), "a").await;
        let other = pool.acquire(project.path(), "b").await;
        drop(first);
        drop(other);
        pool.shutdown_all().await;
        assert!(pool.cached_registry(project.path(), "a").await.is_none());
        assert!(pool.cached_registry(project.path(), "b").await.is_none());
        assert!(pool.entries.read().await.is_empty());
    }
}
