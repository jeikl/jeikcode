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

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
static ORIGIN: OnceLock<Instant> = OnceLock::new();
static STAGE_DELAY: OnceLock<std::time::Duration> = OnceLock::new();

pub fn enabled() -> bool {
    sink().is_some()
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
    // load). Set ATOMCODE_TUIX_LOG=/path to enable diagnosis.
    SINK.get_or_init(|| {
        let path = std::env::var("ATOMCODE_TUIX_LOG").ok()?;
        if path.is_empty() {
            return None;
        }
        // Truncate on each run so stale events from prior sessions
        // don't confuse diagnosis.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .ok()?;
        Some(Mutex::new(file))
    })
    .as_ref()
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
