//! Windows Ctrl-C/Ctrl-Break scoping for a running foreground external
//! command — closes `docs/WINDOWS_BACKEND_ANALYSIS.md` §4.5: until now,
//! nothing requested a new console process group for a foreground child,
//! so a Ctrl-C during one could reach rush and the child at the same time
//! (both attached to the same console) instead of being scoped to just
//! the child, the way a Unix process group scopes a terminal signal (see
//! `job.rs`'s own SIGINT-to-foreground-process-group handling for the
//! Unix side of the same concern).
//!
//! The mechanism: [`install`] registers a console-control handler
//! ([`handle_ctrl_event`]) that claims `CTRL_C_EVENT` (returns `TRUE`) so
//! rush's own process is never killed by it — unlike the idle-prompt case
//! (`main.rs`'s `ReadResult::Interrupted`, handled entirely at the
//! readline level via `rusty_lines`' own raw-mode key reading, independent
//! of this module), a foreground external command leaves rush blocked in
//! `Child::wait()`, not reading from the console at all, so only a real
//! console-control-event handler can observe a Ctrl-C at that point. If a
//! foreground child is currently tracked ([`ForegroundGuard`]), the
//! handler forwards `CTRL_BREAK_EVENT` to that child's own process group
//! instead — `CTRL_C_EVENT` itself can't be scoped to one process group by
//! Windows' own design (`GenerateConsoleCtrlEvent` only accepts a nonzero
//! group id for `CTRL_BREAK_EVENT`), which is exactly why this crate's
//! `exec.rs::build_stage` gives a foreground child `CREATE_NEW_PROCESS_GROUP`
//! (whose id is the child's own pid) rather than leaving it in rush's own
//! group.
//!
//! **Not confirmed at runtime**: this sandbox has no Windows machine, the
//! same constraint `docs/WINDOWS_BACKEND_ANALYSIS.md`'s §4.5 and
//! `docs/ARCHITECTURE.md`'s G11 section already flag for their own claims.

use std::sync::Mutex;

/// The process-group ids (== pids, since each was created with
/// `CREATE_NEW_PROCESS_GROUP`) of every currently-running foreground
/// pipeline stage — usually one entry, more than one only while a
/// multi-stage foreground pipeline is running (each stage got its own
/// independent group; there's no single Windows mechanism spanning
/// several already-independent `CreateProcessW` calls the way Unix
/// `setpgid` can put multiple children in one shared group).
static FOREGROUND_GROUPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// RAII guard: registers `pid` as a current foreground target for the
/// duration of the guard (i.e. for as long as `exec.rs::run` is blocked
/// waiting on it), removing it again on drop — including on an early
/// return, so a stale pid never lingers once its child is no longer
/// running and could, in principle, be reused by an unrelated later
/// process.
pub struct ForegroundGuard {
    pid: u32,
}

impl ForegroundGuard {
    pub fn new(pid: u32) -> Self {
        FOREGROUND_GROUPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(pid);
        ForegroundGuard { pid }
    }
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        let mut groups = FOREGROUND_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = groups.iter().position(|&p| p == self.pid) {
            groups.remove(i);
        }
    }
}

/// Runs on Windows' own dedicated console-control-handler thread (per
/// `rusty_win32::console`'s own documented `SetConsoleCtrlHandler`
/// contract) — kept to the same "store/forward, nothing else" discipline
/// `trap.rs`'s Unix `record_signal` follows for the same reason (a handler
/// context is not a safe place for anything heavier).
extern "system" fn handle_ctrl_event(ctrl_type: u32) -> i32 {
    if ctrl_type == rusty_win32::console::CTRL_C_EVENT {
        let groups = FOREGROUND_GROUPS.lock().unwrap_or_else(|e| e.into_inner());
        for &pid in groups.iter() {
            let _ = rusty_win32::console::generate_ctrl_event(
                rusty_win32::console::CTRL_BREAK_EVENT,
                pid,
            );
        }
        // Claim the event either way (even with no foreground child
        // tracked): rush's own process must survive a Ctrl-C at any point
        // it's running, not just while idle at the prompt.
        1
    } else {
        0
    }
}

/// Install the handler above. Call once, at shell startup, unconditionally
/// (even non-interactively — a script running an external command in
/// batch mode is exactly as exposed to this danger as an interactive
/// session, and installing costs nothing when no foreground child is ever
/// tracked). Failure is not fatal: an unclaimed `CTRL_C_EVENT` falls
/// through to Windows' own default handler, exactly today's pre-existing
/// (if imperfect) behavior — not a new failure mode this introduces.
pub fn install() {
    let _ = rusty_win32::console::install_ctrl_handler(handle_ctrl_event);
}
