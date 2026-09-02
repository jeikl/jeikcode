//! Live bash registry, session-scoped long-job keywords, idle-decision
//! sentinels, and process-tree busy sampling.
//!
//! Session overlay (`global=false`) is written to the bound session sidecar
//! (`<id>.bashkw.json`) so `/resume` after a JeikCode restart still sees it.
//! `config.toml` is only touched when the model passes `global: true`.

use atomcode_kernel::tool::ProgressSink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Marker in bash output / tool result: the process is still running and the
/// model must promote or kill it. WebUI/TUI keep the original pane inflight.
pub const AWAIT_DECISION_MARK: &str = "[bash-await-decision]";
/// Written to the original pane when `bash_kill_by_id` wins.
pub const KILLED_BY_TOOL_MARK: &str = "[task was canceled by bash kill tool]";
/// Written to the original pane when a keyword promote lands on a live task.
pub const PROMOTED_MARK: &str = "[bash promoted to long job]";

pub struct LiveBash {
    pub bashid: String,
    pub command: String,
    pub promoted: AtomicBool,
    /// First-level idle already elapsed with output but 0 CPU; now on
    /// `second_levell_secs` grace in case a silent compile is about to start.
    pub second_level: AtomicBool,
    pub kill: CancellationToken,
    pub progress: ProgressSink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusyKind {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleAction {
    /// Keep running; treat this invocation as a batch job.
    AutoPromote,
    /// Pager / REPL / wait-for-key. Kill, keep captured output.
    KillStuck,
    /// Foreground server. Kill and tell the model to detach.
    KillResident,
    /// Could not sample CPU; ask the model.
    AwaitDecision,
}

/// Idle expiry: bytes already decided there was no new output.
/// CPU decides whether that silence is work. Disk/network IO is NOT busy:
/// those go through first+second idle and then the model decides.
pub fn classify_idle(has_output: bool, busy: BusyKind, resident: bool) -> IdleAction {
    match busy {
        BusyKind::Yes => IdleAction::AutoPromote,
        BusyKind::No if resident && has_output => IdleAction::KillResident,
        BusyKind::No => IdleAction::KillStuck,
        BusyKind::Unknown if has_output => IdleAction::AwaitDecision,
        BusyKind::Unknown => IdleAction::KillStuck,
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: Mutex<Vec<Arc<LiveBash>>> = Mutex::new(Vec::new());
/// Session overlay only — not seeded from global config.toml.
static SESSION_KEYWORDS: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// Bound `<id>.bashkw.json` for the live CodingRuntime session.
static BOUND_BASHKW: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Interpreters that must not be auto-promoted as long-job keywords
/// (`python script.py` stays a short probe). Explicit `action=add` still works.
pub fn is_generic_long_keyword(keyword: &str) -> bool {
    matches!(
        keyword.trim().to_ascii_lowercase().as_str(),
        "python"
            | "python3"
            | "python2"
            | "py"
            | "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "java"
            | "javaw"
            | "bash"
            | "sh"
            | "zsh"
            | "dash"
            | "fish"
            | "cmd"
            | "cmd.exe"
            | "powershell"
            | "pwsh"
            | "ruby"
            | "perl"
            | "php"
            | "lua"
    )
}

pub fn new_bashid() -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("b-{n:08x}")
}

pub fn register_live_bash(entry: Arc<LiveBash>) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(entry);
}

pub fn unregister_live_bash(bashid: &str) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|e| e.bashid != bashid);
}

pub fn find_live_bash(bashid: &str) -> Option<Arc<LiveBash>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|e| e.bashid == bashid)
        .cloned()
}

pub fn session_long_keywords() -> Vec<String> {
    SESSION_KEYWORDS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Bind this process to a session sidecar and load its keywords.
/// Called from CodingRuntime `prepare` on Fresh/Resume/Draft.
pub fn bind_session_long_keywords(path: PathBuf, keywords: Vec<String>) {
    set_live_long_keywords(keywords);
    *BOUND_BASHKW.write().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn flush_session_keywords() {
    let path = BOUND_BASHKW
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(path) = path else {
        return;
    };
    let payload = serde_json::json!({
        "v": 1,
        "keywords": session_long_keywords(),
    });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, payload.to_string());
}

/// Disk config ∪ session overlay. Config wins as persistence; session covers
/// this process without writing `config.toml`.
pub fn effective_long_keywords() -> Vec<String> {
    let mut v = crate::tools::bash::resolve_bash_timeout_config().long_bash_command_keyword;
    for k in session_long_keywords() {
        if !v.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
            v.push(k);
        }
    }
    v
}

pub fn live_long_keywords() -> Vec<String> {
    effective_long_keywords()
}

pub fn set_live_long_keywords(keywords: Vec<String>) {
    *SESSION_KEYWORDS.write().unwrap_or_else(|e| e.into_inner()) = keywords;
}

pub fn add_live_long_keyword(keyword: &str) -> bool {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return false;
    }
    let mut g = SESSION_KEYWORDS.write().unwrap_or_else(|e| e.into_inner());
    if g.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
        return false;
    }
    g.push(keyword.to_string());
    drop(g);
    flush_session_keywords();
    true
}

pub fn remove_live_long_keyword(keyword: &str) -> bool {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return false;
    }
    let mut g = SESSION_KEYWORDS.write().unwrap_or_else(|e| e.into_inner());
    let before = g.len();
    g.retain(|k| !k.eq_ignore_ascii_case(keyword));
    let changed = g.len() != before;
    drop(g);
    if changed {
        flush_session_keywords();
    }
    changed
}

/// Whole-word match (alphanumeric / `_` / `-` / `.`). `a` does not match `cat`.
pub fn command_matches_keyword(command: &str, keyword: &str) -> bool {
    let k = keyword.trim();
    if k.is_empty() {
        return false;
    }
    command
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        .any(|w| !w.is_empty() && w.eq_ignore_ascii_case(k))
}

pub fn command_matches_any_keyword(command: &str, keywords: &[String]) -> bool {
    keywords.iter().any(|k| command_matches_keyword(command, k))
}

/// Promote every live bash whose command contains `keyword`. Returns how many
/// were newly promoted (already-promoted entries are skipped).
pub fn promote_matching(keyword: &str) -> usize {
    let snapshot: Vec<Arc<LiveBash>> = REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut n = 0;
    for e in snapshot {
        if command_matches_keyword(&e.command, keyword) && !e.promoted.swap(true, Ordering::SeqCst)
        {
            e.progress
                .emit(format!("{PROMOTED_MARK} keyword={keyword}\n"));
            n += 1;
        }
    }
    n
}

pub fn kill_by_id(bashid: &str) -> bool {
    if let Some(e) = find_live_bash(bashid) {
        e.kill.cancel();
        true
    } else {
        false
    }
}

/// Parse Linux `/proc/<pid>/stat`. Returns `(pgrp, state, utime+stime ticks)`.
pub fn parse_linux_proc_stat(stat: &str) -> Option<(u32, char, u64)> {
    let after = stat.rsplit_once(')')?.1;
    let mut parts = after.split_whitespace();
    let state = parts.next()?.chars().next()?;
    let _ppid = parts.next()?;
    let pgrp: u32 = parts.next()?.parse().ok()?;
    for _ in 0..8 {
        parts.next()?;
    }
    let utime: u64 = parts.next()?.parse().ok()?;
    let stime: u64 = parts.next()?.parse().ok()?;
    Some((pgrp, state, utime.saturating_add(stime)))
}

#[cfg(test)]
pub fn parse_linux_proc_io(io: &str) -> u64 {
    let mut rchar = 0u64;
    let mut wchar = 0u64;
    for line in io.lines() {
        if let Some(v) = line.strip_prefix("rchar:") {
            rchar = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("wchar:") {
            wchar = v.trim().parse().unwrap_or(0);
        }
    }
    rchar.saturating_add(wchar)
}

/// TCP hex state that means data or a handshake is in flight — not LISTEN
/// (servers sitting idle) and not TIME_WAIT/CLOSE.
#[cfg(test)]
pub fn tcp_hex_state_is_inflight(st: &str) -> bool {
    matches!(st, "01" | "02" | "03" | "04" | "05" | "08" | "09" | "0B")
}

/// `/proc/net/tcp` (and tcp6) line → inode if the connection is in-flight.
#[cfg(test)]
pub fn parse_proc_net_tcp_inflight_inode(line: &str) -> Option<u64> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    // sl local rem st tx:rx tr tm->when retrnsmt uid timeout inode
    if cols.len() < 10 || cols[0] == "sl" {
        return None;
    }
    if !tcp_hex_state_is_inflight(cols[3]) {
        return None;
    }
    cols[9].parse().ok()
}

#[cfg(target_os = "linux")]
struct LinuxSnap {
    cpu: u64,
    runnable: bool,
}

#[cfg(target_os = "linux")]
fn linux_pgroup_snapshot(pgid: u32) -> Option<LinuxSnap> {
    let mut cpu = 0u64;
    let mut runnable = false;
    let mut any = false;
    let dir = std::fs::read_dir("/proc").ok()?;
    for ent in dir.flatten() {
        let name = ent.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.as_bytes().iter().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let _pid: u32 = match s.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let path = ent.path();
        let Ok(stat) = std::fs::read_to_string(path.join("stat")) else {
            continue;
        };
        let Some((pgrp, state, ticks)) = parse_linux_proc_stat(&stat) else {
            continue;
        };
        if pgrp != pgid {
            continue;
        }
        any = true;
        cpu = cpu.saturating_add(ticks);
        // Runnable on CPU only. Disk-sleep (D) and ESTABLISHED-TCP-without-CPU
        // are ordinary short-command idle and go through first+second rounds.
        if state == 'R' {
            runnable = true;
        }
    }
    any.then_some(LinuxSnap { cpu, runnable })
}

/// Two 250ms samples of the process tree. `Unknown` when the platform cannot
/// observe CPU (caller should await-decision if there was output).
pub async fn tree_is_busy(
    pgid: Option<u32>,
    #[cfg(windows)] job: &Option<crate::process_utils::JobHandle>,
) -> BusyKind {
    #[cfg(windows)]
    {
        let _ = pgid;
        let Some(j) = job.as_ref() else {
            return BusyKind::Unknown;
        };
        let Some((c1, _)) = j.cpu_and_io() else {
            return BusyKind::Unknown;
        };
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let Some((c2, _)) = j.cpu_and_io() else {
            return BusyKind::Unknown;
        };
        // CPU only. Disk/network byte counters must not auto-promote: those
        // commands take the first+second idle path and the model decides.
        return if c2 > c1 { BusyKind::Yes } else { BusyKind::No };
    }
    #[cfg(target_os = "linux")]
    {
        let Some(pgid) = pgid else {
            return BusyKind::Unknown;
        };
        let Some(a) = linux_pgroup_snapshot(pgid) else {
            return BusyKind::Unknown;
        };
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let Some(b) = linux_pgroup_snapshot(pgid) else {
            return BusyKind::Unknown;
        };
        return if a.runnable || b.runnable || b.cpu > a.cpu {
            BusyKind::Yes
        } else {
            BusyKind::No
        };
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pgid;
        BusyKind::Unknown
    }
}

pub fn decision_prompt(
    bashid: &str,
    idle_secs: u64,
    second_secs: u64,
    suggested_keyword: &str,
) -> String {
    format!(
        "{AWAIT_DECISION_MARK}\n\
         bashid: {bashid}\n\
         This command already printed output, then went silent through first idle \
         ({idle_secs}s) and second-level grace ({second_secs}s). \
         It is STILL RUNNING in this pane.\n\
         If you were running a network-IO or disk-IO command, this likely means the \
         task has timed out — prefer `bash_kill_by_id` with {{\"bashid\":\"{bashid}\"}}.\n\
         Only if after careful consideration you still believe it is making progress, \
         upgrade it to a temporary long bash with `long_bash_keyword_actions` \
         {{\"action\":\"add\",\"keyword\":\"{suggested_keyword}\"}} \
         (global defaults to false: this session only, survives JeikCode restart on resume).\n\
         Do not start a replacement bash. Output of add/kill stays on this pane."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_idle_matrix() {
        use BusyKind::*;
        use IdleAction::*;
        assert_eq!(classify_idle(false, Yes, false), AutoPromote);
        assert_eq!(classify_idle(true, Yes, false), AutoPromote);
        assert_eq!(classify_idle(false, No, false), KillStuck);
        assert_eq!(classify_idle(true, No, false), KillStuck);
        assert_eq!(classify_idle(true, No, true), KillResident);
        assert_eq!(classify_idle(false, No, true), KillStuck);
        assert_eq!(classify_idle(true, Unknown, false), AwaitDecision);
        assert_eq!(classify_idle(false, Unknown, false), KillStuck);
    }

    #[test]
    fn parse_linux_proc_io_sums_rchar_wchar() {
        let io = "rchar: 100\nwchar: 23\nsyscr: 1\nread_bytes: 0\nwrite_bytes: 0\n";
        assert_eq!(parse_linux_proc_io(io), 123);
    }

    #[test]
    fn inflight_tcp_excludes_listen() {
        assert!(tcp_hex_state_is_inflight("01"));
        assert!(tcp_hex_state_is_inflight("02"));
        assert!(!tcp_hex_state_is_inflight("0A"));
        assert!(!tcp_hex_state_is_inflight("06"));
        let line = "   0: 0100007F:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1";
        assert_eq!(parse_proc_net_tcp_inflight_inode(line), None);
        let est = "   1: 0100007F:0050 0100007F:E24A 01 00000000:00000000 00:00000000 00000000     0        0 99999 1";
        assert_eq!(parse_proc_net_tcp_inflight_inode(est), Some(99999));
    }

    #[test]
    fn parse_linux_stat_extracts_pgrp_state_cpu() {
        // fields 14-15 (utime/stime) sit after state,ppid,pgrp + 8 more.
        let line = "42 (gcc) R 1 42 42 0 -1 0 0 0 0 0 100 50 0 0 0";
        let (pgrp, state, cpu) = parse_linux_proc_stat(line).unwrap();
        assert_eq!(pgrp, 42);
        assert_eq!(state, 'R');
        assert_eq!(cpu, 150);
    }

    #[test]
    fn generic_interpreters_are_not_auto_keyword_material() {
        assert!(is_generic_long_keyword("python"));
        assert!(is_generic_long_keyword("Node"));
        assert!(!is_generic_long_keyword("ninja"));
        assert!(!is_generic_long_keyword("webpack"));
    }

    #[test]
    fn session_sidecar_round_trips_keywords() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.bashkw.json");
        let prev = session_long_keywords();
        bind_session_long_keywords(path.clone(), Vec::new());
        assert!(add_live_long_keyword("ninja"));
        assert!(!add_live_long_keyword("Ninja"));
        let raw = std::fs::read_to_string(&path).expect("sidecar written");
        assert!(raw.contains("ninja"), "{raw}");
        assert!(remove_live_long_keyword("ninja"));
        let raw2 = std::fs::read_to_string(&path).unwrap();
        assert!(!raw2.contains("ninja"), "{raw2}");
        *BOUND_BASHKW.write().unwrap_or_else(|e| e.into_inner()) = None;
        set_live_long_keywords(prev);
    }

    #[test]
    fn keyword_is_whole_word() {
        assert!(command_matches_keyword("systemctl status foo", "status"));
        assert!(command_matches_keyword("./ninja -C build", "ninja"));
        assert!(!command_matches_keyword("cat file", "a"));
        assert!(!command_matches_keyword("catch me", "cat"));
        assert!(command_matches_keyword("CAT file", "cat"));
    }
}
