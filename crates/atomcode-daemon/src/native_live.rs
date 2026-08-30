use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use atomcode_coding::{
    CodingRuntime, CodingRuntimeConfig, DriverCommand, RuntimeMode, RuntimePhase, UserInput,
};
use atomcode_kernel::message::SessionSnapshot;
use atomcode_telemetry::Telemetry;
use tokio::sync::Mutex;

use crate::live_hub::{HubError, LiveBinding, LiveJoin, LiveRuntimeControl, LiveViewHub};

static HUB: OnceLock<Arc<LiveViewHub>> = OnceLock::new();
static EMBEDDED_BINDING: StdMutex<Option<LiveBinding>> = StdMutex::new(None);
static HEADLESS: OnceLock<Mutex<Option<HeadlessRuntime>>> = OnceLock::new();
static REMOTE_COMMAND: StdMutex<Option<tokio::sync::mpsc::UnboundedSender<String>>> =
    StdMutex::new(None);

struct HeadlessRuntime {
    binding: LiveBinding,
    handle: atomcode_coding::CodingRuntimeHandle,
}

fn hub() -> &'static Arc<LiveViewHub> {
    HUB.get_or_init(|| Arc::new(LiveViewHub::new()))
}

/// Live hub session currently in a turn (TUI / `--host` / `/webui` sync).
/// `GET /chat/active` unions this so the WebUI sidebar can spin for live tasks.
pub fn live_running_session_id() -> Option<String> {
    hub().running_session_id()
}

fn headless() -> &'static Mutex<Option<HeadlessRuntime>> {
    HEADLESS.get_or_init(|| Mutex::new(None))
}

pub fn register_embedded_runtime(
    session_id: String,
    working_dir: PathBuf,
    provider: String,
    provider_fingerprint: String,
    snapshot: SessionSnapshot,
    control: Arc<dyn LiveRuntimeControl>,
) -> Result<LiveBinding, HubError> {
    let headless_owner = headless()
        .try_lock()
        .map_err(|_| HubError::RuntimeUnavailable)?;
    if headless_owner.is_some() {
        return Err(HubError::RuntimeUnavailable);
    }
    let binding = hub().bind_with_provider(
        session_id,
        working_dir,
        provider,
        provider_fingerprint,
        snapshot,
        control,
    )?;
    *EMBEDDED_BINDING
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(binding.clone());
    // Seed the daemon's project state to the shared TUI's working dir so the webui footer
    // + session list match it. A no-op if the server hasn't started yet (DAEMON_PROJECT is
    // None) — that case is covered by `init_project_state` reading the embedded binding at
    // startup; this call handles an ALREADY-running (persistent) daemon, where the embed
    // happens after init. `/cd` keeps it current afterward via the same `live_set_working_dir`.
    crate::live_set_working_dir(binding.working_dir.clone());
    Ok(binding)
}

pub fn embedded_binding() -> Option<LiveBinding> {
    EMBEDDED_BINDING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

pub fn unregister_embedded_runtime(binding: &LiveBinding) -> Result<(), HubError> {
    hub().unbind(binding)?;
    let mut embedded = EMBEDDED_BINDING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if embedded
        .as_ref()
        .is_some_and(|current| current.id == binding.id)
    {
        *embedded = None;
        *REMOTE_COMMAND
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }
    Ok(())
}

pub fn register_remote_command_sink() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    *REMOTE_COMMAND
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(sender);
    receiver
}

pub fn send_remote_command(command: String) -> bool {
    REMOTE_COMMAND
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|sender| sender.send(command).is_ok())
}

/// True when `/live` should follow `SessionRuntimeRegistry` instead of the
/// single LiveHub binding (OpenCode multi-view: hub may be on A while client watches B).
static SESSION_DRAFTS: OnceLock<StdMutex<HashMap<String, PathBuf>>> = OnceLock::new();

fn session_drafts() -> &'static StdMutex<HashMap<String, PathBuf>> {
    SESSION_DRAFTS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Remember a `/sessions` draft id that has no catalog row yet.
pub fn register_session_draft(session_id: String, working_dir: PathBuf) {
    session_drafts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(session_id, working_dir);
}

pub fn take_session_draft(session_id: &str) -> Option<PathBuf> {
    session_drafts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(session_id)
}

pub fn is_session_draft(session_id: &str) -> bool {
    session_drafts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(session_id)
}

/// Spawn (or attach) a CodingRuntime into the L2 registry without claiming the
/// LiveHub binding — used when the hub/TUI is already bound to another session.
pub async fn ensure_registry_runner(
    working_dir: PathBuf,
    telemetry: Arc<Telemetry>,
    provider_name: String,
    mode: RuntimeMode,
    session_id: String,
) -> Result<(), String> {
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    if reg.handle(&session_id).is_some() {
        return Ok(());
    }

    let config =
        atomcode_config::config::Config::load(&atomcode_config::config::Config::default_path())
            .map_err(|error| error.to_string())?;
    if !config.selection_exists(&provider_name) {
        return Err(format!("provider {provider_name:?} not found"));
    }
    let runtime_config: CodingRuntimeConfig =
        crate::live_api::live_runtime_config(&config, &provider_name, &working_dir, telemetry);

    let session_mode = if let Ok(snapshot) = load_snapshot(&working_dir, &session_id) {
        atomcode_coding::SessionMode::ExternalSnapshot {
            id: session_id.clone(),
            snapshot,
        }
    } else {
        // Draft or brand-new id: staged until first Submit.
        let _ = take_session_draft(&session_id);
        atomcode_coding::SessionMode::Draft {
            id: session_id.clone(),
        }
    };

    let (runtime, _) = crate::start_native_runtime_with_session(runtime_config, session_mode)
        .await
        .map_err(|error| error.to_string())?;
    let CodingRuntime {
        handle,
        mut events,
        task,
        ..
    } = runtime;
    handle
        .set_mode(mode)
        .await
        .map_err(|error| format!("failed to set registry runner mode: {error}"))?;
    let _ = handle
        .wait_mcp_ready(atomcode_capabilities::mcp::CONNECT_TIMEOUT)
        .await;

    let _ = reg.open_or_attach(session_id.clone(), working_dir.clone());
    let _ = reg.bind_handle(&session_id, handle.clone(), None);

    let forward_id = session_id.clone();
    let forward_dir = working_dir;
    tokio::spawn(async move {
        let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
        let _ = reg.open_or_attach(forward_id.clone(), forward_dir);
        while let Some(envelope) = events.recv().await {
            let _ = reg.push_runtime_event(&forward_id, envelope.generation, envelope.event);
        }
        let _ = task.await;
    });
    Ok(())
}

pub fn prefer_registry_live_stream(requested_session_id: &str) -> bool {
    if is_session_draft(requested_session_id) {
        return true;
    }
    if let Ok(current) = join() {
        if current.binding.session_id != requested_session_id {
            return true;
        }
        return false;
    }
    atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
        .lookup(&requested_session_id.to_string())
        .is_some()
}

/// Snapshot for a registry-backed live view (memory first, then disk catalog).
pub fn registry_session_snapshot(
    working_dir: &Path,
    session_id: &str,
) -> Result<SessionSnapshot, String> {
    if let Some(snap) =
        atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
            .snapshot(&session_id.to_string())
    {
        return Ok(snap);
    }
    match load_snapshot(working_dir, session_id) {
        Ok(snapshot) => Ok(snapshot),
        Err(_) if is_session_draft(session_id) => Ok(SessionSnapshot::new(Vec::new())),
        Err(error) => Err(error),
    }
}

/// Submit to a session owned by the L2 registry (TUI background / non-hub view).
pub async fn submit_via_registry(
    session_id: &str,
    runtime_input: UserInput,
    echo_input: UserInput,
    client_input_id: Option<String>,
) -> Result<atomcode_coding::SubmitReceipt, String> {
    let handle =
        atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
            .handle(&session_id.to_string())
            .ok_or_else(|| {
                format!("session {session_id} has no live registry handle for submit")
            })?;
    let receipt = handle
        .submit(runtime_input)
        .await
        .map_err(|error| format!("registry submit failed: {error}"))?;
    let working_dir = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
        .lookup(&session_id.to_string())
        .map(|e| e.working_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
        .ensure_and_push_view(
            session_id.to_string(),
            working_dir,
            atomcode_coding::session_runtime_registry::SessionViewEvent::InputAccepted {
                input: echo_input,
                client_input_id,
            },
        );
    Ok(receipt)
}

/// Cancel the turn for a registry-owned session.
pub fn cancel_via_registry(session_id: &str) -> Result<(), String> {
    atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global()
        .cancel(&session_id.to_string())
        .map_err(|error| error.to_string())
}

/// Deliver a Respond for a pending request on a registry-owned session.
pub fn resolve_via_registry(
    session_id: &str,
    id: atomcode_kernel::event::RequestId,
    value: serde_json::Value,
    kind: &str,
) -> Result<(), String> {
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    reg.resolve_request(&session_id.to_string(), id, value)
        .map_err(|error| error.to_string())?;
    let working_dir = reg
        .lookup(&session_id.to_string())
        .map(|e| e.working_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = reg.ensure_and_push_view(
        session_id.to_string(),
        working_dir,
        atomcode_coding::session_runtime_registry::SessionViewEvent::RequestResolved {
            request_id: id,
            kind: kind.to_string(),
        },
    );
    Ok(())
}

/// Respond to the latest pending request on a registry session.
pub fn resolve_pending_kind_via_registry(
    session_id: &str,
    kind: &str,
    value: serde_json::Value,
) -> Result<u64, String> {
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    let id = reg
        .pending_request_id(&session_id.to_string())
        .ok_or_else(|| format!("session {session_id} has no pending {kind} request"))?;
    resolve_via_registry(session_id, id, value, kind)?;
    Ok(id)
}

pub fn publish(
    binding: &LiveBinding,
    event: atomcode_coding::SequencedRuntimeEvent,
) -> Result<(), HubError> {
    // Dual-write: hub (bound view) + registry (any view of this session).
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    let _ = reg.open_or_attach(binding.session_id.clone(), binding.working_dir.clone());
    let _ = reg.push_runtime_event(&binding.session_id, event.generation, event.event.clone());
    hub().publish(binding, event)
}

pub fn publish_unsequenced(
    binding: &LiveBinding,
    event: atomcode_coding::CodingRuntimeEvent,
) -> Result<(), HubError> {
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    let _ = reg.open_or_attach(binding.session_id.clone(), binding.working_dir.clone());
    let _ = reg.push_runtime_event(&binding.session_id, 0, event.clone());
    hub().publish_unsequenced(binding, event)
}

pub fn join() -> Result<LiveJoin, HubError> {
    hub().join()
}

pub fn join_for_provider(expected_session_id: Option<&str>) -> Result<LiveJoin, HubError> {
    hub().join_for_provider(expected_session_id)
}

pub fn binding() -> Result<LiveBinding, HubError> {
    hub().binding()
}

pub fn submit(input: UserInput) -> Result<(), HubError> {
    hub().submit(input)
}

pub async fn submit_confirmed(
    input: UserInput,
) -> Result<atomcode_coding::SubmitReceipt, HubError> {
    hub().submit_confirmed(input).await
}

/// Submit `runtime_input` to the model while echoing `echo_input` to the live view
/// (see [`crate::live_hub::LiveViewHub::submit_confirmed_with_echo`]). Used by the
/// webui image path so the VL caption feeds the model but the user's original
/// message + image is what displays.
pub async fn submit_confirmed_with_echo(
    runtime_input: UserInput,
    echo_input: UserInput,
    client_input_id: Option<String>,
) -> Result<atomcode_coding::SubmitReceipt, HubError> {
    hub()
        .submit_confirmed_with_echo(runtime_input, echo_input, client_input_id)
        .await
}

pub fn accept_local_input(input: UserInput) -> Result<(), HubError> {
    hub().accept_local_input(input)
}

pub fn respond(
    id: atomcode_kernel::event::RequestId,
    value: serde_json::Value,
) -> Result<(), HubError> {
    hub().respond(id, value)
}

pub fn respond_pending_kind(kind: &str, value: serde_json::Value) -> Result<u64, HubError> {
    hub().respond_pending_kind(kind, value)
}

pub async fn respond_confirmed(
    id: atomcode_kernel::event::RequestId,
    value: serde_json::Value,
) -> Result<(), HubError> {
    hub().respond_confirmed(id, value).await
}

pub async fn respond_pending_kind_confirmed(
    kind: &str,
    value: serde_json::Value,
) -> Result<u64, HubError> {
    hub().respond_pending_kind_confirmed(kind, value).await
}

pub fn cancel() -> Result<(), HubError> {
    hub().cancel()
}

pub async fn cancel_confirmed() -> Result<(), HubError> {
    hub().cancel_confirmed().await
}

pub fn dispatch(command: DriverCommand) -> Result<(), HubError> {
    hub().dispatch(command)
}

pub async fn set_mode(mode: RuntimeMode) -> Result<(), HubError> {
    hub().set_mode(mode).await
}

pub async fn reload_provider(
    expected: &LiveBinding,
    next: atomcode_coding::CodingAgentConfig,
    provider_fingerprint: String,
) -> Result<atomcode_coding::RuntimeGeneration, HubError> {
    hub()
        .reload_provider(expected, next, provider_fingerprint)
        .await
}

pub fn provider_fingerprint(
    config: &atomcode_config::config::Config,
    provider_name: &str,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    if !config.selection_exists(provider_name) {
        return Err(format!("provider {provider_name:?} not found"));
    }
    let mut normalized = config.clone();
    normalized.default_provider = provider_name.to_string();
    // Serialize through Value so map keys are canonicalized before hashing;
    // Config contains HashMaps whose iteration order differs across processes.
    let canonical = serde_json::to_value(&normalized)
        .map_err(|error| format!("serialize provider configuration failed: {error}"))?;
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| format!("serialize provider configuration failed: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub async fn resume_session(
    session_id: String,
) -> Result<atomcode_coding::SessionChanged, HubError> {
    let binding = hub().binding()?;
    if binding.session_id == session_id {
        return Ok(atomcode_coding::SessionChanged {
            generation: atomcode_coding::RuntimeGeneration(binding.generation),
            session_id: Some(binding.session_id),
            working_dir: binding.working_dir,
        });
    }
    let project_bucket =
        atomcode_capabilities::session::SessionManager::project_hash(&binding.working_dir);
    let prepared = match crate::legacy_convert::prepare_catalog_session_resume_in_project(
        &project_bucket,
        &session_id,
    ) {
        Ok(Some(prepared)) => prepared,
        _ => crate::legacy_convert::prepare_catalog_session_resume_any_project(&session_id)
            .map_err(|error| HubError::RuntimeRejected(error.to_string()))?
            .ok_or_else(|| {
                HubError::RuntimeRejected(format!("session {session_id:?} not found in catalog"))
            })?,
    };
    let target_dir = PathBuf::from(&prepared.view.meta.working_dir);
    // OpenCode model: switching the live WebUI/TUI *view* never reconfigures the
    // bound CodingRuntime. Execution stays on its session; the hub only projects
    // another transcript. (Provider reload still refuses ActiveTurn.)
    let _lease = prepared.lease;
    hub().switch_view_only(session_id, target_dir, prepared.view.snapshot)
}

/// Move the bound runtime to a fresh staged session. This is the only safe way
/// for the daemon to release the current idle session's lease before deleting
/// that session from disk.
pub async fn fresh_session(
    expected: &LiveBinding,
) -> Result<crate::live_hub::FreshSessionOutcome, HubError> {
    hub().fresh_session(expected).await
}

pub async fn change_directory(
    working_dir: PathBuf,
) -> Result<atomcode_coding::SessionChanged, HubError> {
    hub().change_directory(working_dir).await
}

pub async fn reload_capabilities() -> Result<atomcode_coding::SessionChanged, HubError> {
    hub().reload_capabilities().await
}

pub fn publish_command_output(text: String) -> Result<(), HubError> {
    hub().publish_command_output(text)
}

pub fn replace_snapshot(
    binding: &LiveBinding,
    session_id: String,
    working_dir: PathBuf,
    snapshot: SessionSnapshot,
) -> Result<LiveBinding, HubError> {
    let next = hub().replace_snapshot(binding, session_id, working_dir, snapshot)?;
    let mut embedded = EMBEDDED_BINDING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if embedded
        .as_ref()
        .is_some_and(|current| current.id == binding.id)
    {
        *embedded = Some(next.clone());
    }
    Ok(next)
}

pub fn commit_runtime_snapshot(
    binding: &LiveBinding,
    session_id: String,
    working_dir: PathBuf,
    snapshot: SessionSnapshot,
) -> Result<LiveBinding, HubError> {
    let next = hub().commit_runtime_snapshot(binding, session_id, working_dir, snapshot)?;
    let mut embedded = EMBEDDED_BINDING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if embedded
        .as_ref()
        .is_some_and(|current| current.id == binding.id)
    {
        *embedded = Some(next.clone());
    }
    Ok(next)
}

fn load_snapshot(working_dir: &Path, session_id: &str) -> Result<SessionSnapshot, String> {
    let bucket = atomcode_capabilities::session::SessionManager::project_hash(working_dir);
    crate::legacy_convert::load_catalog_session_view_in_project(&bucket, session_id)
        .map_err(|error| error.to_string())?
        .map(|session| session.snapshot)
        .ok_or_else(|| format!("session {session_id:?} not found"))
}

async fn bind_after_mcp_ready<T, E>(
    readiness: impl std::future::Future<Output = Result<(), E>>,
    bind: impl FnOnce() -> Result<T, String>,
) -> Result<T, String>
where
    E: std::fmt::Debug,
{
    readiness
        .await
        .map_err(|error| format!("MCP readiness wait failed: {error:?}"))?;
    bind()
}

pub async fn ensure_headless_runtime(
    working_dir: PathBuf,
    telemetry: Arc<Telemetry>,
    provider_name: String,
    mode: RuntimeMode,
    requested_session_id: Option<String>,
) -> Result<LiveJoin, String> {
    if let Some(binding) = embedded_binding() {
        if requested_session_id
            .as_deref()
            .is_some_and(|requested| requested != binding.session_id)
        {
            return Err(format!(
                "embedded runtime is bound to session {:?}, requested {:?}",
                binding.session_id,
                requested_session_id.as_deref().unwrap_or_default()
            ));
        }
        return join().map_err(|error| format!("live hub join failed: {error:?}"));
    }

    let mut owner = headless().lock().await;
    let can_reuse = owner.is_some()
        && join().is_ok_and(|current| {
            current.binding.working_dir == working_dir
                && requested_session_id
                    .as_deref()
                    .is_none_or(|requested| requested == current.binding.session_id)
        });
    if can_reuse {
        return join().map_err(|error| format!("live hub join failed: {error:?}"));
    }

    if let Some(old) = owner.take() {
        if matches!(
            old.handle.status().phase,
            RuntimePhase::InTurn | RuntimePhase::WaitingApproval | RuntimePhase::Reconfiguring
        ) {
            *owner = Some(old);
            return Err("cannot replace an active live runtime".into());
        }
        old.handle
            .shutdown()
            .await
            .map_err(|_| "failed to stop previous live runtime".to_string())?;
        let _ = hub().unbind(&old.binding);
    }

    let config =
        atomcode_config::config::Config::load(&atomcode_config::config::Config::default_path())
            .map_err(|error| error.to_string())?;
    if !config.selection_exists(&provider_name) {
        return Err(format!("provider {provider_name:?} not found"));
    }
    let provider_fingerprint = provider_fingerprint(&config, &provider_name)?;
    let runtime_config: CodingRuntimeConfig =
        crate::live_api::live_runtime_config(&config, &provider_name, &working_dir, telemetry);
    let (session_mode, initial_snapshot) = match requested_session_id {
        Some(id) => {
            let snapshot = load_snapshot(&working_dir, &id)?;
            (
                atomcode_coding::SessionMode::ExternalSnapshot {
                    id,
                    snapshot: snapshot.clone(),
                },
                snapshot,
            )
        }
        None => (
            atomcode_coding::SessionMode::Fresh,
            SessionSnapshot::new(Vec::new()),
        ),
    };
    let (runtime, _) = crate::start_native_runtime_with_session(runtime_config, session_mode)
        .await
        .map_err(|error| error.to_string())?;
    let CodingRuntime {
        handle,
        mut events,
        task,
        session,
        ..
    } = runtime;
    handle
        .set_mode(mode)
        .await
        .map_err(|error| format!("failed to set live mode: {error}"))?;

    // Wait for initial MCP tools to be published to the mounted kernel catalog
    // before the first turn. Without this, a headless
    // runtime created by `atomcode.exe webui` (which has no pre-existing
    // CodingRuntime from the TUI) would start its first turn before background
    // MCP connections complete, making MCP tools invisible to the agent even
    // though `/mcp/status` shows them as connected.
    // Timeout prevents a stalled MCP server from blocking the first message.
    let session_id = session
        .map(|session| session.id)
        .ok_or_else(|| "live runtime started without a persistent session".to_string())?;
    let binding = bind_after_mcp_ready(
        handle.wait_mcp_ready(atomcode_capabilities::mcp::CONNECT_TIMEOUT),
        || {
            hub()
                .bind_with_provider(
                    session_id.clone(),
                    working_dir.clone(),
                    provider_name,
                    provider_fingerprint,
                    initial_snapshot,
                    Arc::new(handle.clone()),
                )
                .map_err(|error| format!("live hub bind failed: {error:?}"))
        },
    )
    .await?;
    let event_binding = binding.clone();
    let event_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let session_change = match &event.event {
                atomcode_coding::CodingRuntimeEvent::SessionChanged(changed) => {
                    Some((changed.session_id.clone(), changed.working_dir.clone()))
                }
                _ => None,
            };
            match publish(&event_binding, event) {
                Ok(()) => {}
                Err(HubError::StaleEvent) => {
                    tracing::warn!("discarded stale live runtime event");
                    continue;
                }
                Err(error) => {
                    tracing::warn!("stopping live event forwarding: {error:?}");
                    break;
                }
            }
            if let Some((Some(session_id), working_dir)) = session_change {
                match event_handle.snapshot().await {
                    Ok(snapshot) => {
                        if let Err(error) = hub().commit_runtime_snapshot(
                            &event_binding,
                            session_id,
                            working_dir,
                            snapshot.as_ref().clone(),
                        ) {
                            tracing::warn!("live session snapshot commit failed: {error:?}");
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!("live runtime session snapshot unavailable: {error}");
                    }
                }
            }
        }
        let _ = task.await;
    });
    *owner = Some(HeadlessRuntime {
        binding: binding.clone(),
        handle: handle.clone(),
    });
    drop(owner);
    let reg = atomcode_coding::session_runtime_registry::SessionRuntimeRegistry::global();
    let _ = reg.open_or_attach(binding.session_id.clone(), binding.working_dir.clone());
    let _ = reg.bind_handle(&binding.session_id, handle, None);
    join().map_err(|error| format!("live hub join failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::bind_after_mcp_ready;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn headless_bind_waits_for_mcp_catalog_readiness() {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel::<()>();
        let bound = Arc::new(AtomicBool::new(false));
        let bound_in_task = Arc::clone(&bound);

        let bind_task = tokio::spawn(async move {
            bind_after_mcp_ready(
                async move {
                    waiting_tx.send(()).unwrap();
                    ready_rx.await.expect("readiness sender must stay alive");
                    Ok::<(), &'static str>(())
                },
                || {
                    bound_in_task.store(true, Ordering::Release);
                    Ok(())
                },
            )
            .await
        });

        waiting_rx.await.unwrap();
        assert!(
            !bound.load(Ordering::Acquire),
            "the live hub must remain unbound while MCP tools are unpublished"
        );

        ready_tx.send(()).unwrap();
        bind_task.await.unwrap().unwrap();
        assert!(
            bound.load(Ordering::Acquire),
            "the live hub should bind after MCP tools reach the catalog"
        );
    }
}
