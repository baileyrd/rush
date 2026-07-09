//! Shell state that outlives a single command: the last exit status (`$?`) and
//! shell variables (`FOO=bar`, `export`).
//!
//! The REPL is single-threaded, so a thread-local `RefCell` is all the
//! synchronisation we need — the same approach `job` uses for its job table.
//!
//! Variables live only in this map, not the process environment. Lookups for
//! `$VAR` consult the map first and fall back to the real environment, and only
//! variables marked *exported* are pushed into child processes (see
//! `exec::build_stage`). Non-exported variables stay private to the shell.

use std::cell::RefCell;
use std::collections::HashMap;

struct Var {
    value: String,
    exported: bool,
}

/// A pending `break`/`continue` request, carrying how many enclosing loops it
/// applies to (`break 2`). The executor consumes it level by level.
#[derive(Clone, Copy)]
pub enum LoopCtl {
    Break(u32),
    Continue(u32),
}

thread_local! {
    static LAST_STATUS: RefCell<i32> = const { RefCell::new(0) };
    static VARS: RefCell<HashMap<String, Var>> = RefCell::new(HashMap::new());
    static LOOP_CTL: RefCell<Option<LoopCtl>> = const { RefCell::new(None) };
    static RETURNING: RefCell<Option<i32>> = const { RefCell::new(None) };
    // `$0` (shell/script name) and `$1`, `$2`, … (positional parameters).
    static SHELL_NAME: RefCell<String> = RefCell::new("rush".to_string());
    static ARGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    // `set -e`: a failing command exits the shell (see exec::exec_list_impl).
    static ERREXIT: RefCell<bool> = const { RefCell::new(false) };
    // The exit status of the most recent command substitution performed
    // while expanding a command's words, if any (see `reset_last_subst_status`).
    static LAST_SUBST_STATUS: RefCell<Option<i32>> = const { RefCell::new(None) };
}

pub fn set_errexit(on: bool) {
    ERREXIT.with(|e| *e.borrow_mut() = on);
}

pub fn errexit() -> bool {
    ERREXIT.with(|e| *e.borrow())
}

/// Clear the "did a command substitution just run" marker. Called right
/// before expanding a simple command's words, so that afterward
/// `take_last_subst_status` reflects only a substitution that happened
/// during *this* command's own expansion — not a stale one left over from
/// something unrelated.
pub fn reset_last_subst_status() {
    LAST_SUBST_STATUS.with(|s| *s.borrow_mut() = None);
}

/// Record a command substitution's exit status — its last job's status, same
/// as `$?` would see from inside it. Used to give a variable-assignment-only
/// command (`x=$(false)`) POSIX's exit-status rule: it's the last
/// substitution's status, not always 0.
pub fn set_last_subst_status(code: i32) {
    LAST_SUBST_STATUS.with(|s| *s.borrow_mut() = Some(code));
}

/// Consume the marker set by `set_last_subst_status`, if any command
/// substitution ran since the last `reset_last_subst_status`.
pub fn take_last_subst_status() -> Option<i32> {
    LAST_SUBST_STATUS.with(|s| s.borrow_mut().take())
}

/// Set `$0` and the positional parameters (`$1`…).
pub fn set_args(name: String, args: Vec<String>) {
    SHELL_NAME.with(|n| *n.borrow_mut() = name);
    ARGS.with(|a| *a.borrow_mut() = args);
}

/// `$n`: `$0` is the shell/script name, `$1`… the positional parameters.
pub fn arg(n: usize) -> Option<String> {
    if n == 0 {
        Some(SHELL_NAME.with(|s| s.borrow().clone()))
    } else {
        ARGS.with(|a| a.borrow().get(n - 1).cloned())
    }
}

/// `$#` — the number of positional parameters.
pub fn arg_count() -> usize {
    ARGS.with(|a| a.borrow().len())
}

/// All positional parameters (`$@` / `$*`).
pub fn args() -> Vec<String> {
    ARGS.with(|a| a.borrow().clone())
}

/// `shift n`: drop the first `n` positional parameters. Returns `false` (and
/// leaves them untouched) if `n` is greater than `$#` — the `shift` builtin
/// reports that as a usage error, matching bash.
pub fn shift(n: usize) -> bool {
    ARGS.with(|a| {
        let mut a = a.borrow_mut();
        if n > a.len() {
            return false;
        }
        a.drain(0..n);
        true
    })
}

/// Record a pending loop-control request (from the `break`/`continue` builtins).
pub fn set_loop_ctl(ctl: Option<LoopCtl>) {
    LOOP_CTL.with(|c| *c.borrow_mut() = ctl);
}

/// The pending loop-control request, if any.
pub fn loop_ctl() -> Option<LoopCtl> {
    LOOP_CTL.with(|c| *c.borrow())
}

/// Record a pending `return` (from the `return` builtin) with its exit code.
pub fn set_returning(code: Option<i32>) {
    RETURNING.with(|r| *r.borrow_mut() = code);
}

/// The pending `return` code, if a function should unwind.
pub fn returning() -> Option<i32> {
    RETURNING.with(|r| *r.borrow())
}

/// Whether any non-local control flow (`break`/`continue`/`return`) is pending,
/// so a list should stop running further commands.
pub fn flow_pending() -> bool {
    loop_ctl().is_some() || returning().is_some()
}

/// The exit status of the most recently completed pipeline — exposed as `$?`.
pub fn last_status() -> i32 {
    LAST_STATUS.with(|s| *s.borrow())
}

pub fn set_last_status(code: i32) {
    LAST_STATUS.with(|s| *s.borrow_mut() = code);
}

/// Look up a shell variable's value (not the environment — see `expand`).
pub fn get(name: &str) -> Option<String> {
    VARS.with(|v| v.borrow().get(name).map(|x| x.value.clone()))
}

/// Remove a shell variable (`unset NAME`).
pub fn unset(name: &str) {
    VARS.with(|v| {
        v.borrow_mut().remove(name);
    });
}

/// Set a variable, preserving its exported flag if it already existed.
pub fn set(name: &str, value: &str) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        let exported = m.get(name).is_some_and(|x| x.exported);
        m.insert(name.to_string(), Var { value: value.to_string(), exported });
    });
}

/// Set a variable and mark it exported (`export NAME=value`).
pub fn set_exported(name: &str, value: &str) {
    VARS.with(|v| {
        v.borrow_mut().insert(
            name.to_string(),
            Var { value: value.to_string(), exported: true },
        );
    });
}

/// Mark an existing (or newly-created, empty) variable exported (`export NAME`).
pub fn export(name: &str) {
    VARS.with(|v| {
        v.borrow_mut()
            .entry(name.to_string())
            .or_insert_with(|| Var { value: String::new(), exported: false })
            .exported = true;
    });
}

/// A snapshot of all variables, for isolating a subshell on platforms without
/// `fork` (see `exec::run_compound`'s `Compound::Subshell` arm) — Unix forks a
/// real child instead, so these are unused there.
#[cfg(not(unix))]
pub type Snapshot = Vec<(String, String, bool)>;

#[cfg(not(unix))]
pub fn snapshot() -> Snapshot {
    VARS.with(|v| {
        v.borrow()
            .iter()
            .map(|(k, x)| (k.clone(), x.value.clone(), x.exported))
            .collect()
    })
}

#[cfg(not(unix))]
pub fn restore(snap: Snapshot) {
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        m.clear();
        for (name, value, exported) in snap {
            m.insert(name, Var { value, exported });
        }
    });
}

/// Every exported variable as `(name, value)`, for seeding child environments.
pub fn exported() -> Vec<(String, String)> {
    VARS.with(|v| {
        v.borrow()
            .iter()
            .filter(|(_, x)| x.exported)
            .map(|(k, x)| (k.clone(), x.value.clone()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_unset_and_export() {
        set("RUSH_V", "1");
        assert_eq!(get("RUSH_V").as_deref(), Some("1"));
        assert!(!exported().iter().any(|(k, _)| k == "RUSH_V"));

        export("RUSH_V");
        assert!(exported().iter().any(|(k, v)| k == "RUSH_V" && v == "1"));

        // Re-setting keeps the exported flag.
        set("RUSH_V", "2");
        assert!(exported().iter().any(|(k, v)| k == "RUSH_V" && v == "2"));

        unset("RUSH_V");
        assert_eq!(get("RUSH_V"), None);
    }

    #[test]
    fn shift_drops_leading_positional_params() {
        set_args("prog".to_string(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        assert!(shift(1));
        assert_eq!(args(), vec!["b", "c"]);

        assert!(shift(0)); // no-op, always succeeds
        assert_eq!(args(), vec!["b", "c"]);

        // Greater than the remaining count: rejected, nothing shifted.
        assert!(!shift(3));
        assert_eq!(args(), vec!["b", "c"]);

        assert!(shift(2));
        assert!(args().is_empty());
        assert!(!shift(1)); // now empty: even 1 is too many

        set_args("prog".to_string(), Vec::new());
    }
}
