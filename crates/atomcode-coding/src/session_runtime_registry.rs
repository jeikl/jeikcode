//! Per-session runtime registry (OpenCode-style session model).
//!
//! One live [`SessionRuntimeEntry`] per [`SessionKey`]. Driver/UI layers hold a
//! **ViewBinding** (`selected: Option<SessionKey>`) and route submit/cancel by
//! key — they never reconfigure another session's handle to "switch views".
//! See `docs/plans/2026-08-27-multi-session-runtime-and-agent-efficiency-design.md`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use std::sync::RwLock;
use tokio::sync::broadcast;

use crate::runtime::{CodingRuntimeHandle, DriverCommand, RuntimeUnavailable, UserInput};
use atomcode_kernel::event::RequestId;
use atomcode_kernel::message::SessionSnapshot;

/// Stable session identifier (UUID string).
pub type SessionKey = String;

/// Soft cap on concurrent live runners in one process (not "background slots").
pub const MAX_LIVE_SESSIONS: usize = 32;

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

impl RuntimeActivity {
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Running
                | Self::WaitingApproval
                | Self::WaitingUserInput
                | Self::Starting
                | Self::Reconfiguring
        )
    }

    pub fn is_idle_releasable(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed | Self::Ready)
    }

    /// A bound runner is mid-turn. `Starting` is only "row exists" (view
    /// subscribe / deferred spawn) and must not spin WebUI `/chat/active`.
    pub fn is_live_turn(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingApproval | Self::WaitingUserInput
        )
    }
}

/// Outcome of [`SessionRuntimeRegistry::open_or_attach`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Registry already held a live entry for this session; caller should attach.
    Attached,
    /// A new entry was registered for this session.
    Registered,
}

/// Sequenced observation for one session (driver projects to UI).
/// OpenCode fan-out: every client view subscribes here for realtime TextDelta etc.
#[derive(Debug, Clone)]
pub struct SequencedSessionEvent {
    pub session_id: SessionKey,
    pub generation: u64,
    pub sequence: u64,
    pub activity: RuntimeActivity,
    /// Full runtime observation when this is a stream event; `None` for pure
    /// activity / lifecycle markers (e.g. bind_handle → Ready).
    pub runtime: Option<crate::runtime::CodingRuntimeEvent>,
    /// View-layer coordination (user echo, steered ack, request resolved, slash output).
    pub view: Option<SessionViewEvent>,
}

/// Driver/UI fan-out events that are not `CodingRuntimeEvent`s but must reach
/// every subscribed view of a session (parity with LiveHub `LiveViewEvent`).
#[derive(Debug, Clone)]
pub enum SessionViewEvent {
    InputAccepted {
        input: UserInput,
        client_input_id: Option<String>,
    },
    Steered {
        count: usize,
        inputs: Vec<atomcode_kernel::event::SteeredInput>,
        client_input_ids: Vec<Option<String>>,
    },
    CommandOutput(String),
    RequestResolved {
        request_id: RequestId,
        kind: String,
    },
}

/// Ring buffer for multi-view replay (TextDelta-heavy turns need headroom).
const JOURNAL_CAP: usize = 2048;
const BROADCAST_CAP: usize = 512;

/// In-memory record for one session's live runner.
#[derive(Debug, Clone)]
pub struct SessionRuntimeEntry {
    pub session_id: SessionKey,
    pub working_dir: PathBuf,
    pub activity: RuntimeActivity,
    pub generation: u64,
    /// Last known conversation snapshot (optional; filled by drivers).
    pub snapshot: Option<SessionSnapshot>,
    /// Transport id used by TUI event fan-in (optional).
    pub runtime_id: Option<u64>,
}

struct LiveInner {
    meta: SessionRuntimeEntry,
    handle: Option<CodingRuntimeHandle>,
    journal: VecDeque<SequencedSessionEvent>,
    next_sequence: u64,
    event_tx: broadcast::Sender<SequencedSessionEvent>,
    pending_request_id: Option<RequestId>,
    /// Recent agent-observation fingerprints. An observer echoing the journal
    /// back (A,B,A,B…) is dropped before it can peg a core.
    recent_runtime_keys: VecDeque<String>,
}

impl std::fmt::Debug for LiveInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveInner")
            .field("meta", &self.meta)
            .field("has_handle", &self.handle.is_some())
            .field("journal_len", &self.journal.len())
            .field("next_sequence", &self.next_sequence)
            .field("pending_request_id", &self.pending_request_id)
            .finish()
    }
}

impl LiveInner {
    fn new(session_id: SessionKey, working_dir: PathBuf) -> Self {
        let (event_tx, _) = broadcast::channel(BROADCAST_CAP);
        Self {
            meta: SessionRuntimeEntry {
                session_id,
                working_dir,
                activity: RuntimeActivity::Starting,
                generation: 1,
                snapshot: None,
                runtime_id: None,
            },
            handle: None,
            journal: VecDeque::new(),
            next_sequence: 0,
            event_tx,
            pending_request_id: None,
            recent_runtime_keys: VecDeque::new(),
        }
    }

    fn push_activity(&mut self, activity: RuntimeActivity) -> SequencedSessionEvent {
        self.push(activity, None, None)
    }

    fn push_runtime(
        &mut self,
        generation: u64,
        event: crate::runtime::CodingRuntimeEvent,
    ) -> SequencedSessionEvent {
        if generation > 0 {
            self.meta.generation = generation;
        }
        let activity = activity_from_runtime_event(&event, self.meta.activity);
        self.push(activity, Some(event), None)
    }

    fn push_view(&mut self, view: SessionViewEvent) -> SequencedSessionEvent {
        let activity = match &view {
            SessionViewEvent::InputAccepted { .. } | SessionViewEvent::Steered { .. } => {
                RuntimeActivity::Running
            }
            SessionViewEvent::RequestResolved { .. } => RuntimeActivity::Running,
            SessionViewEvent::CommandOutput(_) => self.meta.activity,
        };
        self.push(activity, None, Some(view))
    }

    fn push(
        &mut self,
        activity: RuntimeActivity,
        runtime: Option<crate::runtime::CodingRuntimeEvent>,
        view: Option<SessionViewEvent>,
    ) -> SequencedSessionEvent {
        self.meta.activity = activity;
        let seq = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let event = SequencedSessionEvent {
            session_id: self.meta.session_id.clone(),
            generation: self.meta.generation,
            sequence: seq,
            activity,
            runtime,
            view,
        };
        if self.journal.len() >= JOURNAL_CAP {
            self.journal.pop_front();
        }
        self.journal.push_back(event.clone());
        let _ = self.event_tx.send(event.clone());
        event
    }
}

fn clip_key(prefix: &str, value: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 128);
    out.push_str(prefix);
    for (i, ch) in value.chars().enumerate() {
        if i >= 192 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn consecutive_runtime_key(event: &crate::runtime::CodingRuntimeEvent) -> Option<String> {
    use crate::runtime::CodingRuntimeEvent;
    use atomcode_kernel::event::AgentEvent;
    match event {
        CodingRuntimeEvent::Agent(AgentEvent::TextDelta(text)) => Some(clip_key("td:", text)),
        CodingRuntimeEvent::Agent(AgentEvent::Reasoning(text)) => Some(clip_key("rd:", text)),
        CodingRuntimeEvent::Agent(AgentEvent::ToolStarted { call }) => {
            Some(format!("ts:{}:{}", call.id, call.name))
        }
        CodingRuntimeEvent::Agent(AgentEvent::ToolProgress { call_id, message }) => {
            Some(format!("tp:{call_id}:{}", clip_key("", message)))
        }
        CodingRuntimeEvent::Agent(AgentEvent::ToolResult { result }) => {
            Some(format!("tr:{}:{}", result.call_id, clip_key("", &result.content)))
        }
        _ => None,
    }
}

fn activity_from_runtime_event(
    event: &crate::runtime::CodingRuntimeEvent,
    previous: RuntimeActivity,
) -> RuntimeActivity {
    use crate::runtime::{CodingRuntimeEvent, TurnCompletion};
    use atomcode_kernel::event::AgentEvent;
    match event {
        CodingRuntimeEvent::Agent(AgentEvent::TurnStarted)
        | CodingRuntimeEvent::Agent(AgentEvent::TextDelta(_))
        | CodingRuntimeEvent::Agent(AgentEvent::ToolCallStreaming { .. })
        | CodingRuntimeEvent::Agent(AgentEvent::ToolBatchStarted { .. })
        | CodingRuntimeEvent::Agent(AgentEvent::ToolStarted { .. })
        | CodingRuntimeEvent::Agent(AgentEvent::ToolResult { .. })
        | CodingRuntimeEvent::Agent(AgentEvent::Reasoning(_)) => RuntimeActivity::Running,
        CodingRuntimeEvent::Request(request) => {
            if request.kind
                == atomcode_capabilities::tools::request_user_input::REQUEST_USER_INPUT_KIND
            {
                RuntimeActivity::WaitingUserInput
            } else {
                RuntimeActivity::WaitingApproval
            }
        }
        CodingRuntimeEvent::TurnFinished(TurnCompletion::Completed { .. })
        | CodingRuntimeEvent::TurnFinished(TurnCompletion::SnapshotUnavailable { .. }) => {
            RuntimeActivity::Ready
        }
        CodingRuntimeEvent::RuntimeStopped(_) => RuntimeActivity::Stopped,
        CodingRuntimeEvent::Reconfiguring { .. } => RuntimeActivity::Reconfiguring,
        CodingRuntimeEvent::Reconfigured { .. } => RuntimeActivity::Ready,
        CodingRuntimeEvent::ProviderUnavailable { .. } => RuntimeActivity::Failed,
        _ => previous,
    }
}

/// L2 registry: one live runner per session within a process.
#[derive(Debug, Default)]
pub struct SessionRuntimeRegistry {
    entries: RwLock<HashMap<SessionKey, LiveInner>>,
}

/// Process-wide registry. Drivers attach views here instead of competing for
/// exclusive "foreground" ownership of a single CodingRuntime.
static GLOBAL_REGISTRY: OnceLock<SessionRuntimeRegistry> = OnceLock::new();

impl SessionRuntimeRegistry {
    pub fn global() -> &'static SessionRuntimeRegistry {
        GLOBAL_REGISTRY.get_or_init(SessionRuntimeRegistry::new)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, key: &SessionKey) -> Option<SessionRuntimeEntry> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|inner| inner.meta.clone())
    }

    pub fn activity(&self, key: &SessionKey) -> Option<RuntimeActivity> {
        self.lookup(key).map(|e| e.activity)
    }

    /// List sessions with live runners under `working_dir` (exact path match).
    pub fn list_activity(&self, working_dir: &Path) -> Vec<(SessionKey, RuntimeActivity)> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|e| e.meta.working_dir == working_dir)
            .map(|e| (e.meta.session_id.clone(), e.meta.activity))
            .collect()
    }

    /// All live runners (any working directory).
    pub fn list_all(&self) -> Vec<SessionRuntimeEntry> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| e.meta.clone())
            .collect()
    }

    /// Session ids whose **bound** runner is actually in a turn.
    ///
    /// `open_or_attach` / `subscribe_or_empty` leave handle-less `Starting`
    /// (or `InputAccepted` → `Running`) rows. Those are views, not occupancy.
    pub fn live_turn_session_ids(&self) -> Vec<SessionKey> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|e| e.handle.is_some() && e.meta.activity.is_live_turn())
            .map(|e| e.meta.session_id.clone())
            .collect()
    }

    pub fn live_count(&self) -> usize {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Register or attach to a session runner. At most one entry per `session_id`.
    /// Does **not** spawn a CodingRuntime — drivers call [`bind_handle`] after start.
    pub fn open_or_attach(
        &self,
        session_id: SessionKey,
        working_dir: PathBuf,
    ) -> Result<(OpenOutcome, SessionRuntimeEntry), RegistryError> {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&session_id) {
            return Ok((OpenOutcome::Attached, existing.meta.clone()));
        }
        if guard.len() >= MAX_LIVE_SESSIONS {
            return Err(RegistryError::LiveLimit {
                max: MAX_LIVE_SESSIONS,
            });
        }
        let inner = LiveInner::new(session_id.clone(), working_dir);
        let meta = inner.meta.clone();
        guard.insert(session_id, inner);
        Ok((OpenOutcome::Registered, meta))
    }

    /// Wire the authoritative CodingRuntime handle after deferred start succeeds.
    pub fn bind_handle(
        &self,
        key: &SessionKey,
        handle: CodingRuntimeHandle,
        runtime_id: Option<u64>,
    ) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        inner.handle = Some(handle);
        inner.meta.runtime_id = runtime_id;
        if matches!(
            inner.meta.activity,
            RuntimeActivity::Starting | RuntimeActivity::Stopped
        ) {
            inner.push_activity(RuntimeActivity::Ready);
        }
        true
    }

    pub fn set_activity(&self, key: &SessionKey, activity: RuntimeActivity) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        inner.push_activity(activity);
        true
    }

    pub fn set_snapshot(&self, key: &SessionKey, snapshot: SessionSnapshot) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        inner.meta.snapshot = Some(snapshot);
        true
    }

    pub fn snapshot(&self, key: &SessionKey) -> Option<SessionSnapshot> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .and_then(|e| e.meta.snapshot.clone())
    }

    pub fn pending_request_id(&self, key: &SessionKey) -> Option<RequestId> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .and_then(|e| e.pending_request_id)
    }

    pub fn set_pending_request(&self, key: &SessionKey, id: Option<RequestId>) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        inner.pending_request_id = id;
        if id.is_some() {
            inner.push_activity(RuntimeActivity::WaitingApproval);
        }
        true
    }

    /// Submit a turn to the session's bound handle.
    pub fn submit(&self, key: &SessionKey, input: UserInput) -> Result<(), RegistryError> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let inner = guard.get(key).ok_or(RegistryError::UnknownSession {
            session_id: key.clone(),
        })?;
        let handle = inner.handle.as_ref().ok_or(RegistryError::HandleNotReady {
            session_id: key.clone(),
        })?;
        handle
            .dispatch(DriverCommand::Submit(input))
            .map_err(|_| RegistryError::Unavailable {
                session_id: key.clone(),
            })?;
        drop(guard);
        self.set_activity(key, RuntimeActivity::Running);
        Ok(())
    }

    pub fn cancel(&self, key: &SessionKey) -> Result<(), RegistryError> {
        self.dispatch(key, DriverCommand::Cancel)
    }

    pub fn resolve_request(
        &self,
        key: &SessionKey,
        id: RequestId,
        value: serde_json::Value,
    ) -> Result<(), RegistryError> {
        self.dispatch(key, DriverCommand::Respond { id, value })?;
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = guard.get_mut(key) {
            if inner.pending_request_id == Some(id) {
                inner.pending_request_id = None;
            }
        }
        Ok(())
    }

    pub fn dispatch(&self, key: &SessionKey, command: DriverCommand) -> Result<(), RegistryError> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let inner = guard.get(key).ok_or(RegistryError::UnknownSession {
            session_id: key.clone(),
        })?;
        let handle = inner.handle.as_ref().ok_or(RegistryError::HandleNotReady {
            session_id: key.clone(),
        })?;
        handle
            .dispatch(command)
            .map_err(|_: RuntimeUnavailable| RegistryError::Unavailable {
                session_id: key.clone(),
            })
    }

    /// Append a CodingRuntime observation for multi-view fan-out.
    /// Safe to call from sync driver paths (std `RwLock`).
    pub fn push_runtime_event(
        &self,
        key: &SessionKey,
        generation: u64,
        event: crate::runtime::CodingRuntimeEvent,
    ) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        if let crate::runtime::CodingRuntimeEvent::TurnFinished(
            crate::runtime::TurnCompletion::Completed { snapshot, .. },
        ) = &event
        {
            inner.meta.snapshot = Some((**snapshot).clone());
        }
        if let crate::runtime::CodingRuntimeEvent::Request(request) = &event {
            inner.pending_request_id = Some(request.id);
        }
        if let crate::runtime::CodingRuntimeEvent::TurnFinished(_) = &event {
            inner.pending_request_id = None;
        }
        // Snapshot already has the completed turn. Replaying TextDelta / ToolStart
        // on top of it duplicates the last assistant (text + tools) after a
        // sidebar session switch reconnects `/live?session_id=`.
        if matches!(
            &event,
            crate::runtime::CodingRuntimeEvent::TurnFinished(_)
                | crate::runtime::CodingRuntimeEvent::RuntimeStopped(_)
        ) {
            inner.journal.clear();
            inner.recent_runtime_keys.clear();
        }
        if let Some(key) = consecutive_runtime_key(&event) {
            if inner.recent_runtime_keys.iter().any(|seen| seen == &key) {
                return false;
            }
            const RECENT_KEY_CAP: usize = 64;
            if inner.recent_runtime_keys.len() >= RECENT_KEY_CAP {
                inner.recent_runtime_keys.pop_front();
            }
            inner.recent_runtime_keys.push_back(key);
        }
        inner.push_runtime(generation, event);
        true
    }

    /// Fan out a view-layer event (InputAccepted / Steered / RequestResolved / CommandOutput).
    pub fn push_view_event(&self, key: &SessionKey, view: SessionViewEvent) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let Some(inner) = guard.get_mut(key) else {
            return false;
        };
        if let SessionViewEvent::RequestResolved { request_id, .. } = &view {
            if inner.pending_request_id == Some(*request_id) {
                inner.pending_request_id = None;
            }
        }
        inner.push_view(view);
        true
    }

    /// Ensure the session exists, then push a view event.
    pub fn ensure_and_push_view(
        &self,
        key: SessionKey,
        working_dir: PathBuf,
        view: SessionViewEvent,
    ) -> bool {
        let _ = self.open_or_attach(key.clone(), working_dir);
        self.push_view_event(&key, view)
    }

    /// Push by TUI transport runtime id (foreground / background runners).
    pub fn push_runtime_event_by_runtime_id(
        &self,
        runtime_id: u64,
        generation: u64,
        event: crate::runtime::CodingRuntimeEvent,
    ) -> bool {
        let key = {
            let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
            guard
                .values()
                .find(|e| e.meta.runtime_id == Some(runtime_id))
                .map(|e| e.meta.session_id.clone())
        };
        let Some(key) = key else {
            return false;
        };
        self.push_runtime_event(&key, generation, event)
    }

    /// Bound CodingRuntime handle for a session, if ready.
    pub fn handle(&self, key: &SessionKey) -> Option<CodingRuntimeHandle> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard.get(key).and_then(|e| e.handle.clone())
    }

    /// Look up session id by TUI transport runtime id.
    pub fn session_id_for_runtime_id(&self, runtime_id: u64) -> Option<SessionKey> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard
            .values()
            .find(|e| e.meta.runtime_id == Some(runtime_id))
            .map(|e| e.meta.session_id.clone())
    }

    /// Subscribe to activity events for a session (after `after_sequence`).
    pub fn subscribe(
        &self,
        key: &SessionKey,
        after_sequence: Option<u64>,
    ) -> Result<
        (
            Vec<SequencedSessionEvent>,
            broadcast::Receiver<SequencedSessionEvent>,
        ),
        RegistryError,
    > {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let inner = guard.get(key).ok_or(RegistryError::UnknownSession {
            session_id: key.clone(),
        })?;
        let replay: Vec<_> = inner
            .journal
            .iter()
            .filter(|e| after_sequence.is_none_or(|after| e.sequence > after))
            .cloned()
            .collect();
        let rx = inner.event_tx.subscribe();
        Ok((replay, rx))
    }

    /// Subscribe even when the session is not yet registered — returns empty
    /// replay and a channel that will receive events after `open_or_attach`.
    /// Prefer [`subscribe`] when the session is known live.
    pub fn subscribe_or_empty(
        &self,
        key: &SessionKey,
        working_dir: PathBuf,
        after_sequence: Option<u64>,
    ) -> Result<
        (
            Vec<SequencedSessionEvent>,
            broadcast::Receiver<SequencedSessionEvent>,
        ),
        RegistryError,
    > {
        match self.subscribe(key, after_sequence) {
            Ok(pair) => Ok(pair),
            Err(RegistryError::UnknownSession { .. }) => {
                self.open_or_attach(key.clone(), working_dir)?;
                self.subscribe(key, after_sequence)
            }
            Err(other) => Err(other),
        }
    }

    pub fn release_if_idle(&self, key: &SessionKey) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        match guard.get(key) {
            Some(entry) if entry.meta.activity.is_idle_releasable() => {
                if let Some(handle) = entry.handle.as_ref() {
                    let _ = handle.dispatch(DriverCommand::Shutdown);
                }
                guard.remove(key);
                true
            }
            _ => false,
        }
    }

    /// Force-remove an entry (e.g. view closed an empty home draft that was registered).
    pub fn force_remove(&self, key: &SessionKey) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.remove(key) {
            if let Some(handle) = entry.handle.as_ref() {
                let _ = handle.dispatch(DriverCommand::Shutdown);
            }
            true
        } else {
            false
        }
    }

    /// Drop the registry row without shutting the runtime down. Used when the
    /// bound handle has already been freshed onto a new session id.
    pub fn detach(&self, key: &SessionKey) -> bool {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        guard.remove(key).is_some()
    }

    pub fn shutdown_all(&self) {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        for (_, entry) in guard.drain() {
            if let Some(handle) = entry.handle.as_ref() {
                let _ = handle.dispatch(DriverCommand::Shutdown);
            }
        }
    }

    /// Reconcile registry membership with the set of session ids the driver still owns.
    /// Unlike the old slot-mirror helper, this does **not** invent entries — it only
    /// drops idle registry rows that the driver no longer tracks.
    pub fn retain_sessions(&self, keep: &HashSet<SessionKey>) {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let stale: Vec<SessionKey> = guard
            .keys()
            .filter(|id| !keep.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            if guard
                .get(&id)
                .is_some_and(|e| e.meta.activity.is_idle_releasable())
            {
                if let Some(entry) = guard.remove(&id) {
                    if let Some(handle) = entry.handle.as_ref() {
                        let _ = handle.dispatch(DriverCommand::Shutdown);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    UnknownSession { session_id: SessionKey },
    HandleNotReady { session_id: SessionKey },
    Unavailable { session_id: SessionKey },
    LiveLimit { max: usize },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSession { session_id } => {
                write!(f, "session {session_id} is not in the runtime registry")
            }
            Self::HandleNotReady { session_id } => {
                write!(f, "session {session_id} runtime handle is not ready")
            }
            Self::Unavailable { session_id } => {
                write!(f, "session {session_id} coding runtime is unavailable")
            }
            Self::LiveLimit { max } => {
                write!(f, "too many live sessions (max {max})")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_or_attach_is_idempotent_per_session() {
        let reg = SessionRuntimeRegistry::new();
        let (o1, e1) = reg
            .open_or_attach("s1".into(), PathBuf::from("/proj"))
            .unwrap();
        assert_eq!(o1, OpenOutcome::Registered);
        let (o2, e2) = reg
            .open_or_attach("s1".into(), PathBuf::from("/proj"))
            .unwrap();
        assert_eq!(o2, OpenOutcome::Attached);
        assert_eq!(e1.session_id, e2.session_id);
    }

    #[tokio::test]
    async fn list_activity_scopes_by_working_dir() {
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("a".into(), PathBuf::from("/p1"))
            .unwrap();
        reg.open_or_attach("b".into(), PathBuf::from("/p2"))
            .unwrap();
        let p1 = reg.list_activity(Path::new("/p1"));
        assert_eq!(p1.len(), 1);
        assert_eq!(p1[0].0, "a");
    }

    #[tokio::test]
    async fn select_semantics_do_not_require_reconfigure() {
        // Registry holds A and B independently; "view switch" is a client concern.
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("a".into(), PathBuf::from("/p")).unwrap();
        reg.open_or_attach("b".into(), PathBuf::from("/p")).unwrap();
        reg.set_activity(&"a".into(), RuntimeActivity::Running);
        assert_eq!(reg.activity(&"a".into()), Some(RuntimeActivity::Running));
        assert_eq!(reg.activity(&"b".into()), Some(RuntimeActivity::Starting));
        // Switching the view to B must not clear A's busy state.
        assert!(reg.activity(&"a".into()).unwrap().is_busy());
    }

    #[tokio::test]
    async fn retain_sessions_drops_idle_stale_only() {
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("keep".into(), PathBuf::from("/p"))
            .unwrap();
        reg.open_or_attach("drop".into(), PathBuf::from("/p"))
            .unwrap();
        reg.set_activity(&"drop".into(), RuntimeActivity::Ready);
        reg.set_activity(&"keep".into(), RuntimeActivity::Running);
        let mut keep = HashSet::new();
        keep.insert("keep".into());
        reg.retain_sessions(&keep);
        assert!(reg.lookup(&"keep".into()).is_some());
        assert!(reg.lookup(&"drop".into()).is_none());
    }

    #[tokio::test]
    async fn subscribe_replays_journal_after_sequence() {
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        reg.set_activity(&"s".into(), RuntimeActivity::Ready);
        reg.set_activity(&"s".into(), RuntimeActivity::Running);
        let (replay, _rx) = reg.subscribe(&"s".into(), Some(0)).unwrap();
        assert!(replay
            .iter()
            .any(|e| e.activity == RuntimeActivity::Running));
    }

    #[tokio::test]
    async fn turn_finished_clears_delta_journal_so_reconnect_does_not_duplicate() {
        use atomcode_kernel::event::AgentEvent;
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        assert!(reg.push_runtime_event(
            &"s".into(),
            1,
            crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta("hi".into())),
        ));
        assert!(reg.push_runtime_event(
            &"s".into(),
            1,
            crate::runtime::CodingRuntimeEvent::TurnFinished(
                crate::runtime::TurnCompletion::SnapshotUnavailable {
                    turn_id: 1,
                    reason: atomcode_kernel::event::StopReason::Stopped,
                    error: crate::runtime::RuntimeSnapshotError {
                        message: "done".into(),
                    },
                    stats: crate::runtime::RuntimeTurnStats {
                        last_usage: None,
                        duration: std::time::Duration::from_millis(1),
                        turn_count: 1,
                        tool_call_count: 0,
                    },
                },
            ),
        ));
        let (replay, _rx) = reg.subscribe(&"s".into(), None).unwrap();
        assert!(
            replay.iter().all(|event| !matches!(
                event.runtime,
                Some(crate::runtime::CodingRuntimeEvent::Agent(
                    AgentEvent::TextDelta(_)
                ))
            )),
            "completed-turn deltas must not be replayed on the next live join"
        );
        assert!(replay.iter().any(|event| matches!(
            event.runtime,
            Some(crate::runtime::CodingRuntimeEvent::TurnFinished(_))
        )));
    }

    #[tokio::test]
    async fn push_runtime_event_fans_out_text_delta_to_subscribers() {
        use atomcode_kernel::event::AgentEvent;
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        let (replay_before, mut rx) = reg.subscribe(&"s".into(), None).unwrap();
        assert!(replay_before
            .iter()
            .all(|e| e.runtime.is_none() && e.view.is_none()));
        assert!(reg.push_runtime_event(
            &"s".into(),
            1,
            crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta("hi".into())),
        ));
        let got = rx.recv().await.expect("fan-out");
        match got.runtime {
            Some(crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta(text))) => {
                assert_eq!(text, "hi");
            }
            other => panic!("expected TextDelta, got {other:?}"),
        }
        assert_eq!(got.activity, RuntimeActivity::Running);
    }

    #[tokio::test]
    async fn push_runtime_event_drops_consecutive_duplicate_agent_observations() {
        use atomcode_kernel::event::AgentEvent;
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        let (_replay, mut rx) = reg.subscribe(&"s".into(), None).unwrap();
        let delta = crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta(
            "正在查看 grok-build".into(),
        ));
        assert!(reg.push_runtime_event(&"s".into(), 1, delta.clone()));
        assert!(
            !reg.push_runtime_event(&"s".into(), 1, delta),
            "observer echo of the same TextDelta must not re-fanout"
        );
        let first = rx.recv().await.expect("first fan-out");
        assert!(matches!(
            first.runtime,
            Some(crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta(_)))
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), rx.recv())
                .await
                .is_err(),
            "duplicate must not reach subscribers"
        );
        assert!(reg.push_runtime_event(
            &"s".into(),
            1,
            crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta("下一句".into())),
        ));
    }

    #[tokio::test]
    async fn push_runtime_event_breaks_alternating_observer_echo() {
        use atomcode_kernel::event::AgentEvent;
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        let a = crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta(
            "正在查看并发与锁".into(),
        ));
        let b = crate::runtime::CodingRuntimeEvent::Agent(AgentEvent::TextDelta(
            "正在查看 FileOperationLockManager".into(),
        ));
        assert!(reg.push_runtime_event(&"s".into(), 1, a.clone()));
        assert!(reg.push_runtime_event(&"s".into(), 1, b.clone()));
        assert!(!reg.push_runtime_event(&"s".into(), 1, a));
        assert!(!reg.push_runtime_event(&"s".into(), 1, b));
    }

    #[tokio::test]
    async fn push_view_event_fans_out_input_accepted() {
        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("s".into(), PathBuf::from("/p")).unwrap();
        let (_replay, mut rx) = reg.subscribe(&"s".into(), None).unwrap();
        assert!(reg.push_view_event(
            &"s".into(),
            SessionViewEvent::InputAccepted {
                input: UserInput::from("hello"),
                client_input_id: Some("c1".into()),
            },
        ));
        let got = rx.recv().await.expect("view fan-out");
        match got.view {
            Some(SessionViewEvent::InputAccepted {
                input,
                client_input_id,
            }) => {
                assert_eq!(input.text, "hello");
                assert_eq!(client_input_id.as_deref(), Some("c1"));
            }
            other => panic!("expected InputAccepted, got {other:?}"),
        }
    }

    #[test]
    fn view_only_rows_are_not_live_turns() {
        assert!(!RuntimeActivity::Starting.is_live_turn());
        assert!(!RuntimeActivity::Ready.is_live_turn());
        assert!(!RuntimeActivity::Reconfiguring.is_live_turn());
        assert!(RuntimeActivity::Running.is_live_turn());
        assert!(RuntimeActivity::WaitingApproval.is_live_turn());
        assert!(RuntimeActivity::WaitingUserInput.is_live_turn());

        let reg = SessionRuntimeRegistry::new();
        reg.open_or_attach("a".into(), PathBuf::from("/p")).unwrap();
        assert!(
            reg.live_turn_session_ids().is_empty(),
            "subscribe/open_or_attach Starting must not occupy /chat/active"
        );
        reg.set_activity(&"a".into(), RuntimeActivity::Running);
        assert!(
            reg.live_turn_session_ids().is_empty(),
            "InputAccepted without a bound handle must not occupy /chat/active"
        );
    }

    #[test]
    fn request_user_input_activity_is_not_approval() {
        use crate::runtime::RuntimeRequest;
        let user_input = crate::runtime::CodingRuntimeEvent::Request(RuntimeRequest {
            id: 1,
            kind: atomcode_capabilities::tools::request_user_input::REQUEST_USER_INPUT_KIND.into(),
            payload: serde_json::json!({}),
            snapshot: None,
        });
        assert_eq!(
            activity_from_runtime_event(&user_input, RuntimeActivity::Running),
            RuntimeActivity::WaitingUserInput
        );
        let approval = crate::runtime::CodingRuntimeEvent::Request(RuntimeRequest {
            id: 2,
            kind: "approval".into(),
            payload: serde_json::json!({}),
            snapshot: None,
        });
        assert_eq!(
            activity_from_runtime_event(&approval, RuntimeActivity::Running),
            RuntimeActivity::WaitingApproval
        );
    }
}
