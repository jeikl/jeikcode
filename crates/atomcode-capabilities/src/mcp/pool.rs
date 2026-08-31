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

use tokio::sync::{watch, RwLock};

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
}

static GLOBAL_MCP_POOL: OnceLock<Arc<ProjectMcpPool>> = OnceLock::new();

impl ProjectMcpPool {
    pub fn new() -> Self {
        Self {
            slots: RwLock::new(HashMap::new()),
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
            project_dir: project_dir.to_path_buf(),
            pool: Arc::clone(self),
        }
    }

    /// Get or lazily initialize the shared registry for `project_dir`.
    pub async fn registry(&self, project_dir: &Path) -> Arc<McpRegistry> {
        self.get_or_init(project_dir).await
    }

    /// Subscribe to registry generation changes for `project_dir`.
    pub async fn generation_rx(&self, project_dir: &Path) -> watch::Receiver<u64> {
        self.get_or_init(project_dir).await;
        let slots = self.slots.read().await;
        slots
            .get(project_dir)
            .map(|slot| slot.generation.subscribe())
            .unwrap_or_else(|| watch::channel(0).1)
    }

    /// Tear down the current registry for `project_dir` and build a fresh one.
    pub async fn reload_full(&self, project_dir: &Path) -> Arc<McpRegistry> {
        let stale_slot = {
            let mut slots = self.slots.write().await;
            slots.remove(project_dir)
        };
        let generation = next_generation(stale_slot.as_ref());
        if let Some(stale) = stale_slot {
            stale.registry.shutdown().await;
        }

        let registry = Arc::new(McpRegistry::from_config_background(project_dir));
        spawn_mcp_registry_warmup(registry.clone());
        self.insert_slot(project_dir, registry.clone(), generation)
            .await;
        registry
    }

    /// Replace the cached registry and shut down superseded instances.
    pub async fn replace(&self, project_dir: &Path, replacement: Arc<McpRegistry>) {
        spawn_mcp_registry_warmup(replacement.clone());
        let stale = {
            let mut slots = self.slots.write().await;
            if !slots.contains_key(project_dir) {
                self.evict_oldest_if_needed(&mut slots).await;
            }
            let stale = slots.remove(project_dir);
            let generation = watch::Sender::new(next_generation(stale.as_ref()));
            slots.insert(
                project_dir.to_path_buf(),
                ProjectMcpSlot {
                    registry: replacement.clone(),
                    generation,
                    last_used: Instant::now(),
                },
            );
            stale
        };
        if let Some(stale) = stale {
            if !Arc::ptr_eq(&stale.registry, &replacement) {
                stale.registry.shutdown().await;
            }
        }
    }

    /// Read-through accessor for daemon status endpoints.
    pub async fn cached_registry(&self, project_dir: &Path) -> Option<Arc<McpRegistry>> {
        let slots = self.slots.read().await;
        slots.get(project_dir).map(|slot| slot.registry.clone())
    }

    async fn get_or_init(&self, project_dir: &Path) -> Arc<McpRegistry> {
        {
            let mut slots = self.slots.write().await;
            if let Some(slot) = slots.get_mut(project_dir) {
                slot.last_used = Instant::now();
                return slot.registry.clone();
            }
        }

        let registry = Arc::new(McpRegistry::from_config_background(project_dir));
        spawn_mcp_registry_warmup(registry.clone());
        self.insert_slot(project_dir, registry.clone(), 1).await;
        registry
    }

    async fn insert_slot(&self, project_dir: &Path, registry: Arc<McpRegistry>, generation: u64) {
        let mut slots = self.slots.write().await;
        self.evict_oldest_if_needed(&mut slots).await;
        slots.insert(
            project_dir.to_path_buf(),
            ProjectMcpSlot {
                registry,
                generation: watch::Sender::new(generation),
                last_used: Instant::now(),
            },
        );
    }

    async fn evict_oldest_if_needed(&self, slots: &mut HashMap<PathBuf, ProjectMcpSlot>) {
        if slots.len() < MCP_CACHE_MAX {
            return;
        }
        let Some(oldest_key) = slots
            .iter()
            .min_by_key(|(_, slot)| slot.last_used)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        if let Some(evicted) = slots.remove(&oldest_key) {
            evicted.registry.shutdown().await;
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

fn next_generation(stale: Option<&ProjectMcpSlot>) -> u64 {
    stale.map(|slot| *slot.generation.borrow() + 1).unwrap_or(1)
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
}
