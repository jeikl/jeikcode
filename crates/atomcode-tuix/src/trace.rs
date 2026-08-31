// crates/atomcode-tuix/src/trace.rs
//
// Opt-in file logger for diagnosing event-loop / render timing issues.
//
// Enabled via env var `ATOMCODE_TUIX_LOG=/path/to/file`. When unset
// every `tuix_trace!` call compiles into a no-op fast path (single
// atomic load + branch predict), so leaving trace points scattered
// through hot paths costs nothing in release.
//
// Why not env_logger / tracing? We write to stderr-redirected-to-tty
// in raw mode; any sloppy write corrupts the display. A dedicated file
// sink with explicit category tags keeps the noise off the terminal
// and keeps the format parseable (one event per line, microsecond
// monotonic timestamps).
//
// Format: `+{elapsed_us} [{CAT}] {tid} {message}`
//   elapsed_us — microseconds since the first log event in this process
//   CAT        — 2-4 char category (IN, APV, TTY, FOC, BASH, REN, RD, BG)
//   tid        — short thread name or id (so `event_loop` vs `tuix-render`
//                is visible at a glance)

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
static ORIGIN: OnceLock<Instant> = OnceLock::new();
static STAGE_DELAY: OnceLock<Duration> = OnceLock::new();
static TTY_KEY_SEQ: AtomicU64 = AtomicU64::new(0);
static LOOP_KEY_SEQ: AtomicU64 = AtomicU64::new(0);
static LAST_READER_US: AtomicU64 = AtomicU64::new(0);
static LAST_WATCHDOG_LOG_US: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_STARTED: AtomicBool = AtomicBool::new(false);
static LAST_POLL_DT_US: AtomicU64 = AtomicU64::new(0);
static LIVE: OnceLock<Mutex<KbdLive>> = OnceLock::new();
static PULSE_AT: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();

/// Cross-thread snapshot of "where the keyboard is right now".
/// Written from the reader, event loop, and bash/tool handlers; read by
/// every diagnostic pulse so a spinning loop can print 张三=1-style state
/// instead of an empty "still looping".
#[derive(Clone, Debug)]
struct KbdLive {
    last_tty: String,
    last_loop: String,
    reader: &'static str,
    phase: String,
    bash: String,
    buf_len: usize,
    prompt: bool,
    paused: bool,
}

impl Default for KbdLive {
    fn default() -> Self {
        Self {
            last_tty: "-".into(),
            last_loop: "-".into(),
            reader: "init",
            phase: "Idle".into(),
            bash: "none".into(),
            buf_len: 0,
            prompt: false,
            paused: false,
        }
    }
}

fn live() -> &'static Mutex<KbdLive> {
    LIVE.get_or_init(|| Mutex::new(KbdLive::default()))
}

fn patch_live(f: impl FnOnce(&mut KbdLive)) {
    if let Ok(mut g) = live().lock() {
        f(&mut g);
    }
}

pub fn note_tty_key(label: &str) {
    patch_live(|g| g.last_tty = label.to_string());
}

pub fn note_loop_key(label: &str) {
    patch_live(|g| g.last_loop = label.to_string());
}

pub fn set_reader(site: &'static str) {
    patch_live(|g| g.reader = site);
}

pub fn set_reader_paused(paused: bool) {
    patch_live(|g| {
        g.paused = paused;
        g.reader = if paused { "paused" } else { "running" };
    });
}

pub fn set_phase(phase: &str) {
    patch_live(|g| g.phase = phase.to_string());
}

pub fn set_prompt(prompt: bool) {
    patch_live(|g| g.prompt = prompt);
}

pub fn set_buf_len(len: usize) {
    patch_live(|g| g.buf_len = len);
}

pub fn set_bash(stage: &str) {
    patch_live(|g| g.bash = stage.to_string());
}

pub fn set_poll_dt_us(us: u64) {
    LAST_POLL_DT_US.store(us, Ordering::Relaxed);
}

pub fn reader_silent_us() -> u64 {
    let last = LAST_READER_US.load(Ordering::Relaxed);
    if last == 0 {
        return 0;
    }
    (origin().elapsed().as_micros() as u64).saturating_sub(last)
}

/// Compact keyboard/TTY/tool snapshot for loop pulses.
/// Example: `kbd last_tty=Char('6') last_loop=Char('6') tty_seq=11 loop_seq=11 gap=0 into_loop=true reader=poll_enter paused=false silent_us=80 poll_dt_us=100122 echo=false canon=false pgrp_ok=true phase=Streaming prompt=false buf_len=11 bash=args`
pub fn kbd() -> String {
    let snap = live().lock().ok().map(|g| g.clone()).unwrap_or_default();
    let tty = tty_key_seq();
    let loopn = loop_key_seq();
    let gap = tty.saturating_sub(loopn);
    let silent = reader_silent_us();
    let reader_alive = silent < 400_000;
    let into_loop = reader_alive && gap == 0;
    let poll_dt = LAST_POLL_DT_US.load(Ordering::Relaxed);
    #[cfg(unix)]
    let (echo, canon, pgrp_ok, isig) = {
        let h = crate::signal_restore::tty_health();
        (
            h.echo,
            h.canonical,
            h.is_tty && h.foreground_group == h.process_group,
            h.signals,
        )
    };
    #[cfg(not(unix))]
    let (echo, canon, pgrp_ok, isig) = (false, false, true, false);
    format!(
        "kbd last_tty={} last_loop={} tty_seq={} loop_seq={} gap={} into_loop={} reader={} paused={} silent_us={} poll_dt_us={} echo={} canon={} pgrp_ok={} isig={} phase={} prompt={} buf_len={} bash={}",
        snap.last_tty,
        snap.last_loop,
        tty,
        loopn,
        gap,
        into_loop,
        snap.reader,
        snap.paused,
        silent,
        poll_dt,
        echo,
        canon,
        pgrp_ok,
        isig,
        snap.phase,
        snap.prompt,
        snap.buf_len,
        snap.bash
    )
}

/// First call for `site` always logs; afterwards at most once per second.
/// Use inside tight loops that might peg a core — the `kbd=...` trailer
/// shows whether keys are still reaching the event loop.
pub fn should_pulse(site: &'static str) -> bool {
    if !enabled() {
        return false;
    }
    let Ok(mut map) = PULSE_AT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    else {
        return true;
    };
    match map.get(site) {
        None => {
            map.insert(site, Instant::now());
            true
        }
        Some(t) if t.elapsed() >= Duration::from_secs(1) => {
            map.insert(site, Instant::now());
            true
        }
        Some(_) => false,
    }
}

/// Log a named loop/site with the current keyboard snapshot.
pub fn pulse(cat: &str, site: &'static str, extra: std::fmt::Arguments<'_>) {
    if should_pulse(site) {
        write_line(cat, format_args!("site={} {} {}", site, kbd(), extra));
    }
}

/// Unconditional pulse (no 1s gate). For one-shot state changes.
pub fn pulse_now(cat: &str, site: &'static str, extra: std::fmt::Arguments<'_>) {
    write_line(cat, format_args!("site={} {} {}", site, kbd(), extra));
}

/// Default diagnostic sink so `tail -f /tmp/tuix-approval.log` works without
/// guessing a path. `ATOMCODE_TUIX_LOG=1` (or `true`/`yes`/`on`/`default`)
/// selects this; any other non-empty value is treated as an explicit path.
pub const DEFAULT_APPROVAL_LOG: &str = "/tmp/tuix-approval.log";

pub fn enabled() -> bool {
    sink().is_some()
}

/// Monotonic count of Press keys the stdin reader actually decoded.
pub fn next_tty_key_seq() -> u64 {
    TTY_KEY_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// Monotonic count of Press keys the event loop dispatched.
/// Compare with [`next_tty_key_seq`]: a growing gap means keys died
/// between the reader thread and `handle_input`.
pub fn next_loop_key_seq() -> u64 {
    LOOP_KEY_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn tty_key_seq() -> u64 {
    TTY_KEY_SEQ.load(Ordering::Relaxed)
}

pub fn loop_key_seq() -> u64 {
    LOOP_KEY_SEQ.load(Ordering::Relaxed)
}

/// Mark that the stdin reader is still making progress (loop tick / poll
/// return / read). The watchdog logs `stage=WATCHDOG` when this stalls —
/// that is the signature of being stuck inside crossterm `event::poll`.
pub fn note_reader_progress() {
    LAST_READER_US.store(origin().elapsed().as_micros() as u64, Ordering::Relaxed);
}

pub fn resolve_log_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if matches!(
        trimmed,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" | "default"
    ) {
        #[cfg(unix)]
        {
            return Some(PathBuf::from(DEFAULT_APPROVAL_LOG));
        }
        #[cfg(windows)]
        {
            return Some(std::env::temp_dir().join("tuix-approval.log"));
        }
    }
    Some(PathBuf::from(trimmed))
}

fn sink() -> Option<&'static Mutex<File>> {
    // STRICTLY opt-in. Earlier versions defaulted to writing
    // `/tmp/tuix.log` unconditionally so users didn't have to set an
    // env var to produce diagnostic logs. That turned the trace
    // infrastructure into a production bottleneck:
    //   - every `tuix_trace!` call on the main thread AND on the
    //     `tuix-render` worker thread contends the same `Mutex<File>`
    //   - main thread emits RD+IN+KEY per keystroke (~3 traces),
    //     worker emits FOOT+REN+THR per paint (~3 traces)
    //   - under IME burst (8 chars in 100µs), that's 50+ mutex ops
    //     from two threads. Lock queueing added 1-3ms of main-thread
    //     stall per burst, which the user perceives as "吞字" —
    //     characters logically accepted but visually delayed.
    //
    // Now: opt-in only. Default build ships no trace overhead at all
    // (the macro's `if enabled()` short-circuits to a single atomic
    // load). Set ATOMCODE_TUIX_LOG=/path (or `=1` for /tmp/tuix-approval.log)
    // to enable diagnosis.
    SINK.get_or_init(|| {
        let raw = std::env::var("ATOMCODE_TUIX_LOG").ok()?;
        let path = resolve_log_path(&raw)?;
        // Truncate on each run so stale events from prior sessions
        // don't confuse diagnosis.
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok()?;
        let _ = writeln!(
            file,
            "+         0us [LOG]              stage=open path={} tail=\"tail -f {}\"",
            path.display(),
            path.display()
        );
        let _ = file.flush();
        spawn_reader_watchdog();
        Some(Mutex::new(file))
    })
    .as_ref()
}

fn spawn_reader_watchdog() {
    if WATCHDOG_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tuix-watch".to_string())
        .spawn(reader_watchdog_loop);
}

fn reader_watchdog_loop() {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if sink().is_none() {
            return;
        }
        // Independent of the event loop AND the stdin reader: this thread
        // keeps dumping keyboard state even when both are wedged, so a
        // freeze still produces a line every second.
        write_line("KBD", format_args!("stage=tick {}", kbd()));
        let last = LAST_READER_US.load(Ordering::Relaxed);
        if last == 0 {
            continue;
        }
        let now = origin().elapsed().as_micros() as u64;
        let silent_us = now.saturating_sub(last);
        if silent_us < 400_000 {
            continue;
        }
        let last_log = LAST_WATCHDOG_LOG_US.load(Ordering::Relaxed);
        if now.saturating_sub(last_log) < 1_000_000 {
            continue;
        }
        LAST_WATCHDOG_LOG_US.store(now, Ordering::Relaxed);
        write_line(
            "RD",
            format_args!(
                "stage=WATCHDOG reader_silent_us={} (reader stuck in poll/read) {}",
                silent_us,
                kbd()
            ),
        );
    }
}

fn origin() -> Instant {
    *ORIGIN.get_or_init(Instant::now)
}

/// Low-level write. Don't call directly — use the `tuix_trace!` macro.
pub fn write_line(cat: &str, args: std::fmt::Arguments<'_>) {
    let Some(sink) = sink() else {
        return;
    };
    let us = origin().elapsed().as_micros();
    let tid = std::thread::current().name().unwrap_or("?").to_string();
    let line = format!("+{:>10}us [{:>3}] {:>14} {}\n", us, cat, tid, args);
    if let Ok(mut f) = sink.lock() {
        let _ = f.write_all(line.as_bytes());
        // `tail -f` must see each line immediately; block buffering hid the
        // freeze window in earlier runs.
        let _ = f.flush();
    }
}

/// Optional diagnostic pause after a coarse-grained semantic checkpoint.
///
/// `ATOMCODE_TUIX_STAGE_DELAY_MS=3000` is intentionally separate from normal
/// tracing: per-key/per-frame trace points must never sleep. Only explicit
/// `tuix_stage!` checkpoints call this function. The delay is capped so a typo
/// cannot leave the TUI apparently wedged for minutes.
pub fn stage_delay(cat: &str) {
    let delay = *STAGE_DELAY.get_or_init(|| {
        let millis = std::env::var("ATOMCODE_TUIX_STAGE_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .min(30_000);
        std::time::Duration::from_millis(millis)
    });
    if delay.is_zero() {
        return;
    }
    write_line(
        cat,
        format_args!("stage=diagnostic_delay_begin delay_ms={}", delay.as_millis()),
    );
    std::thread::sleep(delay);
    write_line(cat, format_args!("stage=diagnostic_delay_end"));
}

/// `tuix_trace!("CAT", "fmt {}", args)` — compiles to a cheap `enabled()`
/// check when the env var is unset. Use short 2-4 char categories so
/// log lines remain grep-able by column:
///   IN  — input event entering handle_input
///   KEY — key-handler outcome (Redraw / Commit / NoOp)
///   PH  — UiPhase transition (Streaming → Idle, etc.)
///   QUE — type-ahead queue push / pop
///   THR — InputThrottle paint/park decision
///   REN — render worker command processed
///   RD  — raw reader thread event
///   APV — approval lifecycle / key routing / response ownership
///   TTY — foreground process-group and termios health / recovery
///   FOC — terminal focus or external terminal ownership changes
///   BASH — Bash tool start/completion boundaries
///   BG  — background-session request parking and replay
///   LOOP — event-loop 1s heartbeat (alive while the reader may be wedged)
///   MDL  — model first token / turn complete
///   TOOL — before_exec / during_exec / after_exec
///   KBD  — 1s snapshot from the watchdog thread (survives a wedged TUI)
#[macro_export]
macro_rules! tuix_trace {
    ($cat:expr, $($arg:tt)*) => {{
        if $crate::trace::enabled() {
            $crate::trace::write_line($cat, format_args!($($arg)*));
        }
    }};
}

/// Coarse-grained diagnostic checkpoint. It behaves exactly like
/// [`tuix_trace!`] unless `ATOMCODE_TUIX_STAGE_DELAY_MS` is set; in that opt-in
/// mode the calling thread pauses after the line is persisted so a tester can
/// identify the exact Bash/approval/focus transition where input disappears.
#[macro_export]
macro_rules! tuix_stage {
    ($cat:expr, $($arg:tt)*) => {{
        if $crate::trace::enabled() {
            $crate::trace::write_line($cat, format_args!($($arg)*));
            $crate::trace::stage_delay($cat);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::resolve_log_path;

    #[test]
    fn resolve_log_path_empty_disables() {
        assert!(resolve_log_path("").is_none());
        assert!(resolve_log_path("   ").is_none());
    }

    #[test]
    fn resolve_log_path_explicit_file_is_kept() {
        let path = resolve_log_path("/tmp/tuix-approval.log").expect("path");
        assert_eq!(path.to_string_lossy(), "/tmp/tuix-approval.log");
    }

    #[test]
    fn kbd_snapshot_lists_the_status_fields() {
        super::note_tty_key("Char('6')");
        super::note_loop_key("Char('6')");
        super::set_reader("poll_enter");
        super::set_bash("args");
        super::set_phase("Streaming");
        let dump = super::kbd();
        assert!(dump.contains("last_tty=Char('6')"), "{dump}");
        assert!(dump.contains("last_loop=Char('6')"), "{dump}");
        assert!(dump.contains("reader=poll_enter"), "{dump}");
        assert!(dump.contains("bash=args"), "{dump}");
        assert!(dump.contains("phase=Streaming"), "{dump}");
        assert!(dump.contains("into_loop="), "{dump}");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_log_path_flag_uses_default_approval_log() {
        let path = resolve_log_path("1").expect("flag");
        assert_eq!(path.to_string_lossy(), super::DEFAULT_APPROVAL_LOG);
        assert_eq!(
            resolve_log_path("true").unwrap().to_string_lossy(),
            super::DEFAULT_APPROVAL_LOG
        );
    }
}
