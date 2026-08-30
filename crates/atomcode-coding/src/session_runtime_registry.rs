//! Per-session runtime registry (OpenCode-style session model).
//!
//! One live [`SessionRuntimeEntry`] per [`SessionKey`]; TUI/WebUI clients hold
//! view bindings and subscribe to events without acquiring exclusive session
//! leases. See `docs/plans/2026-08-27-multi-session-runtime-and-agent-efficiency-design.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::sync::RwLock;

/// Stable session identifier (UUID string).
pub type SessionKey = String;

/// Neutral runtime activity — Driver layers project UI labels from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeActivity {
    Starting,
    Ready,
    Running,
    WaitingApproval,
    WaitingUserInput,
    Reconfiguring,
    Stopping,
    Stopped,
    Failed,
}

/// Outcome of [`SessionRuntimeRegistry::open_or_attach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Registry already held a live entry for this session; caller should attach.
    Attached,
    /// A new entry was registered for this session.
    Registered,
}

/// In-memory record for one session's live runner (runtime handle wired in later phases).
#[derive(Debug, Clone)]
pub struct SessionRuntimeEntry {
    pub session_id: SessionKey,
    pub working_dir: PathBuf,
    pub activity: RuntimeActivity,
    pub generation: u64,
}

/// L2 registry: one live runner per session within a process.
#[derive(Debug, Default)]
pub struct SessionRuntimeRegistry {
    entries: RwLock<HashMap<SessionKey, SessionRuntimeEntry>>,
}

/// Process-wide registry (Phase A). Driver layers attach views here instead of
/// competing for per-session OS leases.
static GLOBAL_REGISTRY: OnceLock<SessionRuntimeRegistry> = OnceLock::new();

impl SessionRuntimeRegistry {
    pub fn global() -> &'static SessionRuntimeRegistry {
        GLOBAL_REGISTRY.get_or_init(SessionRuntimeRegistry::new)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub async fn lookup(&self, key: &SessionKey) -> Option<SessionRuntimeEntry> {
        self.entries.read().await.get(key).cloned()
    }

    pub async fn activity(&self, key: &SessionKey) -> Option<RuntimeActivity> {
        self.lookup(key).await.map(|e| e.activity)
    }

    /// List sessions with live runners under `working_dir` (exact path match).
    pub async fn list_activity(
        &self,
        working_dir: &Path,
    ) -> Vec<(SessionKey, RuntimeActivity)> {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| e.working_dir == working_dir)
            .map(|e| (e.session_id.clone(), e.activity))
            .collect()
    }

    /// All live runners (any working directory).
    pub async fn list_all(&self) -> Vec<SessionRuntimeEntry> {
        self.entries.read().await.values().cloned().collect()
    }

    /// Register or attach to a session runner. At most one entry per `session_id`.
    pub async fn open_or_attach(
        &self,
        session_id: SessionKey,
        working_dir: PathBuf,
    ) -> (OpenOutcome, SessionRuntimeEntry) {
        let mut guard = self.entries.write().await;
        if let Some(existing) = guard.get(&session_id) {
            return (OpenOutcome::Attached, existing.clone());
        }
        let entry = SessionRuntimeEntry {
            session_id: session_id.clone(),
            working_dir,
            activity: RuntimeActivity::Starting,
            generation: 1,
        };
        guard.insert(session_id, entry.clone());
        (OpenOutcome::Registered, entry)
    }

    pub async fn set_activity(&self, key: &SessionKey, activity: RuntimeActivity) -> bool {
        let mut guard = self.entries.write().await;
        if let Some(entry) = guard.get_mut(key) {
            entry.activity = activity;
            true
        } else {
            false
        }
    }

    pub async fn release_if_idle(&self, key: &SessionKey) -> bool {
        let mut guard = self.entries.write().await;
        match guard.get(key) {
            Some(entry)
                if matches!(
                    entry.activity,
                    RuntimeActivity::Stopped | RuntimeActivity::Failed | RuntimeActivity::Ready
                ) =>
            {
                guard.remove(key);
                true
            }
            _ => false,
        }
    }

    pub async fn shutdown_all(&self) {
        self.entries.write().await.clear();
    }
}

/// Keep the process-wide registry aligned with TUI foreground/background slots.
pub async fn sync_from_tui_slots(
    foreground_id: SessionKey,
    foreground_dir: PathBuf,
    foreground_running: bool,
    backgrounds: &[(SessionKey, PathBuf, bool)],
) {
    let reg = SessionRuntimeRegistry::global();
    reg.open_or_attach(foreground_id.clone(), foreground_dir.clone())
        .await;
    let fg_activity = if foreground_running {
        RuntimeActivity::Running
    } else {
        RuntimeActivity::Ready
    };
    reg.set_activity(&foreground_id, fg_activity).await;

    let mut live_ids = std::collections::HashSet::new();
    live_ids.insert(foreground_id);
    for (session_id, working_dir, running) in backgrounds {
        live_ids.insert(session_id.clone());
        reg.open_or_attach(session_id.clone(), working_dir.clone())
            .await;
        let activity = if *running {
            RuntimeActivity::Running
        } else {
            RuntimeActivity::Ready
        };
        reg.set_activity(session_id, activity).await;
    }

    let stale: Vec<SessionKey> = reg
        .list_all()
        .await
        .into_iter()
        .filter(|e| e.working_dir == foreground_dir && !live_ids.contains(&e.session_id))
        .map(|e| e.session_id)
        .collect();
    for id in stale {
        reg.release_if_idle(&id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_or_attach_is_idempotent_per_session() {
        let reg = SessionRuntimeRegistry::new();
        let (o1, e1) = reg
            .open_or_attach("s1".into(), PathBuf::from("/proj"))
            .await;
        assert_eq!(o1, OpenOutcome::Registered);
        let (o2, e2) = reg
            .open_or_attach("s1".into(), PathBuf::from("/proj"))
            .await;
        assert_eq!(o2, OpenOutcome::Attached);
        assert_eq!(e1.session_id, e2.session_id);
    }

    #[tokio::test]
    async fn list_activity_scopes_by_working_dir() {
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("a".into(), PathBuf::from("/p1"))
            .await;
        reg.open_or_attach("b".into(), PathBuf::from("/p2"))
            .await;
        let p1 = reg.list_activity(Path::new("/p1")).await;
        assert_eq!(p1.len(), 1);
        assert_eq!(p1[0].0, "a");
    }
}
