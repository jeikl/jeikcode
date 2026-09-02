//! Session-owned MCP registries for stateful servers such as browsers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, RwLock};

use super::config::McpScope;
use super::registry::McpRegistry;

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
    pub async fn acquire(
        self: &Arc<Self>,
        project_dir: &Path,
        session_id: &str,
    ) -> SessionMcpLease {
        let key = SessionMcpKey {
            project_dir: project_key(project_dir),
            session_id: session_id.to_string(),
        };
        let _lifecycle = self.lifecycle.lock().await;
        let generation = {
            let mut entries = self.entries.write().await;
            if let Some(entry) = entries
                .get(&key)
                // Drop schedules async cleanup after the owner count reaches
                // zero. Never resurrect that terminal generation while its
                // cleanup task is waiting for the lifecycle lock.
                .filter(|entry| entry.generation.owners.load(Ordering::Acquire) > 0)
            {
                entry.generation.owners.fetch_add(1, Ordering::AcqRel);
                entry.generation.clone()
            } else {
                // Remove a zero-owner generation left in the short interval
                // between synchronous Drop and asynchronous release(). The
                // release task retains its own Arc and will shut it down.
                entries.remove(&key);
                let registry = Arc::new(McpRegistry::from_config_background_for_scope(
                    &key.project_dir,
                    None,
                    McpScope::Session,
                ));
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
        SessionMcpLease {
            pool: self.clone(),
            key: Some(key),
            generation,
        }
    }

    async fn release(&self, key: SessionMcpKey, generation: Arc<SessionMcpGeneration>) {
        let _lifecycle = self.lifecycle.lock().await;
        // Defensive guard against future acquisition changes: a generation
        // with a live owner must never be removed or shut down by a delayed
        // zero-owner cleanup task.
        if generation.owners.load(Ordering::Acquire) != 0 {
            return;
        }
        {
            let mut entries = self.entries.write().await;
            let current = entries
                .get(&key)
                .is_some_and(|entry| Arc::ptr_eq(&entry.generation, &generation));
            if current {
                entries.remove(&key);
            }
        }
        // A retired generation is intentionally absent/replaced in the map; its
        // last lease still owns deterministic teardown.
        generation.registry.shutdown().await;
    }

    /// Retire every session generation for one project during `/mcp reload`.
    /// Existing turns keep their old Arc until their runtime hands off; new
    /// prepares immediately acquire fresh config without killing an in-flight call.
    pub async fn invalidate_project(&self, project_dir: &Path) {
        let project_dir = project_key(project_dir);
        let _lifecycle = self.lifecycle.lock().await;
        let retired: Vec<_> = {
            let mut entries = self.entries.write().await;
            let keys: Vec<_> = entries
                .keys()
                .filter(|key| key.project_dir == project_dir)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| entries.remove(&key).map(|entry| entry.generation))
                .collect()
        };
        for generation in retired {
            if generation.owners.load(Ordering::Acquire) == 0 {
                generation.registry.shutdown().await;
            }
        }
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

    /// Host shutdown backstop for all still-live session registries.
    pub async fn shutdown_all(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let registries: Vec<_> = self
            .entries
            .write()
            .await
            .drain()
            .map(|(_, entry)| entry.generation.registry.clone())
            .collect();
        for registry in registries {
            registry.shutdown().await;
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
pub struct SessionMcpLease {
    pool: Arc<SessionMcpPool>,
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
        let Some(key) = self.key.take() else {
            return;
        };
        let previous = self.generation.owners.fetch_sub(1, Ordering::AcqRel);
        if previous != 1 {
            return;
        }
        let pool = self.pool.clone();
        let generation = self.generation.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                pool.release(key, generation).await;
            });
        } else {
            // A host dropping outside Tokio is already tearing down. Cancel
            // background initialization immediately; process-level shutdown is
            // still covered by stdio RAII/Job Object/process-group ownership.
            self.generation.registry.cancel_pending_work();
        }
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
        for _ in 0..20 {
            if pool.entries.read().await.is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("last lease must dispose its session registry");
    }

    #[tokio::test]
    async fn project_invalidation_hands_new_owners_a_fresh_generation() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let old = pool.acquire(project.path(), "a").await;
        pool.invalidate_project(project.path()).await;
        let fresh = pool.acquire(project.path(), "a").await;

        assert!(!Arc::ptr_eq(&old.registry(), &fresh.registry()));
        assert_eq!(pool.owner_count(project.path(), "a").await, 1);
        drop(old);
        drop(fresh);
    }

    #[tokio::test]
    async fn acquire_never_resurrects_a_zero_owner_generation() {
        let pool = Arc::new(SessionMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let mut terminal = pool.acquire(project.path(), "a").await;
        let old = terminal.registry();

        // Reproduce the state between lease Drop's fetch_sub(1 -> 0) and its
        // asynchronously scheduled release task without relying on scheduler
        // timing. Disarm Drop because this test performs that transition here.
        terminal.key.take();
        terminal.generation.owners.store(0, Ordering::Release);

        let fresh = pool.acquire(project.path(), "a").await;
        assert!(!Arc::ptr_eq(&old, &fresh.registry()));
        assert_eq!(pool.owner_count(project.path(), "a").await, 1);

        // A delayed cleanup for the old generation must not evict the fresh one.
        let old_generation = terminal.generation.clone();
        let old_key = SessionMcpKey {
            project_dir: project_key(project.path()),
            session_id: "a".to_string(),
        };
        pool.release(old_key, old_generation).await;
        let cached = pool.cached_registry(project.path(), "a").await.unwrap();
        assert!(Arc::ptr_eq(&cached, &fresh.registry()));
        drop(fresh);
    }
}
