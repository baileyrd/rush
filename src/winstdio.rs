//! Minimal Win32 std-handle facade for the `cfg(not(unix))` execution path —
//! the Windows counterpart of the Unix-only `sys` module, scoped to exactly
//! what the redirect machinery needs. Declared by hand rather than via
//! `windows-sys`, for the same minimal-dependency reason `sys` sits on
//! rusty_libc: two stable, documented kernel32 calls don't justify a crate.
//! (Windows is the only supported non-Unix target; see docs/ARCHITECTURE.md
//! "Windows strategy".)
//!
//! Windows has no `dup2`/fd table. What it has instead is three process-global
//! std-handle slots (`GetStdHandle`/`SetStdHandle`), which both Rust's own
//! stdio (`println!`, `std::io::stdin` — they re-resolve the slot on every
//! read/write) and `std::process::Command`'s inherited stdio (resolved at
//! spawn) follow. So "redirect fd 1 for the duration" translates to "swap the
//! `STD_OUTPUT_HANDLE` slot and swap it back" — which is exactly what
//! `exec::redirect_stdio`'s non-Unix arm does with these wrappers.

/// A raw Win32 `HANDLE` (same representation `std::os::windows::io` uses).
pub type RawHandle = *mut core::ffi::c_void;

pub const STD_INPUT_HANDLE: u32 = -10i32 as u32;
pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(nstdhandle: u32) -> RawHandle;
    fn SetStdHandle(nstdhandle: u32, hhandle: RawHandle) -> i32;
}

/// The std-handle slot for a shell fd, or `None` for fd 3+ — Windows has no
/// general fd table to put those in (the platform limit callers report).
pub fn slot_for_fd(fd: i32) -> Option<u32> {
    match fd {
        0 => Some(STD_INPUT_HANDLE),
        1 => Some(STD_OUTPUT_HANDLE),
        2 => Some(STD_ERROR_HANDLE),
        _ => None,
    }
}

/// The handle currently occupying `slot`.
pub fn get(slot: u32) -> RawHandle {
    unsafe { GetStdHandle(slot) }
}

/// Point `slot` at `handle`. The slot is a plain pointer store: nothing is
/// duplicated or closed, so whoever owns `handle` must keep it alive for as
/// long as the slot references it.
pub fn set(slot: u32, handle: RawHandle) -> bool {
    unsafe { SetStdHandle(slot, handle) != 0 }
}

/// The stdin handle the process started with, captured by `main` before any
/// redirect can swap the slot. `usize::MAX` = never captured (unit tests),
/// treated as "not redirected".
static STARTUP_STDIN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

pub fn capture_startup_stdin() {
    STARTUP_STDIN.store(get(STD_INPUT_HANDLE) as usize, std::sync::atomic::Ordering::Relaxed);
}

/// Has a redirect (`read x < f`, `exec 0< f`, a here-doc) swapped the stdin
/// slot away from the process's original handle? Distinguishes "fd 0 is a
/// redirect target" (read it directly, unbuffered) from "fd 0 is the shell's
/// own stdin" (read through `std::io::stdin()`, whose buffer the line editor
/// shares — see `builtins::read_fd_byte`).
pub fn stdin_is_redirected() -> bool {
    let captured = STARTUP_STDIN.load(std::sync::atomic::Ordering::Relaxed);
    captured != usize::MAX && get(STD_INPUT_HANDLE) as usize != captured
}
