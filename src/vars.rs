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
    // One frame per active function call, pushed/popped by
    // `push_local_frame`/`pop_local_frame`. Each frame lists the names
    // `local` has shadowed *in that call*, alongside what they were
    // beforehand (`None` meaning "didn't exist") — see `declare_local`.
    static LOCAL_STACK: RefCell<Vec<LocalFrame>> = const { RefCell::new(Vec::new()) };
}

/// A prior value (`value`, `exported`) to restore when a `local`-shadowed
/// name's function call returns, or `None` if the name didn't exist before.
type PriorValue = Option<(String, bool)>;
/// One function call's set of `local`-shadowed names, in declaration order.
type LocalFrame = Vec<(String, PriorValue)>;

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

/// Push a fresh, empty local-variable frame — called when entering a
/// function call (`exec::call_function`).
pub fn push_local_frame() {
    LOCAL_STACK.with(|s| s.borrow_mut().push(Vec::new()));
}

/// Pop the current function call's local-variable frame, restoring each name
/// `local` shadowed in it to whatever it was beforehand — or removing it, if
/// it didn't exist before the call. Nesting falls out naturally: an inner
/// call's frame captures whatever the *enclosing* call's own locals had
/// already shadowed things to, so popping the inner frame restores the
/// outer call's local value, not the top-level one (verified against real
/// bash directly).
pub fn pop_local_frame() {
    let Some(frame) = LOCAL_STACK.with(|s| s.borrow_mut().pop()) else {
        return;
    };
    VARS.with(|v| {
        let mut m = v.borrow_mut();
        for (name, prior) in frame {
            match prior {
                Some((value, exported)) => {
                    m.insert(name, Var { value, exported });
                }
                None => {
                    m.remove(&name);
                }
            }
        }
    });
}

/// `local [name[=value]]...`: shadow `name` with a fresh binding for the
/// current function call, restored automatically (see `pop_local_frame`)
/// when it returns. `value: None` (a bare `local name`) leaves `name`
/// genuinely unset within the function, matching bash — not merely set to
/// `""` (`${name-default}` inside the function sees it as unset). Returns
/// `false` if there's no active function call to declare into (the `local`
/// builtin reports that as a usage error); a name already made local earlier
/// in *this same* call keeps its originally-captured prior value, so a
/// second `local x` in one call still restores to the pre-call value, not
/// the first `local`'s.
pub fn declare_local(name: &str, value: Option<&str>) -> bool {
    let declared = LOCAL_STACK.with(|s| {
        let mut stack = s.borrow_mut();
        let Some(frame) = stack.last_mut() else {
            return false;
        };
        if !frame.iter().any(|(n, _)| n == name) {
            let prior = VARS.with(|v| v.borrow().get(name).map(|x| (x.value.clone(), x.exported)));
            frame.push((name.to_string(), prior));
        }
        true
    });
    if declared {
        match value {
            Some(v) => set(name, v),
            None => unset(name),
        }
    }
    declared
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

    #[test]
    fn local_outside_a_function_is_rejected() {
        assert!(!declare_local("RUSH_LOCAL_TOP", Some("1")));
        // Rejected: must not fall through to setting it as a plain global.
        assert_eq!(get("RUSH_LOCAL_TOP"), None);
    }

    #[test]
    fn local_shadows_and_restores_on_frame_pop() {
        set("RUSH_LOCAL_X", "outer");

        push_local_frame();
        assert!(declare_local("RUSH_LOCAL_X", Some("inner")));
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("inner"));

        // A bare `local name` (no `=value`) leaves it genuinely unset, not
        // merely set to `""`.
        assert!(declare_local("RUSH_LOCAL_Y", None));
        assert_eq!(get("RUSH_LOCAL_Y"), None);

        // A second `local` for the same name in the *same* frame doesn't
        // re-capture — it must still restore to the pre-frame value.
        assert!(declare_local("RUSH_LOCAL_X", Some("inner2")));
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("inner2"));

        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_X").as_deref(), Some("outer"));

        unset("RUSH_LOCAL_X");
    }

    #[test]
    fn nested_frames_restore_to_the_enclosing_frames_own_value() {
        set("RUSH_LOCAL_N", "top");

        push_local_frame();
        declare_local("RUSH_LOCAL_N", Some("outer_call"));

        push_local_frame();
        declare_local("RUSH_LOCAL_N", Some("inner_call"));
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("inner_call"));
        pop_local_frame();

        // Popping the inner call's frame restores the *outer* call's own
        // local value, not the top-level one — matches real bash.
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("outer_call"));
        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_N").as_deref(), Some("top"));

        unset("RUSH_LOCAL_N");
    }

    #[test]
    fn local_of_a_name_that_never_existed_is_removed_on_pop() {
        unset("RUSH_LOCAL_NEW");
        push_local_frame();
        declare_local("RUSH_LOCAL_NEW", Some("value"));
        assert_eq!(get("RUSH_LOCAL_NEW").as_deref(), Some("value"));
        pop_local_frame();
        assert_eq!(get("RUSH_LOCAL_NEW"), None);
    }
}
