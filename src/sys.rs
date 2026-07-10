//! Syscall facade: the `libc` crate by default, `rusty_libc` under the
//! `rusty-libc` feature.
//!
//! Only the syscall *functions* rush issues live here; every function mirrors
//! the `libc` signature it replaces — same arguments, same `unsafe`-ness, same
//! return convention (`-1`/errno on failure) — so call sites change only the
//! path (`libc::waitpid` → `sys::waitpid`) and keep their existing `unsafe`
//! blocks. Constants (`SIG*`, `RLIMIT_*`, `W*` flags, `F_*`, `STDIN_FILENO`)
//! and types (`c_int`, `pid_t`, `rlimit`, …) are backend-independent and stay
//! referenced as `libc::…`.
//!
//! ## fork
//!
//! The default backend uses glibc's `fork` (which resets glibc's internal
//! malloc/stdio locks in the child). The `rusty-libc` backend uses a **raw**
//! `clone(SIGCHLD)` fork, which does not — so it is only sound because rush is
//! single-threaded at every fork point: on Linux the here-document feeders are
//! backed by [`memfd_heredoc`] (an in-memory file) rather than background
//! threads, so no other thread can be holding a lock across a fork. See
//! `docs/LIBC_DEPENDENCY_ANALYSIS.md` §4.2. `std::process::Command` still uses
//! std's own spawn path in both backends.
//!
//! ## errno
//!
//! `rusty_libc` returns errors as `Result<_, Errno>` and does not write
//! glibc's TLS `errno`, so `std::io::Error::last_os_error()` is meaningless
//! after its calls. Read errno through [`last_os_error`] instead: it reads
//! glibc's `errno` in the default backend and, in the `rusty-libc` backend,
//! the code stashed by the last failed wrapper. Every call site that inspects
//! errno after a `sys::` call uses `sys::last_os_error()`.

#![allow(non_snake_case)] // W* helpers keep their libc names.
#![allow(clippy::missing_safety_doc)] // These mirror libc; safety is libc's.

pub use imp::*;

/// Build a rewound, memory-backed file containing `body` — the thread-free
/// here-document backing (Linux only). The caller either `dup2`s it onto a
/// target fd (in-process compound) or hands it to a child as stdin. Replacing
/// the old background writer thread with this is what lets the `rusty-libc`
/// backend fork safely (no thread can hold a lock across the fork).
#[cfg(target_os = "linux")]
pub fn memfd_heredoc(body: &[u8]) -> std::io::Result<std::fs::File> {
    use std::io::{Seek, Write};
    use std::os::fd::FromRawFd;

    let fd = imp::memfd_create_raw()?;
    // SAFETY: `fd` is a fresh, exclusively-owned memfd descriptor.
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    f.write_all(body)?;
    f.rewind()?; // leave the offset at the start for the reader
    Ok(f)
}

// ---- default backend: the libc crate -------------------------------------
#[cfg(not(feature = "rusty-libc"))]
mod imp {
    use libc::{c_int, mode_t, pid_t, rlimit, sighandler_t, uid_t};

    /// errno as an `io::Error`; glibc's TLS `errno` in this backend.
    pub fn last_os_error() -> std::io::Error {
        std::io::Error::last_os_error()
    }

    /// glibc `fork` (see the module note).
    pub unsafe fn fork() -> pid_t {
        unsafe { libc::fork() }
    }

    /// Create an anonymous in-memory file, returning its descriptor (Linux
    /// only; backs [`super::memfd_heredoc`]).
    #[cfg(target_os = "linux")]
    pub fn memfd_create_raw() -> std::io::Result<std::os::fd::RawFd> {
        // SAFETY: valid nul-terminated name; flags is a plain bitmask.
        let fd = unsafe { libc::memfd_create(c"rush_heredoc".as_ptr(), libc::MFD_CLOEXEC) };
        if fd == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    }

    pub unsafe fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t {
        unsafe { libc::waitpid(pid, status, options) }
    }

    pub fn WIFEXITED(s: c_int) -> bool {
        libc::WIFEXITED(s)
    }
    pub fn WEXITSTATUS(s: c_int) -> c_int {
        libc::WEXITSTATUS(s)
    }
    pub fn WIFSIGNALED(s: c_int) -> bool {
        libc::WIFSIGNALED(s)
    }
    pub fn WTERMSIG(s: c_int) -> c_int {
        libc::WTERMSIG(s)
    }
    pub fn WIFSTOPPED(s: c_int) -> bool {
        libc::WIFSTOPPED(s)
    }
    pub fn WSTOPSIG(s: c_int) -> c_int {
        libc::WSTOPSIG(s)
    }
    pub fn WIFCONTINUED(s: c_int) -> bool {
        libc::WIFCONTINUED(s)
    }

    pub unsafe fn signal(sig: c_int, handler: sighandler_t) -> sighandler_t {
        unsafe { libc::signal(sig, handler) }
    }

    pub unsafe fn setpgid(pid: pid_t, pgid: pid_t) -> c_int {
        unsafe { libc::setpgid(pid, pgid) }
    }
    pub unsafe fn kill(pid: pid_t, sig: c_int) -> c_int {
        unsafe { libc::kill(pid, sig) }
    }
    pub unsafe fn killpg(pgrp: pid_t, sig: c_int) -> c_int {
        unsafe { libc::killpg(pgrp, sig) }
    }
    pub unsafe fn getpid() -> pid_t {
        unsafe { libc::getpid() }
    }
    pub unsafe fn getppid() -> pid_t {
        unsafe { libc::getppid() }
    }
    pub unsafe fn getuid() -> uid_t {
        unsafe { libc::getuid() }
    }
    pub unsafe fn tcsetpgrp(fd: c_int, pgrp: pid_t) -> c_int {
        unsafe { libc::tcsetpgrp(fd, pgrp) }
    }
    pub unsafe fn isatty(fd: c_int) -> c_int {
        unsafe { libc::isatty(fd) }
    }

    pub unsafe fn dup(fd: c_int) -> c_int {
        unsafe { libc::dup(fd) }
    }
    pub unsafe fn dup2(oldfd: c_int, newfd: c_int) -> c_int {
        unsafe { libc::dup2(oldfd, newfd) }
    }
    pub unsafe fn close(fd: c_int) -> c_int {
        unsafe { libc::close(fd) }
    }
    /// Create a pipe, writing `[read, write]` into `fds` (points at 2 ints).
    pub unsafe fn pipe(fds: *mut c_int) -> c_int {
        unsafe { libc::pipe(fds) }
    }
    pub unsafe fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int {
        unsafe { libc::fcntl(fd, cmd, arg) }
    }

    pub unsafe fn getrlimit(resource: c_int, rlim: *mut rlimit) -> c_int {
        unsafe { libc::getrlimit(resource as _, rlim) }
    }
    pub unsafe fn setrlimit(resource: c_int, rlim: *const rlimit) -> c_int {
        unsafe { libc::setrlimit(resource as _, rlim) }
    }
    pub unsafe fn umask(mask: mode_t) -> mode_t {
        unsafe { libc::umask(mask) }
    }
}

// ---- rusty-libc backend --------------------------------------------------
#[cfg(feature = "rusty-libc")]
mod imp {
    use libc::{c_int, mode_t, pid_t, rlimit, sighandler_t, uid_t};
    use rusty_libc::{fd, process, rlimit as rl, signal as sig, termios, umask as um, wait, Errno};
    use std::cell::Cell;

    thread_local! {
        // errno from the last failed rusty_libc wrapper (see module note).
        static LAST_ERRNO: Cell<i32> = const { Cell::new(0) };
    }
    fn stash(e: Errno) {
        LAST_ERRNO.with(|c| c.set(e.code()));
    }
    /// Run `f`; on `Err` stash its errno and return `fail` (mirrors libc's
    /// "-1 and set errno" convention without touching glibc's TLS errno).
    fn call<T>(fail: T, f: impl FnOnce() -> Result<T, Errno>) -> T {
        match f() {
            Ok(v) => v,
            Err(e) => {
                stash(e);
                fail
            }
        }
    }

    pub fn last_os_error() -> std::io::Error {
        std::io::Error::from_raw_os_error(LAST_ERRNO.with(|c| c.get()))
    }

    /// Raw `clone(SIGCHLD)` fork (see the module note): sound because rush is
    /// single-threaded at every fork point in this backend (Linux here-docs
    /// are memfd-backed, not thread-fed).
    pub unsafe fn fork() -> pid_t {
        match unsafe { process::fork() } {
            Ok(pid) => pid,
            Err(e) => {
                stash(e);
                -1
            }
        }
    }

    /// Create an anonymous in-memory file, returning its descriptor (Linux
    /// only; backs [`super::memfd_heredoc`]).
    #[cfg(target_os = "linux")]
    pub fn memfd_create_raw() -> std::io::Result<std::os::fd::RawFd> {
        fd::memfd_create(c"rush_heredoc", fd::MFD_CLOEXEC)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code()))
    }

    pub unsafe fn waitpid(pid: pid_t, status: *mut c_int, options: c_int) -> pid_t {
        match wait::waitpid(pid, options) {
            Ok((wpid, st)) => {
                if !status.is_null() {
                    unsafe { *status = st };
                }
                wpid
            }
            Err(e) => {
                stash(e);
                -1
            }
        }
    }

    pub fn WIFEXITED(s: c_int) -> bool {
        wait::wifexited(s)
    }
    pub fn WEXITSTATUS(s: c_int) -> c_int {
        wait::wexitstatus(s)
    }
    pub fn WIFSIGNALED(s: c_int) -> bool {
        wait::wifsignaled(s)
    }
    pub fn WTERMSIG(s: c_int) -> c_int {
        wait::wtermsig(s)
    }
    pub fn WIFSTOPPED(s: c_int) -> bool {
        wait::wifstopped(s)
    }
    pub fn WSTOPSIG(s: c_int) -> c_int {
        wait::wstopsig(s)
    }
    pub fn WIFCONTINUED(s: c_int) -> bool {
        wait::wifcontinued(s)
    }

    pub unsafe fn signal(signum: c_int, handler: sighandler_t) -> sighandler_t {
        // rusty_libc's signal is glibc-BSD-compatible (persistent, SA_RESTART).
        match unsafe { sig::signal(signum, handler) } {
            Ok(prev) => prev,
            Err(e) => {
                stash(e);
                libc::SIG_ERR
            }
        }
    }

    pub unsafe fn setpgid(pid: pid_t, pgid: pid_t) -> c_int {
        call(-1, || process::setpgid(pid, pgid).map(|_| 0))
    }
    pub unsafe fn kill(pid: pid_t, signum: c_int) -> c_int {
        call(-1, || process::kill(pid, signum).map(|_| 0))
    }
    pub unsafe fn killpg(pgrp: pid_t, signum: c_int) -> c_int {
        call(-1, || process::killpg(pgrp, signum).map(|_| 0))
    }
    pub unsafe fn getpid() -> pid_t {
        process::getpid()
    }
    pub unsafe fn getppid() -> pid_t {
        process::getppid()
    }
    pub unsafe fn getuid() -> uid_t {
        process::getuid()
    }
    pub unsafe fn tcsetpgrp(fd: c_int, pgrp: pid_t) -> c_int {
        call(-1, || termios::tcsetpgrp(fd, pgrp).map(|_| 0))
    }
    pub unsafe fn isatty(fd: c_int) -> c_int {
        c_int::from(termios::isatty(fd))
    }

    pub unsafe fn dup(fildes: c_int) -> c_int {
        call(-1, || fd::dup(fildes))
    }
    pub unsafe fn dup2(oldfd: c_int, newfd: c_int) -> c_int {
        call(-1, || fd::dup2(oldfd, newfd))
    }
    pub unsafe fn close(fildes: c_int) -> c_int {
        call(-1, || fd::close(fildes).map(|_| 0))
    }
    pub unsafe fn pipe(fds: *mut c_int) -> c_int {
        match fd::pipe2(0) {
            Ok((r, w)) => {
                unsafe {
                    *fds = r;
                    *fds.add(1) = w;
                }
                0
            }
            Err(e) => {
                stash(e);
                -1
            }
        }
    }
    pub unsafe fn fcntl(fildes: c_int, cmd: c_int, arg: c_int) -> c_int {
        call(-1, || fd::fcntl(fildes, cmd, arg))
    }

    pub unsafe fn getrlimit(resource: c_int, rlim: *mut rlimit) -> c_int {
        match rl::getrlimit(resource) {
            Ok(r) => {
                unsafe {
                    (*rlim).rlim_cur = r.cur;
                    (*rlim).rlim_max = r.max;
                }
                0
            }
            Err(e) => {
                stash(e);
                -1
            }
        }
    }
    pub unsafe fn setrlimit(resource: c_int, rlim: *const rlimit) -> c_int {
        let r = unsafe {
            rl::Rlimit {
                cur: (*rlim).rlim_cur,
                max: (*rlim).rlim_max,
            }
        };
        call(-1, || rl::setrlimit(resource, &r).map(|_| 0))
    }
    pub unsafe fn umask(mask: mode_t) -> mode_t {
        // On Linux (the only target for this backend) `mode_t` is `u32`.
        um::umask(mask)
    }
}
