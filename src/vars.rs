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

thread_local! {
    static LAST_STATUS: RefCell<i32> = const { RefCell::new(0) };
    static VARS: RefCell<HashMap<String, Var>> = RefCell::new(HashMap::new());
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
