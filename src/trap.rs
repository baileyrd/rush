//! `trap 'command' NAME...` registers a command string to run when the named
//! event happens: `EXIT` (every process-exit path), `INT` (Ctrl-C at an idle
//! prompt — not a running foreground job, which is a child process under job
//! control and never reaches the shell itself), and, on Unix, `TERM`/`HUP` —
//! real signals the shell can receive at any time (the standard
//! container/daemon graceful-shutdown pattern).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

thread_local! {
    static TRAPS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    // Names currently being fired, so a trap body that itself exits (or
    // otherwise re-triggers the same trap) can't recurse forever.
    static FIRING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// The most recently received `TERM`/`HUP` signal number, or 0 for "none" —
/// set only by `record_signal` (real signal-handler context: no heap, no
/// locks, nothing beyond a single atomic store) and consumed by
/// `check_pending`, called back in ordinary code at safe points.
#[cfg(unix)]
static PENDING_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record_signal(sig: libc::c_int) {
    PENDING_SIGNAL.store(sig, std::sync::atomic::Ordering::SeqCst);
}

/// Install real handlers for `TERM`/`HUP` — the two POSIX-mandated signals
/// beyond `INT` a script can usefully trap (`ERR`/`DEBUG` are a separate,
/// still-untracked bash extension). Safe to call once at startup in every
/// mode (interactive or not) — unlike job control's own signal setup, this
/// doesn't depend on a real terminal, since the target use case (a
/// container's PID 1 catching `TERM` to shut down gracefully) has none.
#[cfg(unix)]
pub fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, record_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, record_signal as *const () as libc::sighandler_t);
    }
}

#[cfg(unix)]
fn signal_name(sig: libc::c_int) -> Option<&'static str> {
    match sig {
        libc::SIGTERM => Some("TERM"),
        libc::SIGHUP => Some("HUP"),
        _ => None,
    }
}

/// Check for (and clear) a signal `record_signal` recorded: if a trap is
/// registered for it, fire it; if not, terminate the shell with the
/// conventional `128 + signal` status — matching a real signal's default
/// disposition, except (like real bash, verified directly) any `EXIT` trap
/// still runs first, via `exit_shell`. A no-op if nothing's pending. Called
/// between top-level commands, before each interactive prompt, and
/// whenever a blocking `waitpid` is interrupted (`ErrorKind::Interrupted`) —
/// the same call sites a real shell's own signal-handling loop checks.
#[cfg(unix)]
pub fn check_pending() {
    let sig = PENDING_SIGNAL.swap(0, std::sync::atomic::Ordering::SeqCst);
    if sig == 0 {
        return;
    }
    match signal_name(sig) {
        Some(name) if TRAPS.with(|t| t.borrow().contains_key(name)) => fire(name),
        _ => exit_shell(128 + sig),
    }
}

pub fn set(name: &str, command: &str) {
    TRAPS.with(|t| t.borrow_mut().insert(name.to_string(), command.to_string()));
}

pub fn unset(name: &str) {
    TRAPS.with(|t| {
        t.borrow_mut().remove(name);
    });
}

pub fn all() -> Vec<(String, String)> {
    TRAPS.with(|t| t.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Run the trap registered for `name`, if any, discarding its own exit status.
/// A no-op if `name` has no trap, or if `name` is already being fired (guards
/// against a trap body re-triggering itself, e.g. an `EXIT` trap that calls
/// `exit`).
pub fn fire(name: &str) {
    let already_firing = FIRING.with(|f| !f.borrow_mut().insert(name.to_string()));
    if already_firing {
        return;
    }
    if let Some(command) = TRAPS.with(|t| t.borrow().get(name).cloned())
        && let Ok(list) = crate::parser::parse(&command)
    {
        let _ = crate::exec::run_list(&list);
    }
    FIRING.with(|f| {
        f.borrow_mut().remove(name);
    });
}

/// Fire the `EXIT` trap (if any), then terminate the process with `code`.
/// Use this instead of `std::process::exit` on any expected exit path, so a
/// registered `EXIT` trap reliably fires exactly once.
pub fn exit_shell(code: i32) -> ! {
    fire("EXIT");
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_unset_and_list() {
        set("EXIT", "echo bye");
        assert_eq!(all(), vec![("EXIT".to_string(), "echo bye".to_string())]);
        unset("EXIT");
        assert!(all().is_empty());
    }
}
