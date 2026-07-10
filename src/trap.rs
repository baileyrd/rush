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
    crate::job::SIGNAL_TABLE.iter().find(|&&(_, s)| s == sig).map(|&(n, _)| n)
}

/// Signals whose handler is installed *on trap registration* (C64) — the
/// catchable, non-job-control set beyond the always-installed
/// `TERM`/`HUP`. `INT` stays with the interactive machinery, and the
/// job-control/stop signals are deliberately left alone.
#[cfg(unix)]
const DYNAMIC_TRAP_SIGNALS: &[&str] = &["QUIT", "ABRT", "ALRM", "USR1", "USR2", "PIPE"];

/// Install `record_signal` for a dynamically-trapped signal, or restore
/// the default disposition when its trap is removed. `TERM`/`HUP` keep
/// their always-installed handlers (the 128+sig default-exit behavior
/// depends on them).
#[cfg(unix)]
fn sync_signal_disposition(name: &str, trapped: bool) {
    if !DYNAMIC_TRAP_SIGNALS.contains(&name) {
        return;
    }
    if let Some(&(_, sig)) = crate::job::SIGNAL_TABLE.iter().find(|(n, _)| *n == name) {
        unsafe {
            if trapped {
                libc::signal(sig, record_signal as *const () as libc::sighandler_t);
            } else {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
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

/// Canonical signal-name table for trap-spec normalization (C44): bare
/// name ↔ conventional number. Numbers are the x86-64 Linux values (the
/// same ones bash's own `trap -l` shows there) — signal *numbers* are
/// platform-convention anyway, and the names are what everything
/// downstream keys on. `EXIT` is 0, per POSIX (`trap 'cmd' 0`).
const SIGNALS: &[(&str, i32)] = &[
    ("EXIT", 0),
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
];

/// Normalize a `trap` signal spec to the canonical bare name delivery
/// keys on (C44): numeric (`15` → `TERM`, `0` → `EXIT`), `SIG`-prefixed
/// (`SIGTERM` → `TERM`), and lowercase (`term`) spellings all collapse to
/// one form — each accepted by real bash, verified directly. `None` for
/// anything not in the table (bash: "invalid signal specification").
/// Without this, a trap registered under `"15"`/`"SIGTERM"` was stored
/// verbatim and silently orphaned — the delivery side only ever looks up
/// `"TERM"`, so the handler never ran and the signal took the default
/// disposition.
pub fn normalize_signal_spec(spec: &str) -> Option<&'static str> {
    // `ERR` (C53) is a shell pseudo-signal like `EXIT`, but with no
    // number and no `SIG` spelling — matched by exact name only, same as
    // bash (`trap 'cmd' err` works there too, case-insensitively).
    if spec.eq_ignore_ascii_case("ERR") {
        return Some("ERR");
    }
    if let Ok(n) = spec.parse::<i32>() {
        return SIGNALS.iter().find(|&&(_, num)| num == n).map(|&(name, _)| name);
    }
    let upper = spec.to_ascii_uppercase();
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|&&(name, _)| name == bare).map(|&(name, _)| name)
}

pub fn set(name: &str, command: &str) {
    TRAPS.with(|t| t.borrow_mut().insert(name.to_string(), command.to_string()));
    #[cfg(unix)]
    sync_signal_disposition(name, true);
}

pub fn unset(name: &str) {
    TRAPS.with(|t| {
        t.borrow_mut().remove(name);
    });
    #[cfg(unix)]
    sync_signal_disposition(name, false);
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

/// Fire the `ERR` trap (if registered) for a command that failed with
/// `status` (C53): the handler sees that status as `$?` on entry, and —
/// matching bash, verified directly — `$?` is put back to the original
/// failing status afterward, regardless of what the handler itself ran.
pub fn fire_err(status: i32) {
    if !TRAPS.with(|t| t.borrow().contains_key("ERR")) {
        return;
    }
    fire("ERR");
    crate::vars::set_last_status(status);
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

    // C44: numeric, `SIG`-prefixed, and lowercase specs all collapse to
    // the canonical bare name; unknown specs are rejected.
    #[test]
    fn signal_spec_normalization() {
        assert_eq!(normalize_signal_spec("15"), Some("TERM"));
        assert_eq!(normalize_signal_spec("SIGTERM"), Some("TERM"));
        assert_eq!(normalize_signal_spec("sigterm"), Some("TERM"));
        assert_eq!(normalize_signal_spec("term"), Some("TERM"));
        assert_eq!(normalize_signal_spec("TERM"), Some("TERM"));
        assert_eq!(normalize_signal_spec("0"), Some("EXIT"));
        assert_eq!(normalize_signal_spec("EXIT"), Some("EXIT"));
        assert_eq!(normalize_signal_spec("1"), Some("HUP"));
        assert_eq!(normalize_signal_spec("2"), Some("INT"));
        assert_eq!(normalize_signal_spec("BOGUS"), None);
        assert_eq!(normalize_signal_spec("99"), None);
        assert_eq!(normalize_signal_spec(""), None);
    }
}
