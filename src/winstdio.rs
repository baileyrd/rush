//! Minimal Win32 std-handle facade for the `cfg(not(unix))` execution path —
//! the Windows counterpart of the Unix-only `sys` module, scoped to exactly
//! what the redirect machinery needs. Declared by hand rather than via
//! `windows-sys`, for the same minimal-dependency reason `sys` sits on
//! rusty_libc: three stable, documented kernel32 calls don't justify a crate.
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
    fn GetConsoleMode(hconsolehandle: RawHandle, lpmode: *mut u32) -> i32;
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

/// Is `handle` a console (the Windows notion of "a tty")? `GetConsoleMode`
/// succeeds only on console handles — the canonical detection idiom.
pub fn is_console(handle: RawHandle) -> bool {
    let mut mode = 0u32;
    unsafe { GetConsoleMode(handle, &mut mode) != 0 }
}
