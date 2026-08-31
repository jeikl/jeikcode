//! Async-signal-safe terminal restore on fatal signals (Unix), plus
//! job-control immunity so a stolen TTY cannot `[Stopped]` the TUI.
//!
//! `TerminalGuard::Drop` (lib.rs) restores the terminal on a graceful exit,
//! and the panic hook covers `panic = "abort"`. But a process **killed by a
//! signal** (`kill`/SIGTERM, terminal close/SIGHUP, or `kill -INT`) runs
//! NEITHER — the kernel tears the process down without unwinding. The shell
//! then inherits a terminal still in raw mode with the Kitty keyboard protocol
//! and bracketed paste armed, plus the leftover `❯` input row, so subsequent
//! keystrokes echo as CSI-u / `200~` gibberish.
//!
//! Separately: if a child (or grandchild) briefly becomes the terminal's
//! foreground process group, the next stdin read delivers SIGTTIN and a
//! `tcsetattr`/stdout write delivers SIGTTOU. Default disposition is Stop —
//! bash prints `[N]+ Stopped atomcode`, the session lease stays held, and
//! resume fails with SessionInUse until that process is killed. Raw mode
//! (ISIG off) already swallows Ctrl-Z as a byte, but SIGTTIN/SIGTTOU are
//! kernel job-control and fire regardless of ISIG. Every TUI that reads
//! stdin must ignore them (vim, less, grok-pager) and `tcsetpgrp` itself
//! back onto the TTY. See [`ignore_job_control_signals`] / [`recover_tty`].
//!
//! This is the reported "Ctrl-C twice to exit writes junk into the input box":
//! the v2 quit chain wedged past the force-exit watchdog, so the TUI was
//! ultimately signal-killed (`zsh: terminated …`) instead of exiting cleanly,
//! and nothing restored the terminal.
//!
//! We install a raw `sigaction` handler — NOT a `tokio` signal task, which the
//! very wedge that triggers the kill would starve. Using only async-signal-safe
//! calls (`write`, `tcsetattr`, `raise`), it emits the restore byte sequence,
//! takes the terminal out of raw mode via the cooked `termios` captured at arm
//! time, then re-raises the signal under the default disposition so the parent
//! still observes the true signal exit status.

use core::ptr::{addr_of, addr_of_mut};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

static INSTALLED: AtomicBool = AtomicBool::new(false);
static TERMIOS_SAVED: AtomicBool = AtomicBool::new(false);
static JOB_CONTROL_IGNORED: AtomicBool = AtomicBool::new(false);
/// Cooked terminal attributes captured before raw mode is enabled, restored
/// verbatim from signal context (`tcsetattr` is async-signal-safe).
static mut ORIG_TERMIOS: core::mem::MaybeUninit<libc::termios> = core::mem::MaybeUninit::uninit();

/// The bytes the signal handler emits to restore the terminal: the canonical
/// `panic_restore_sequence` (Kitty-keyboard pop, mouse off, cursor show,
/// autowrap, scroll-region release, bracketed-paste off, CRLF). Reused so the
/// restore contract lives in ONE place rather than re-appended per exit path.
///
/// Returns a static slice (no allocation) so it is callable from signal context,
/// and pure, hence unit-testable without delivering a real signal.
pub(crate) fn restore_writes() -> &'static [u8] {
    crate::panic_restore_sequence()
}

extern "C" fn handler(signo: c_int) {
    // async-signal-safe calls ONLY below.
    let seq = restore_writes();
    unsafe {
        let _ = libc::write(libc::STDOUT_FILENO, seq.as_ptr().cast(), seq.len());
    }
    // Acquire pairs with the SeqCst (release) store in `arm()` so the termios
    // bytes tcgetattr wrote are visible here even when the signal is delivered
    // on a different thread (weakly-ordered targets, e.g. aarch64).
    if TERMIOS_SAVED.load(Ordering::Acquire) {
        unsafe {
            libc::tcsetattr(
                libc::STDIN_FILENO,
                libc::TCSANOW,
                addr_of!(ORIG_TERMIOS).cast::<libc::termios>(),
            );
        }
    }
    // Re-raise under the default disposition so the exit status still reflects
    // the signal (the shell's "terminated"/"interrupt" message is correct — we
    // only cleaned the terminal first).
    unsafe {
        libc::signal(signo, libc::SIG_DFL);
        libc::raise(signo);
    }
}

/// Ignore job-control stop signals so a stolen TTY cannot `[Stopped]` the TUI.
///
/// If a child (or grandchild) briefly becomes the terminal's foreground
/// process group via `tcsetpgrp`, the next stdin read from our reader thread
/// delivers SIGTTIN and a stdout write / `tcsetattr` delivers SIGTTOU. Default
/// disposition is Stop — bash then prints `[N]+ Stopped atomcode`, the session
/// lease stays held by the stopped process, and `atomcode` resume fails with
/// SessionInUse until that process is killed.
///
/// Every TUI that reads stdin must ignore these (vim, less, grok-pager).
/// Idempotent. Safe to call from the reader thread on I/O errors.
pub(crate) fn ignore_job_control_signals() {
    if JOB_CONTROL_IGNORED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        // `signal()` (not `sigaction`) matches grok-pager / vim: a TUI that
        // reads stdin must not be Stoppable by job-control. SIGTSTP is also
        // ignored here so a Ctrl-Z that arrives *before* the event-loop tokio
        // handler is installed cannot `[Stopped]` us. The event loop later
        // replaces SIGTSTP with its own catch-and-refresh handler; this
        // function is idempotent so recover_tty() will not overwrite that.
        libc::signal(libc::SIGTTIN, libc::SIG_IGN);
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::signal(libc::SIGTSTP, libc::SIG_IGN);
    }
}

/// Reclaim the terminal's foreground process group.
///
/// After `ignore_job_control_signals`, a background read no longer Stops us
/// (it returns EIO instead). We still have to `tcsetpgrp` ourselves back onto
/// the TTY, or keystrokes go to the thief / vanish and the TUI looks frozen.
/// No-op when stdin is not a TTY. SIGTTOU is already ignored, so this is safe
/// even while we are still in the background.
pub(crate) fn claim_foreground_tty() {
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }
        let pgid = libc::getpgrp();
        if pgid > 0 {
            let _ = libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
        }
    }
}

/// Recover from a stolen TTY / EIO without dropping to `[Stopped]`.
///
/// Called from the stdin reader on `poll`/`read` errors and from the SIGTSTP /
/// SIGCONT arms. Re-asserts job-control ignores, takes the foreground pgrp
/// back, and puts the terminal in raw mode so the next keystroke is a TUI
/// event rather than a kernel-generated stop.
pub(crate) fn recover_tty() {
    ignore_job_control_signals();
    claim_foreground_tty();
    let _ = crossterm::terminal::enable_raw_mode();
}

/// Compact Linux TTY health snapshot used by the opt-in TUI diagnostic log.
///
/// A lost foreground pgrp normally makes `read(2)` fail with EIO, but a lost
/// raw mode is more treacherous: `poll(2)` keeps returning `Ok(false)` while
/// canonical mode waits for a whole line. The old reader only recovered on an
/// error, so arrows/Enter appeared dead forever without producing anything we
/// could diagnose. Keep the test here beside the code that owns recovery.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TtyHealth {
    pub is_tty: bool,
    pub process_group: i32,
    pub foreground_group: i32,
    pub canonical: bool,
    pub echo: bool,
    pub signals: bool,
}

impl TtyHealth {
    #[inline]
    pub(crate) fn needs_recovery(self) -> bool {
        self.is_tty
            && (self.foreground_group != self.process_group || self.canonical || self.echo)
    }
}

pub(crate) fn tty_health() -> TtyHealth {
    unsafe {
        let is_tty = libc::isatty(libc::STDIN_FILENO) != 0;
        let process_group = libc::getpgrp();
        let foreground_group = if is_tty {
            libc::tcgetpgrp(libc::STDIN_FILENO)
        } else {
            -1
        };
        let mut termios: libc::termios = core::mem::zeroed();
        let have_termios =
            is_tty && libc::tcgetattr(libc::STDIN_FILENO, &mut termios as *mut _) == 0;
        TtyHealth {
            is_tty,
            process_group,
            foreground_group,
            canonical: have_termios && (termios.c_lflag & libc::ICANON) != 0,
            echo: have_termios && (termios.c_lflag & libc::ECHO) != 0,
            signals: have_termios && (termios.c_lflag & libc::ISIG) != 0,
        }
    }
}

pub(crate) fn trace_tty(stage: &str) -> TtyHealth {
    let health = tty_health();
    crate::tuix_trace!(
        "TTY",
        "stage={} tty={} pgrp={} fg_pgrp={} canonical={} echo={} isig={} unhealthy={}",
        stage,
        health.is_tty,
        health.process_group,
        health.foreground_group,
        health.canonical,
        health.echo,
        health.signals,
        health.needs_recovery()
    );
    health
}

/// Recover only when the terminal is observably unhealthy. Safe to call from
/// the sole stdin-reader thread on a quiet poll timeout or an explicit
/// Bash/approval boundary; healthy calls do not rewrite termios.
pub(crate) fn recover_tty_if_needed(stage: &str) -> bool {
    let before = trace_tty(stage);
    if !before.needs_recovery() {
        return false;
    }
    crate::tuix_trace!("TTY", "stage={} action=recover_begin", stage);
    recover_tty();
    let after = trace_tty("recover_done");
    crate::tuix_trace!(
        "TTY",
        "stage={} action=recover_end healthy={}",
        stage,
        !after.needs_recovery()
    );
    true
}

/// Capture the cooked `termios` (before raw mode flips it) and install the
/// terminal-restore handler for SIGTERM / SIGINT / SIGHUP. Idempotent — only the
/// first call takes effect. Call this immediately before `enable_raw_mode()`.
pub(crate) fn arm() {
    // Job-control ignores must be in place BEFORE the first stdin read /
    // tcsetattr. A child that stole the TTY between process start and here
    // would otherwise Stop us the moment we flip raw mode.
    ignore_job_control_signals();
    claim_foreground_tty();
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        if libc::tcgetattr(
            libc::STDIN_FILENO,
            addr_of_mut!(ORIG_TERMIOS).cast::<libc::termios>(),
        ) == 0
        {
            TERMIOS_SAVED.store(true, Ordering::SeqCst);
        }
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = handler as *const () as libc::sighandler_t;
        // Block the signals we manage during the handler so a second one can't
        // re-enter it mid-restore and kill us under the wrong signal's status.
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaddset(&mut sa.sa_mask, libc::SIGTERM);
        libc::sigaddset(&mut sa.sa_mask, libc::SIGINT);
        libc::sigaddset(&mut sa.sa_mask, libc::SIGHUP);
        sa.sa_flags = 0;
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaction(sig, &sa, core::ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{restore_writes, TtyHealth};

    #[test]
    fn tty_health_recovers_foreground_or_raw_mode_drift() {
        let healthy = TtyHealth {
            is_tty: true,
            process_group: 42,
            foreground_group: 42,
            canonical: false,
            echo: false,
            signals: false,
        };
        assert!(!healthy.needs_recovery());
        assert!(TtyHealth {
            canonical: true,
            ..healthy
        }
        .needs_recovery());
        assert!(TtyHealth {
            foreground_group: 99,
            ..healthy
        }
        .needs_recovery());
        assert!(TtyHealth {
            echo: true,
            ..healthy
        }
        .needs_recovery());
        assert!(!TtyHealth {
            is_tty: false,
            canonical: true,
            echo: true,
            foreground_group: -1,
            ..healthy
        }
        .needs_recovery());
    }

    /// The whole point over the panic sequence: a signal-kill must ALSO disable
    /// bracketed paste, or the shell wraps every paste in `200~`/`201~`. And it
    /// must still pop the Kitty protocol + show the cursor (else CSI-u echo /
    /// invisible cursor). Pins the exact restore contract the handler emits.
    #[test]
    fn signal_restore_disables_bracketed_paste_and_pops_kitty() {
        let text = String::from_utf8_lossy(restore_writes());
        assert!(
            text.contains("\x1b[?2004l"),
            "must disable bracketed paste (the panic sequence omits it): {text:?}"
        );
        assert!(
            text.contains("\x1b[<1u"),
            "must pop Kitty keyboard: {text:?}"
        );
        assert!(text.contains("\x1b[?25h"), "must show the cursor: {text:?}");
    }

    /// Pins the Linux job-control contract: SIGTTIN/SIGTTOU must be SIG_IGN
    /// so a stolen TTY cannot Stop the process. Querying sigaction after
    /// install is the only way to lock this without delivering a real signal.
    #[test]
    fn ignore_job_control_signals_sets_sig_ign() {
        super::ignore_job_control_signals();
        unsafe {
            let mut old: libc::sigaction = core::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGTTIN, core::ptr::null(), &mut old),
                0,
                "sigaction query SIGTTIN"
            );
            assert_eq!(
                old.sa_sigaction,
                libc::SIG_IGN,
                "SIGTTIN must be ignored so a background stdin read cannot Stop us"
            );
            assert_eq!(
                libc::sigaction(libc::SIGTTOU, core::ptr::null(), &mut old),
                0,
                "sigaction query SIGTTOU"
            );
            assert_eq!(
                old.sa_sigaction,
                libc::SIG_IGN,
                "SIGTTOU must be ignored so a background tcsetattr/write cannot Stop us"
            );
            assert_eq!(
                libc::sigaction(libc::SIGTSTP, core::ptr::null(), &mut old),
                0,
                "sigaction query SIGTSTP"
            );
            assert_eq!(
                old.sa_sigaction, libc::SIG_IGN,
                "SIGTSTP must be ignored until the event loop installs its catch-and-refresh handler"
            );
        }
        // Non-TTY stdin (cargo test without a real terminal) is a no-op; with
        // a TTY this just re-asserts our own pgrp. Must not panic.
        // Do NOT call recover_tty() here — it enable_raw_mode()s the test
        // runner's terminal.
        super::claim_foreground_tty();
    }
}
