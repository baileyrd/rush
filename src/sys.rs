//! Syscall facade: the `libc` crate (default) or `rusty_libc` (the `rusty-libc`
//! feature). This is the **only** module that names `libc`; the rest of rush
//! uses `sys::` for every syscall, type, and constant, so a
//! `--no-default-features --features rusty-libc` build links no `libc` at all.
//!
//! Each backend exports the same surface — the syscall *functions* rush issues
//! (mirroring the `libc` signatures: same args, same `unsafe`-ness, same
//! `-1`/errno convention), the C types (`c_int`, `pid_t`, `rlimit`, …), and the
//! constants (`SIG*`, `RLIMIT_*`, `W*`, `F_*`, `STDIN_FILENO`). The libc
//! backend re-exports them from `libc`; the rusty-libc backend from
//! `rusty_libc` (defining the handful `rusty_libc` doesn't ship).
//!
//! ## fork
//!
//! The default backend uses glibc's `fork` (which resets glibc's internal
//! malloc/stdio locks in the child). The `rusty-libc` backend uses a **raw**
//! `clone(SIGCHLD)` fork, which does not — so it is only sound because rush is
//! single-threaded at every fork point: on Linux the here-document feeders are
//! backed by [`memfd_heredoc`] (an in-memory file) rather than background
//! threads. See `docs/LIBC_DEPENDENCY_ANALYSIS.md` §4.2. `std::process::Command`
//! still uses std's own spawn path in both backends.
//!
//! ## errno
//!
//! `rusty_libc` returns errors as `Result<_, Errno>` and does not write
//! glibc's TLS `errno`, so `std::io::Error::last_os_error()` is meaningless
//! after its calls. Read errno through [`last_os_error`] instead: it reads
//! glibc's `errno` in the default backend and, in the `rusty-libc` backend,
//! the code stashed by the last failed wrapper.

#![allow(non_snake_case)] // W* helpers keep their libc names.
#![allow(non_camel_case_types)] // C type aliases keep their libc names.
#![allow(clippy::missing_safety_doc)] // These mirror libc; safety is libc's.

pub use imp::*;

#[cfg(all(unix, not(feature = "libc-backend"), not(feature = "rusty-libc")))]
compile_error!("enable one syscall backend: the default `libc-backend`, or `rusty-libc`");

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
#[cfg(all(feature = "libc-backend", not(feature = "rusty-libc")))]
mod imp {
    // C types and every constant rush uses come straight from libc.
    pub use libc::{c_int, mode_t, pid_t, rlim_t, rlimit, sighandler_t, uid_t};
    // Which constants a given build actually references depends on cfg (e.g.
    // `F_GETFD` only on the non-Linux here-doc path), so allow unused here.
    #[allow(unused_imports)]
    pub use libc::{
        FD_CLOEXEC, F_GETFD, F_SETFD, RLIMIT_AS, RLIMIT_CORE, RLIMIT_CPU, RLIMIT_DATA, RLIMIT_FSIZE,
        RLIMIT_LOCKS, RLIMIT_MEMLOCK, RLIMIT_MSGQUEUE, RLIMIT_NICE, RLIMIT_NOFILE, RLIMIT_NPROC,
        RLIMIT_RSS, RLIMIT_RTPRIO, RLIMIT_SIGPENDING, RLIMIT_STACK, RLIM_INFINITY, SIGABRT, SIGALRM,
        SIGBUS, SIGCHLD, SIGCONT, SIGFPE, SIGHUP, SIGILL, SIGINT, SIGKILL, SIGPIPE, SIGQUIT,
        SIGSEGV, SIGSTOP, SIGTERM, SIGTRAP, SIGTSTP, SIGTTIN, SIGTTOU, SIGUSR1, SIGUSR2, SIG_DFL,
        SIG_IGN, STDIN_FILENO, WCONTINUED, WNOHANG, WUNTRACED,
    };

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
    use rusty_libc::{fd, process, rlimit as rl, signal as sig, termios, umask as um, wait, Errno};
    use std::cell::Cell;

    // C types rush uses. Linux is the only target for this backend, so these
    // are the Linux/glibc widths.
    pub type c_int = i32;
    pub type pid_t = i32;
    pub type mode_t = u32;
    pub type rlim_t = u64;
    pub type uid_t = u32;
    /// A signal disposition (`SIG_DFL`/`SIG_IGN`/`SIG_ERR` or a handler pointer
    /// cast to an integer), matching libc's `sighandler_t`.
    pub type sighandler_t = usize;

    /// Kernel/glibc `struct rlimit`, both fields 64-bit. Field names match
    /// libc's so call sites are backend-agnostic.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct rlimit {
        pub rlim_cur: rlim_t,
        pub rlim_max: rlim_t,
    }

    // Constants: values from rusty_libc where it ships them, else defined here.
    // `F_GETFD` is only referenced on the non-Linux here-doc path, so allow it
    // to be unused on Linux.
    #[allow(unused_imports)]
    pub use rusty_libc::fd::{FD_CLOEXEC, F_GETFD, F_SETFD};
    pub use rusty_libc::rlimit::{
        RLIMIT_AS, RLIMIT_CORE, RLIMIT_CPU, RLIMIT_DATA, RLIMIT_FSIZE, RLIMIT_LOCKS, RLIMIT_MEMLOCK,
        RLIMIT_MSGQUEUE, RLIMIT_NICE, RLIMIT_NOFILE, RLIMIT_NPROC, RLIMIT_RSS, RLIMIT_RTPRIO,
        RLIMIT_SIGPENDING, RLIMIT_STACK, RLIM_INFINITY,
    };
    pub use rusty_libc::signal::{
        SIGABRT, SIGALRM, SIGBUS, SIGCHLD, SIGCONT, SIGFPE, SIGHUP, SIGILL, SIGINT, SIGKILL,
        SIGPIPE, SIGQUIT, SIGSEGV, SIGSTOP, SIGTERM, SIGTRAP, SIGTSTP, SIGTTIN, SIGTTOU, SIGUSR1,
        SIGUSR2, SIG_DFL, SIG_IGN,
    };
    pub use rusty_libc::wait::{WCONTINUED, WNOHANG, WUNTRACED};

    /// `signal` returning this indicates an error (matches libc's `SIG_ERR`).
    pub const SIG_ERR: sighandler_t = usize::MAX;
    /// Standard input file descriptor.
    pub const STDIN_FILENO: c_int = 0;

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
                SIG_ERR
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
