//! Shared read-only schema cache for `scope=session` MCP servers.
//!
//! A short-lived probe process runs `initialize` + `tools/list`, publishes the
//! snapshot, then shuts down. Every session reads that snapshot so WebUI/TUI
//! can show Connected and the agent can mount tools without spawning a
//! per-session process until the first `call_tool`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use tokio::sync::{Mutex, RwLock};

use super::client::McpToolInfo;
use super::config::{load_mcp_config, McpScope, McpServerConfig};
use super::registry::McpRegistry;
use super::types::ServerStatus;
use super::CONNECT_TIMEOUT;

#[derive(Clone, Debug, Default)]
pub struct SessionMcpSchemaSnapshot {
    pub tools: Vec<McpToolInfo>,
    pub instructions: std::collections::BTreeMap<String, String>,
    pub statuses: Vec<(String, ServerStatus)>,
    /// Allowed `scope=session` configs captured with this probe. Live
    /// registries diff against this to recycle only servers whose transport
    /// identity changed.
    pub configs: Vec<McpServerConfig>,
}

struct SchemaCacheState {
    by_project: RwLock<HashMap<PathBuf, SessionMcpSchemaSnapshot>>,
    probe_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

static CACHE: OnceLock<SchemaCacheState> = OnceLock::new();

fn cache() -> &'static SchemaCacheState {
    CACHE.get_or_init(|| SchemaCacheState {
        by_project: RwLock::new(HashMap::new()),
        probe_locks: Mutex::new(HashMap::new()),
    })
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

pub async fn cached_session_mcp_schema(project_dir: &Path) -> Option<SessionMcpSchemaSnapshot> {
    let key = project_key(project_dir);
    cache().by_project.read().await.get(&key).cloned()
}

async fn probe_lock(project_dir: &Path) -> Arc<Mutex<()>> {
    let key = project_key(project_dir);
    let mut locks = cache().probe_locks.lock().await;
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Run one short-lived session-scope registry, store the snapshot, hydrate any
/// already-live session registries, then shut the probe down. Existing
/// per-session transports are left running.
pub async fn refresh_session_mcp_schema(project_dir: &Path) -> SessionMcpSchemaSnapshot {
    let key = project_key(project_dir);
    let lock = probe_lock(project_dir).await;
    let _guard = lock.lock().await;

    let probe = McpRegistry::from_config_background_for_scope(
        project_dir,
        None,
        super::config::McpScope::Session,
    );
    probe.wait_for_initial_connections(CONNECT_TIMEOUT).await;
    let tools = probe.list_all_tools_cached().await;
    let statuses = probe.server_statuses().await;
    let instructions = probe.server_instructions_snapshot();
    probe.shutdown().await;

    let configs = match load_mcp_config(project_dir) {
        Ok(_) => allowed_session_configs(project_dir),
        Err(_) => cache()
            .by_project
            .read()
            .await
            .get(&key)
            .map(|previous| previous.configs.clone())
            .unwrap_or_default(),
    };
    let snapshot = SessionMcpSchemaSnapshot {
        tools,
        instructions,
        statuses,
        configs,
    };
    cache()
        .by_project
        .write()
        .await
        .insert(key.clone(), snapshot.clone());
    super::session_pool::SessionMcpPool::global()
        .hydrate_project(&key, &snapshot)
        .await;
    snapshot
}

/// Return the cached snapshot, probing once if this project has never been
/// probed in the current process.
pub async fn ensure_session_mcp_schema(project_dir: &Path) -> SessionMcpSchemaSnapshot {
    if let Some(snapshot) = cached_session_mcp_schema(project_dir).await {
        return snapshot;
    }
    refresh_session_mcp_schema(project_dir).await
}

fn allowed_session_configs(project_dir: &Path) -> Vec<McpServerConfig> {
    match load_mcp_config(project_dir) {
        Ok(configs) => {
            let session: Vec<_> = configs
                .into_iter()
                .filter(|config| config.scope == McpScope::Session)
                .collect();
            McpRegistry::partition_by_trust(session, project_dir).0
        }
        Err(_) => Vec::new(),
    }
}
