//! Per-project MCP connection pool for a single driver process.
//!
//! Transports are shared across every [`CodingRuntime`] that targets the same
//! working directory (TUI foreground/background slots, CLI, daemon `/chat`).
//! Reload tears down the previous registry — stdio subprocess trees included —
//! before building a replacement from disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tokio::sync::{watch, Mutex, RwLock};

use super::registry::McpRegistry;

/// Maximum number of per-project MCP registries cached in one process.
pub const MCP_CACHE_MAX: usize = 5;

/// Cached MCP registry for a specific project directory.
#[derive(Clone)]
pub struct CachedMcpRegistry {
    pub registry: Arc<McpRegistry>,
    pub last_used: Instant,
}

struct ProjectMcpSlot {
    registry: Arc<McpRegistry>,
    generation: watch::Sender<u64>,
    last_used: Instant,
}

/// Process-wide pool keyed by project working directory.
pub struct ProjectMcpPool {
    slots: RwLock<HashMap<PathBuf, ProjectMcpSlot>>,
    /// Serializes only registry lifecycle transitions. MCP tool calls never
    /// take this lock: sessions continue to call shared transports concurrently.
    /// Without it, two cache misses (or a miss racing reload) can both spawn a
    /// complete registry and one becomes an unreachable process-tree leak.
    lifecycle: Mutex<()>,
}

static GLOBAL_MCP_POOL: OnceLock<Arc<ProjectMcpPool>> = OnceLock::new();

impl ProjectMcpPool {
    pub fn new() -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
            lifecycle: Mutex::new(()),
        }
    }

    /// Shared pool for the current driver process.
    pub fn global() -> Arc<Self> {
        GLOBAL_MCP_POOL
            .get_or_init(|| Arc::new(Self::new()))
            .clone()
    }

    /// Stable handle used by runtimes and drivers for one project directory.
    pub fn handle(self: &Arc<Self>, project_dir: &Path) -> ProjectMcpHandle {
        ProjectMcpHandle {
            project_dir: project_key(project_dir),
            pool: Arc::clone(self),
        }
    }

    /// Get or lazily initialize the shared registry for `project_dir`.
    pub async fn registry(&self, project_dir: &Path) -> Arc<McpRegistry> {
        self.get_or_init(&project_key(project_dir)).await
    }

    /// Subscribe to registry generation changes for `project_dir`.
    pub async fn generation_rx(&self, project_dir: &Path) -> watch::Receiver<u64> {
        let project_dir = project_key(project_dir);
        self.get_or_init(&project_dir).await;
        let slots = self.slots.read().await;
        slots
            .get(&project_dir)
            .map(|slot| slot.generation.subscribe())
            .unwrap_or_else(|| watch::channel(0).1)
    }

    /// Tear down the current registry for `project_dir` and build a fresh one.
    pub async fn reload_full(&self, project_dir: &Path) -> Arc<McpRegistry> {
        let project_dir = project_key(project_dir);
        super::session_pool::SessionMcpPool::global()
            .invalidate_project(&project_dir)
            .await;
        let _lifecycle = self.lifecycle.lock().await;
        let registry = Arc::new(McpRegistry::from_config_background(&project_dir));
        spawn_mcp_registry_warmup(registry.clone());
        let (stale, evicted) = {
            let mut slots = self.slots.write().await;
            let stale = slots.remove(&project_dir);
            let generation = generation_for_replacement(stale.as_ref());
            let evicted = evict_oldest_if_needed(&mut slots, &project_dir);
            slots.insert(
                project_dir,
                ProjectMcpSlot {
                    registry: registry.clone(),
                    generation,
                    last_used: Instant::now(),
                },
            );
            (stale, evicted)
        };
        shutdown_distinct(stale, &registry).await;
        shutdown_slot(evicted).await;
        registry
    }

    /// Replace the cached registry and shut down superseded instances.
    pub async fn replace(&self, project_dir: &Path, replacement: Arc<McpRegistry>) {
        let project_dir = project_key(project_dir);
        let _lifecycle = self.lifecycle.lock().await;
        spawn_mcp_registry_warmup(replacement.clone());
        let (stale, evicted) = {
            let mut slots = self.slots.write().await;
            let stale = slots.remove(&project_dir);
            let generation = generation_for_replacement(stale.as_ref());
            let evicted = evict_oldest_if_needed(&mut slots, &project_dir);
            slots.insert(
                project_dir,
                ProjectMcpSlot {
                    registry: replacement.clone(),
                    generation,
                    last_used: Instant::now(),
                },
            );
            (stale, evicted)
        };
        shutdown_distinct(stale, &replacement).await;
        shutdown_slot(evicted).await;
    }

    /// Read-through accessor for daemon status endpoints.
    pub async fn cached_registry(&self, project_dir: &Path) -> Option<Arc<McpRegistry>> {
        let project_dir = project_key(project_dir);
        let slots = self.slots.read().await;
        slots.get(&project_dir).map(|slot| slot.registry.clone())
    }

    async fn get_or_init(&self, project_dir: &Path) -> Arc<McpRegistry> {
        let _lifecycle = self.lifecycle.lock().await;
        {
            let mut slots = self.slots.write().await;
            if let Some(slot) = slots.get_mut(project_dir) {
                slot.last_used = Instant::now();
                return slot.registry.clone();
            }
        }

        let registry = Arc::new(McpRegistry::from_config_background(project_dir));
        spawn_mcp_registry_warmup(registry.clone());
        let evicted = {
            let mut slots = self.slots.write().await;
            let evicted = evict_oldest_if_needed(&mut slots, project_dir);
            slots.insert(
                project_dir.to_path_buf(),
                ProjectMcpSlot {
                    registry: registry.clone(),
                    generation: watch::Sender::new(1),
                    last_used: Instant::now(),
                },
            );
            evicted
        };
        shutdown_slot(evicted).await;
        registry
    }

    /// Explicit owner shutdown for long-lived drivers. Draining the map first
    /// prevents a concurrent status lookup from retaining a supposedly cached
    /// registry while its process trees are being reaped.
    pub async fn shutdown_all(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let stale: Vec<_> = {
            let mut slots = self.slots.write().await;
            slots.drain().map(|(_, slot)| slot).collect()
        };
        for slot in stale {
            slot.registry.shutdown().await;
        }
    }
}

/// Stable per-project reference into a [`ProjectMcpPool`].
#[derive(Clone)]
pub struct ProjectMcpHandle {
    project_dir: PathBuf,
    pool: Arc<ProjectMcpPool>,
}

impl ProjectMcpHandle {
    pub async fn registry(&self) -> Arc<McpRegistry> {
        self.pool.registry(&self.project_dir).await
    }

    pub async fn reload_full(&self) -> Arc<McpRegistry> {
        self.pool.reload_full(&self.project_dir).await
    }

    pub async fn generation_rx(&self) -> watch::Receiver<u64> {
        self.pool.generation_rx(&self.project_dir).await
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

fn generation_for_replacement(stale: Option<&ProjectMcpSlot>) -> watch::Sender<u64> {
    if let Some(stale) = stale {
        let generation = stale.generation.clone();
        let next = (*generation.borrow()).saturating_add(1);
        generation.send_replace(next);
        generation
    } else {
        watch::Sender::new(1)
    }
}

fn evict_oldest_if_needed(
    slots: &mut HashMap<PathBuf, ProjectMcpSlot>,
    incoming: &Path,
) -> Option<ProjectMcpSlot> {
    if slots.contains_key(incoming) || slots.len() < MCP_CACHE_MAX {
        return None;
    }
    let oldest_key = slots
        .iter()
        .min_by_key(|(_, slot)| slot.last_used)
        .map(|(key, _)| key.clone())?;
    slots.remove(&oldest_key)
}

async fn shutdown_slot(slot: Option<ProjectMcpSlot>) {
    if let Some(slot) = slot {
        slot.registry.shutdown().await;
    }
}

async fn shutdown_distinct(stale: Option<ProjectMcpSlot>, current: &Arc<McpRegistry>) {
    if let Some(stale) = stale {
        if !Arc::ptr_eq(&stale.registry, current) {
            stale.registry.shutdown().await;
        }
    }
}

fn spawn_mcp_registry_warmup(registry: Arc<McpRegistry>) {
    tokio::spawn(async move {
        tokio::select! {
            _ = registry.wait_until_initial_connections_done() => {}
            _ = registry.wait_for_cancellation() => return,
        }
        let _statuses = registry.server_statuses().await;
        let _tools = registry.list_all_tools_cached().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reload_full_replaces_the_cached_registry_for_one_project() {
        let pool = Arc::new(ProjectMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let first = pool.registry(project.path()).await;
        let second = pool.reload_full(project.path()).await;
        assert!(!Arc::ptr_eq(&first, &second));
        let cached = pool
            .cached_registry(project.path())
            .await
            .expect("project must stay cached");
        assert!(Arc::ptr_eq(&cached, &second));
    }

    #[tokio::test]
    async fn concurrent_cache_misses_share_exactly_one_registry() {
        let pool = Arc::new(ProjectMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let futures = (0..32).map(|_| pool.registry(project.path()));
        let registries = futures::future::join_all(futures).await;
        let first = &registries[0];
        assert!(registries
            .iter()
            .all(|registry| Arc::ptr_eq(first, registry)));
        assert_eq!(pool.slots.read().await.len(), 1);
    }

    #[tokio::test]
    async fn reload_notifies_existing_generation_subscribers() {
        let pool = Arc::new(ProjectMcpPool::new());
        let project = tempfile::tempdir().unwrap();
        let first = pool.registry(project.path()).await;
        let mut generation = pool.generation_rx(project.path()).await;
        let second = pool.reload_full(project.path()).await;
        generation.changed().await.unwrap();
        assert_eq!(*generation.borrow_and_update(), 2);
        assert!(!Arc::ptr_eq(&first, &second));
    }
}
