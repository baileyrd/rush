//! Shell state that outlives a single command: the last exit status (`$?`),
//! and (later) shell variables.
//!
//! The REPL is single-threaded, so a thread-local `RefCell` is all the
//! synchronisation we need — the same approach `job` uses for its job table.

use std::cell::RefCell;

thread_local! {
    static LAST_STATUS: RefCell<i32> = const { RefCell::new(0) };
}

/// The exit status of the most recently completed pipeline — exposed as `$?`.
pub fn last_status() -> i32 {
    LAST_STATUS.with(|s| *s.borrow())
}

pub fn set_last_status(code: i32) {
    LAST_STATUS.with(|s| *s.borrow_mut() = code);
}
