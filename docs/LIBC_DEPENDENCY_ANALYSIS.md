# libc dependency analysis: what rush uses, and what rolling our own Rust replacement takes

Assessment of rush's dependency on the `libc` crate and on the platform C
library itself, and of what a home-grown Rust implementation (`rusty_libc`)
would require. Companion design/requirements doc lives in the `rusty_libc`
repo (`DESIGN.md`).

## 1. Two different dependencies, easy to conflate

**The `libc` crate** (`libc = "0.2"`, Unix-only in `Cargo.toml`) contains no
implementation. It is FFI *declarations*: constants, struct layouts, and
`extern "C"` prototypes that resolve at link time against the platform C
library (glibc on Linux, libSystem on macOS). Removing it means providing our
own way to reach those ~30 kernel facilities.

**The platform C library itself** is linked by Rust's standard library on
every `*-linux-gnu` target regardless of whether the `libc` crate is present.
rush leans on std heavily — `std::process::Command` (the main spawn path,
`job.rs:151`, `exec.rs:1744`), `pre_exec`/`exec` (`CommandExt`), all of
`std::fs`/`std::io`, the default allocator (glibc `malloc`), and helper
threads (`exec.rs:1323`, `exec.rs:2148`).

So there are two distinct goals with very different costs:

- **Goal A — replace the `libc` crate** with our own raw-syscall crate
  (`rusty_libc`). Feasible; detailed below. The binary still links glibc for
  std.
- **Goal B — produce a binary with no C library at all.** Requires replacing
  everything std gets from glibc: program startup (`_start`, argv/env/auxv,
  TLS setup), the allocator, `posix_spawn`, `dlopen`-adjacent machinery, and
  a std reimplementation or an Eyra/origin/relibc-style shim. That is a
  multi-month systems project unrelated to rush's actual goals; if a
  glibc-free binary is ever wanted, `--target x86_64-unknown-linux-musl`
  (static) achieves it today with zero new code. **Not recommended as a
  hand-rolled project.** The rest of this document addresses Goal A.

## 2. Inventory of the current surface

150 `libc::` call sites across 6 files, ~100 distinct symbols, all
`cfg(unix)`. Grouped by facility:

| Facility | Symbols | Sites | Kernel mapping (x86_64 / aarch64) |
|---|---|---|---|
| Process creation | `fork` | `exec.rs:562` (subshells), `:895` (coproc), `:1503`, `:1568` (command substitution), `:1626` (fd inheritance); `job.rs` pipeline stages | `SYS_fork` / `SYS_clone(SIGCHLD)` (no `fork` on aarch64) |
| Wait & status | `waitpid`, `WNOHANG`, `WUNTRACED`, `WCONTINUED`, `WIFEXITED`, `WEXITSTATUS`, `WIFSIGNALED`, `WTERMSIG`, `WIFSTOPPED`, `WSTOPSIG`, `WIFCONTINUED` | `job.rs` (10 sites) | `SYS_wait4`; W-macros are trivial bit tests |
| Signals | `signal`, `SIG_DFL`, `SIG_IGN`, `sighandler_t`, ~20 `SIG*` constants | `trap.rs:39,68`, `main.rs:142` (SIGPIPE), `exec.rs:892`, `job.rs` (disposition resets for INT/QUIT/TSTP/TTIN/TTOU/CHLD/CONT) | `SYS_rt_sigaction` — **the hard one, see §4.1** |
| Job control | `setpgid`, `tcsetpgrp`, `kill`, `killpg`, `getpid`, `getppid`, `getuid` | `job.rs`, `main.rs` | direct syscalls; `tcsetpgrp` = `ioctl(TIOCSPGRP)`, `killpg(pg)` = `kill(-pg)` |
| Terminal modes | `termios`, `tcgetattr`, `tcsetattr`, `TCSADRAIN`, `ICANON`, `ECHO`, `ISIG`, `IEXTEN`, `IXON`, `ICRNL`, `INLCR`, `VMIN`, `VTIME`, `isatty` | `editor.rs:159–191` (raw-mode guard), `:109` | `ioctl(TCGETS/TCSETSW)` with the **kernel** `termios` struct — layout differs from glibc's, see §4.3 |
| Terminal size | `ioctl`, `TIOCGWINSZ`, `winsize` | `editor.rs:313` | direct |
| Input | `read`, `poll`, `pollfd`, `POLLIN` | `editor.rs:200–212` | `SYS_read`; `SYS_poll` / `SYS_ppoll` (no `poll` on aarch64) |
| File descriptors | `pipe`, `dup`, `dup2`, `close`, `fcntl` (`F_GETFD`/`F_SETFD`/`FD_CLOEXEC`) | `exec.rs`, `job.rs` (11 `dup2` sites) | `pipe2`, `dup`, `dup2`/`dup3`, `close`, `fcntl` |
| Resource limits | `getrlimit`, `setrlimit`, `rlimit`, `rlim_t`, `RLIM_INFINITY`, 16 `RLIMIT_*` | `builtins.rs:1905–2019` (`ulimit`) | `SYS_prlimit64` covers both |
| umask | `umask`, `mode_t` | `builtins.rs:1840–1855` | direct |

Roughly **25 syscalls** and a few dozen constants/struct layouts cover
everything.

## 3. Options

| Option | New code | Removes glibc from binary | Risk | Verdict |
|---|---|---|---|---|
| Keep `libc` crate (status quo) | 0 | no | none | fine; it's the industry-standard, zero-cost binding |
| Adopt `rustix` (existing raw-syscall crate) | 0 | no | low | the honest benchmark: it already does exactly what rusty_libc would, battle-tested, and is what std itself is migrating toward |
| **Build `rusty_libc` (Goal A)** | ~1.5–2.5k LOC | no | medium (two sharp edges, §4) | viable as a deliberate, educational, dependency-zero project; plan below |
| Full libc replacement (Goal B) | tens of kLOC | yes | very high | don't; use static musl if a glibc-free binary is the goal |

The remainder assumes we're building `rusty_libc` for its own sake
(zero third-party deps, full control) with eyes open about what it does and
doesn't buy.

## 4. The hard parts (in order of danger)

### 4.1 `signal()` → `rt_sigaction` needs a hand-written signal trampoline

The kernel `sigaction` is not glibc's: the kernel struct wants
`SA_RESTORER` set and a `sa_restorer` pointer to a function that executes
`SYS_rt_sigreturn` on x86_64 — glibc normally supplies this trampoline.
rusty_libc must ship its own, in `core::arch::asm!` (a `mov rax, 15; syscall`
stub that must **not** be inlined, must not touch the stack, and needs
correct unwind/CFI treatment or at minimum `nop`-padding conventions).
Getting it wrong means a crash on the *first delivered signal* — for a shell
that traps SIGTERM/SIGHUP and juggles SIGCHLD, that's immediately. On
aarch64 the kernel provides a default restorer via the vDSO, so it's
x86_64-specific pain. Also: kernel `sa_mask` is 8 bytes
(`sigsetsize = 8`), not glibc's 128-byte `sigset_t`.

rush's handlers are already minimal and async-signal-safe
(`trap.rs:26` writes one atomic), which helps — nothing about the *handlers*
needs to change, only installation.

### 4.2 Raw `fork()` vs. rush's helper threads

glibc's `fork()` is not just `SYS_clone`: it runs `pthread_atfork` handlers
and, critically, resets glibc-internal locks (malloc arena locks, stdio
locks) in the child. rush spawns short-lived helper threads that allocate
(`exec.rs:1323` here-doc feeder, `exec.rs:2148`). If a raw
`SYS_clone(SIGCHLD)` fires while a helper thread holds the glibc malloc
lock, the forked child inherits a locked heap and **deadlocks on its first
allocation** — and rush's forked children (`run_subshell_forked`,
`capture_compound`, coproc) don't `exec`; they keep running the full Rust
interpreter, allocating freely. glibc's fork protects against this; a raw
syscall cannot, because the locks are glibc-private.

This is the single biggest correctness risk. Options, best first:

1. **Keep `fork` on glibc** (via std's `libc` linkage or one `extern "C"`
   declaration of our own — no `libc` crate needed) and raw-syscall
   everything else. Pragmatic, loses purity on one symbol.
2. Guarantee helper threads are quiescent before any fork (join them, or
   move here-doc feeding after the fork). Auditable today but a standing
   landmine for future code.
3. Accept the race. Not acceptable for a shell.

Note the same reasoning is why we should **not** replace
`std::process::Command` spawning (`posix_spawn` under the hood) with our own
fork/exec — std's path is correct, vfork-fast, and reports exec failures
properly (`job.rs:303` comment relies on this distinction).

### 4.3 `termios` struct ABI mismatch

The kernel's `struct termios` (used by `TCGETS`/`TCSETSW` ioctls) has
`NCCS = 19` and no `c_ispeed`/`c_ospeed` fields on x86_64; glibc's has
`NCCS = 32` plus speed fields, and glibc's `tcsetattr` translates. rusty_libc
must define the **kernel** layout and use it consistently —
copy-pasting glibc-shaped struct definitions produces silent stack
corruption on the ioctl. Same class of care applies to `sigaction` (§4.1)
and `rlimit` (use `prlimit64`'s always-64-bit struct and skip the legacy
`getrlimit` width games).

### 4.4 Architecture divergence

Syscall numbers differ per arch, and aarch64 **removed** legacy syscalls
rush's surface maps to: no `fork` (use `clone`), no `dup2` (use `dup3`), no
`pipe` (use `pipe2`), no `poll` (use `ppoll`). rusty_libc needs a per-arch
number table and, in a few cases, per-arch call shapes. Each new
architecture is a new table + asm stub + CI target.

### 4.5 errno and std interop

Our syscall layer gets errors as `-4095..-1` return values and should expose
`Result<T, Errno>` directly — cleaner than C's `errno`. But it must not
*write* glibc's TLS `errno`, so `std::io::Error::last_os_error()` is wrong
after a rusty_libc failure; call sites must build
`io::Error::from_raw_os_error(e)` from the returned code. rush's call sites
mostly check return values directly, so the migration is mechanical, but
every site needs eyes.

### 4.6 Portability hard stop: macOS/BSD

Linux is the only major kernel with a **stable public syscall ABI**. On
macOS, bypassing libSystem is unsupported and breaks across OS releases;
OpenBSD actively enforces libc-only syscall origins. Therefore rusty_libc is
necessarily `cfg(target_os = "linux")`. rush's `cfg(unix)` code must either
keep the `libc` crate for non-Linux Unix or gate those platforms out. The
clean shape: rush grows a thin internal `sys` facade; Linux backs it with
rusty_libc, other Unixes back it with the `libc` crate (or rush declares
Linux-only support).

## 5. What rusty_libc must export (the contract)

A drop-in for rush needs, minimally:

- **asm core**: `syscall0..syscall6` for x86_64 + aarch64; errno decode.
- **process**: `fork()` (see §4.2 decision), `wait4`/`waitpid` + `W*`
  status functions, `kill`, `killpg`, `getpid`, `getppid`, `getuid`,
  `setpgid`, `exit_group`.
- **signals**: `sigaction`-based `signal()`-alike + x86_64 restorer
  trampoline; the ~20 `SIG*` constants; `SIG_DFL`/`SIG_IGN`.
- **terminal**: kernel `Termios`, `tcgetattr`, `tcsetattr(TCSADRAIN)`,
  `tcgetpgrp`/`tcsetpgrp`, `isatty`, `winsize`/`TIOCGWINSZ` ioctl; flag and
  `c_cc` constants (`ICANON`, `ECHO`, `ISIG`, `IEXTEN`, `IXON`, `ICRNL`,
  `INLCR`, `VMIN`, `VTIME`).
- **fds**: `read`, `poll`, `pipe2`, `dup`, `dup3`, `close`,
  `fcntl`(`F_GETFD`/`F_SETFD`/`FD_CLOEXEC`).
- **limits/umask**: `prlimit64`-backed get/set, 16 `RLIMIT_*` constants,
  `RLIM_INFINITY`, `umask`.

`#![no_std]` core (it needs nothing from std), safe wrappers returning
`Result`, `unsafe` confined to the asm module and struct-pointer ioctls.

## 6. Effort estimate and phasing

Behind a cargo feature (`rusty-libc`), default staying on the `libc` crate
until the test suite passes both ways on both arches. rush's PTY-based
integration tests (`tests/pty`) are the real safety net here — they exercise
job control, raw mode, and signals end to end.

| Phase | Scope | Risk | Estimate |
|---|---|---|---|
| 1 | asm core + fds, poll/read, ioctl (winsize), umask, prlimit64/ulimit, pids, setpgid, tcsetpgrp, isatty | low — every call is a straight syscall | ~1–2 weeks, ~700–1000 LOC incl. tests |
| 2 | termios raw-mode path (kernel struct, TCGETS/TCSETSW) | medium (§4.3) | ~3–4 days |
| 3 | signals: rt_sigaction + restorer trampoline | high (§4.1) | ~1 week incl. signal-storm stress tests |
| 4 | fork + wait4 + status macros, per §4.2 decision | high (§4.2) | ~1–2 weeks incl. fork-under-thread-load stress tests |
| 5 | second arch (aarch64) + CI matrix (x86_64/aarch64 × feature on/off) | medium | ~1 week |

**Total: roughly 4–6 weeks** to a Linux-only, dual-arch replacement of the
`libc` crate, plus permanent ownership of a per-arch ABI surface that the
`libc` crate/rustix maintainers currently own for us. Rust std continues to
link glibc either way (Goal A ≠ Goal B).

## 7. Recommendation

- If the motivation is **dependency hygiene or binary purity**: static musl
  builds (zero code) or `rustix` (zero code, removes the FFI-declaration
  crate) deliver more for less; the status quo is also defensible since
  `libc` is a declarations-only crate.
- If the motivation is **owning the stack / learning / zero third-party
  deps** — the premise of rusty_libc — the project is well-scoped and
  tractable: the surface is small (§5), and rush's test suite can validate
  it. Follow the phasing in §6, keep it Linux-only, put a `sys` facade in
  rush, and make an explicit early decision on §4.2 (recommended: leave
  `fork` and `std::process::Command` on glibc's implementations; raw-syscall
  everything else).
