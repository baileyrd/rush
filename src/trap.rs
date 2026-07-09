//! `trap 'command' NAME...` registers a command string to run when the named
//! event happens. Only `EXIT` (every process-exit path) and `INT` (Ctrl-C at
//! an idle prompt — not a running foreground job, which is a child process
//! under job control and never reaches the shell itself) are recognized.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

thread_local! {
    static TRAPS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    // Names currently being fired, so a trap body that itself exits (or
    // otherwise re-triggers the same trap) can't recurse forever.
    static FIRING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
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
