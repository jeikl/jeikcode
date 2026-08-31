//! Live bash jobs: a wait timeout parks the process instead of killing it.
//!
//! `timeout` on `bash` / `bash_timeout_add` is a per-call wait budget. The process
//! lifetime hard cap is `[tools.bash] max_timeout_secs` from spawn. Completing
//! inside a wait window returns immediately; leftover output is never discarded.

use super::err;
use async_trait::async_trait;
use atomcode_kernel::tool::{RiskLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Default extra wait when `bash_timeout_add` omits `timeout`. Also the minimum
/// increment advertised to the model.
pub(crate) const TIMEOUT_ADD_DEFAULT_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveStatus {
    Running,
    Exited { success: bool, code: Option<i32> },
    Cancelled,
    KilledMaxTimeout,
}

impl LiveStatus {
    pub(crate) fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

pub(crate) struct Capture {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub reported_stdout: usize,
    pub reported_stderr: usize,
}

pub(crate) struct LiveBash {
    pub id: String,
    pub first_timeout_secs: u64, // original wait; kept for diagnostics / future footer
    pub started: Instant,
    pub max_deadline: Instant,
    pub capture: Mutex<Capture>,
    status_tx: watch::Sender<LiveStatus>,
    status_rx: watch::Receiver<LiveStatus>,
    pub kill: CancellationToken,
}

impl LiveBash {
    pub(crate) fn new(first_timeout_secs: u64, max_lifetime_secs: u64) -> Arc<Self> {
        let (status_tx, status_rx) = watch::channel(LiveStatus::Running);
        let now = Instant::now();
        Arc::new(Self {
            id: alloc_bash_id(),
            first_timeout_secs,
            started: now,
            max_deadline: now + Duration::from_secs(max_lifetime_secs.max(1)),
            capture: Mutex::new(Capture {
                stdout: Vec::new(),
                stderr: Vec::new(),
                reported_stdout: 0,
                reported_stderr: 0,
            }),
            status_tx,
            status_rx,
            kill: CancellationToken::new(),
        })
    }

    pub(crate) fn current_status(&self) -> LiveStatus {
        *self.status_rx.borrow()
    }

    pub(crate) fn publish(&self, status: LiveStatus) {
        let _ = self.status_tx.send(status);
    }

    pub(crate) async fn wait_until_not_running(&self) {
        let mut rx = self.status_rx.clone();
        loop {
            if !rx.borrow().is_running() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

fn live_jobs() -> &'static Mutex<HashMap<String, Arc<LiveBash>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<LiveBash>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_jobs() -> std::sync::MutexGuard<'static, HashMap<String, Arc<LiveBash>>> {
    live_jobs().lock().unwrap_or_else(|e| e.into_inner())
}

fn alloc_bash_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("bash-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn register_live_bash(job: Arc<LiveBash>) {
    lock_jobs().insert(job.id.clone(), job);
}

pub(crate) fn unregister_live_bash(id: &str) {
    lock_jobs().remove(id);
}

pub(crate) fn lookup_live_bash(id: &str) -> Option<Arc<LiveBash>> {
    lock_jobs().get(id).cloned()
}

pub(crate) fn remaining_secs(deadline: Instant) -> u64 {
    deadline.saturating_duration_since(Instant::now()).as_secs()
}

pub(crate) fn suggested_add_secs(job: &LiveBash) -> u64 {
    let remaining = remaining_secs(job.max_deadline);
    if remaining == 0 {
        1
    } else {
        TIMEOUT_ADD_DEFAULT_SECS.min(remaining)
    }
}

/// Footer (and a matching head line) so fold/truncation cannot hide `bash_id`.
pub(crate) fn still_running_footer(job: &LiveBash, waited_secs: u64) -> String {
    let elapsed = job.started.elapsed().as_secs().max(1);
    let suggest = suggested_add_secs(job);
    let remaining = remaining_secs(job.max_deadline);
    format!(
        "[bash still running] bash_id={id} elapsed={elapsed}s\n\n\
当前任务执行时间较长，但后台并没有停止。请增加等待时间，每次至少加 600 秒。\n\
在加时窗口内若命令结束会立即返回结果。不要重新开一条 bash 跑同一条命令。\n\
调用：bash_timeout_add({{\"bash_id\":\"{id}\",\"timeout\":{suggest}}})\n\
waited_secs={waited} first_wait_secs={first} remaining_hard_cap_secs={remaining}",
        id = job.id,
        waited = waited_secs,
        first = job.first_timeout_secs,
    )
}

/// RAII: dropping `execute` without parking kills the job (kernel cancel-drop).
pub(crate) struct KeepAliveOnTimeout {
    pub job: Arc<LiveBash>,
    pub keep_alive: bool,
}

impl Drop for KeepAliveOnTimeout {
    fn drop(&mut self) {
        if !self.keep_alive {
            self.job.kill.cancel();
        }
    }
}

#[derive(Default)]
pub struct BashTimeoutAddTool;

#[derive(Deserialize)]
struct AddArgs {
    bash_id: String,
    #[serde(default)]
    timeout: Option<u64>,
}

#[async_trait]
impl Tool for BashTimeoutAddTool {
    fn name(&self) -> &str {
        "bash_timeout_add"
    }
    fn description(&self) -> &str {
        "Keep waiting on a bash command that is still running. The original wait expired; \
         the process was NOT killed and its output was NOT discarded. Pass `bash_id` from \
         that result. `timeout` is extra seconds to block (omit → 600). ALWAYS add at least \
         600 seconds. This call blocks until the command exits or the extra wait ends; \
         if it exits first the result returns immediately. Do NOT start a new bash for the \
         same command. The hard process lifetime is config `[tools.bash] max_timeout_secs`."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "bash_id": {
                    "type": "string",
                    "description": "Id from a still-running bash result (unique per command in the session), e.g. bash-12"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Extra seconds to wait this call. ALWAYS add at least 600. Omit to use 600. Completes immediately if the command exits first. Clamped to remaining max_timeout_secs."
                }
            },
            "required": ["bash_id"]
        })
    }
    fn risk(&self, _args: &str) -> RiskLevel {
        RiskLevel::Safe
    }
    fn parallel_safe(&self, _args: &str) -> bool {
        true
    }
    async fn execute(&self, args: &str, ctx: &ToolContext) -> ToolResult {
        let a: AddArgs = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => {
                return err(format!(
                    "bash_timeout_add: invalid arguments: {e}. Expected {{\"bash_id\":\"bash-N\",\"timeout\":600}}."
                ))
            }
        };
        let bash_id = a.bash_id.trim();
        if bash_id.is_empty() {
            return err(
                "bash_timeout_add: `bash_id` is required (from a still-running bash result)."
                    .to_string(),
            );
        }
        let Some(job) = lookup_live_bash(bash_id) else {
            return err(format!(
                "bash_timeout_add: unknown bash_id `{bash_id}`. It may have already finished, \
                 been cancelled, or never existed."
            ));
        };
        let remaining = remaining_secs(job.max_deadline);
        if remaining == 0 {
            let _ =
                tokio::time::timeout(Duration::from_secs(2), job.wait_until_not_running()).await;
            return super::finish_live_job(&job);
        }
        let extra = a
            .timeout
            .unwrap_or(TIMEOUT_ADD_DEFAULT_SECS)
            .clamp(1, remaining);
        super::wait_on_live_bash(&job, extra, ctx, true)
            .await
            .into_result()
    }
}

#[cfg(test)]
pub(crate) fn kill_live_bash_for_test(id: &str) {
    if let Some(job) = lookup_live_bash(id) {
        job.kill.cancel();
    }
}
