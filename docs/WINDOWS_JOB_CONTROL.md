# Windows background jobs — a design doc, not yet implemented

Written in response to a review request to scope closing Windows' job-control
gap, without writing the implementation itself: the Win32 FFI this needs is
hard to get right blind, and this session has no Windows machine to verify
against interactively — only after-the-fact build/test signal from CI's
`windows-latest` runner. This document is the foundation for a future
session (or a human contributor) to implement against, staged so review and
CI feedback can happen incrementally rather than in one large unverifiable
patch.

**Scope decision (already made, not this document's to revisit):** background
jobs only — `cmd &`, `jobs`, `wait`, `kill`, a real `$!`. Explicitly *not* in
scope: `fg`/`bg` terminal hand-off, Ctrl-Z suspend/resume, process
substitution, `coproc`. See "Deliberately out of scope" below for why each of
those is a materially harder, separate problem.

## Starting point: what `docs/ARCHITECTURE.md` already establishes

The "Windows strategy (G11)" section of `ARCHITECTURE.md` (`job.rs` writeup)
already did the hard verification work of confirming *why* Windows is
foreground-only today, and it's worth restating precisely so this document
doesn't quietly contradict it:

- `#[cfg(unix)]`/`#[cfg(not(unix))]` are decided by the Rust *target triple*.
  Every Windows target (`x86_64-pc-windows-msvc`, `x86_64-pc-windows-gnu`,
  including under MSYS2) sets `cfg(windows)`, never `cfg(unix)` — verified by
  actually cross-compiling. So `job.rs` (`#[cfg(unix)] mod job;`) never
  compiles in on Windows, full stop, not as a policy choice but because
  there's no code path that would reach it.
- `job.rs`'s actual implementation is POSIX process groups and signals via
  `libc`/`rusty_libc` (`setpgid`, `tcsetpgrp`, `WIFSTOPPED`, `SIGTSTP`, …) —
  none of which have a Windows equivalent to call into even if the module did
  compile.
- Within the foreground-only ceiling, Windows is already at parity for the
  everyday loop: builtin/function dispatch, `$$`/`$BASHPID`, pipes, and
  redirects (via `winstdio`'s std-handle-slot facade, `redirect_stdio`'s
  non-Unix twin).

This document proposes a **new**, Windows-native mechanism — not a port of
`job.rs`'s `libc` calls, which have no target. Job Objects and process groups
are a different model from POSIX process groups, not a translation of it.

## Why Job Objects, not raw `CreateProcess`

Today, `exec::run_background`'s non-Unix arm (`src/exec.rs`, next to
`spawn_failure_status`) is a one-line `Err("background jobs are not
supported on this platform")`. A naive fix — spawn via
`std::process::Command` and just don't `.wait()` — would "work" for the
single-process case, but silently breaks the moment the backgrounded command
is itself a pipeline or spawns children of its own: there's no way to `kill`
the whole tree, `wait` doesn't know what "done" means beyond the one direct
child, and a backgrounded job that outlives the shell has nothing tracking
it at all.

**Windows Job Objects** are the actual right primitive here — conceptually
the closest Windows analog to a POSIX process group for this purpose (not a
perfect match; see "Terminology" below):

- `CreateJobObjectW` creates a job (a Windows kernel object, not a Unix
  `fork`-style job — see terminology note).
- `AssignProcessToJobObject` puts a freshly spawned process (started with
  `CREATE_SUSPENDED`, before its main thread runs) into the job.
- `SetInformationJobObject` with `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` and
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` means: close the job handle (e.g. on
  shell exit) and every process in it dies, too — the Windows equivalent of
  the process-group-wide cleanup `job.rs` gets from `libc`'s signals.
- Any child the backgrounded process itself spawns inherits job membership
  automatically (a job property, not something the child opts into) — this
  is what makes a job object correct for a whole subtree, not just the one
  directly-spawned process.
- `TerminateJobObject` kills every process in the job in one call — backs
  `kill %n`.
- `QueryInformationJobObject` with `JobObjectBasicProcessIdList` enumerates
  live member pids — one way to poll "is this job still running" for `wait`
  without a blocking-wait primitive (see below).

**Terminology note**, worth being explicit about since "job" means two
different things here: bash's own "job" (one `&&`/`||`/pipe chain
backgrounded together, tracked in `job.rs`'s `JobEntry`/`State`) and a
Windows "Job Object" (a kernel handle that groups processes for
lifetime/resource management) are different concepts that happen to share a
name. The proposed design uses one Windows Job Object per shell job — a
clean 1:1 mapping, not a coincidence to paper over.

## Proposed shape: a new `src/winjob.rs`, mirroring `job.rs`'s surface

Not a shared module with `job.rs` — the implementations have nothing in
common at the syscall level, same reasoning `sys.rs` splits by backend
already documents. A new `#[cfg(not(unix))] mod winjob;` alongside the
existing `#[cfg(unix)] mod job;` in `lib.rs`, matching the same public
surface `exec.rs`/`builtins.rs` already call through so the call sites don't
need `cfg`-splitting themselves beyond what they already have:

```rust
// src/winjob.rs — sketch, not implemented
pub fn run_background(pipeline: &Pipeline) -> Result<(), String> { .. }
pub fn is_builtin(name: &str) -> bool { .. }      // "jobs" | "wait" | "kill" | "disown"
pub fn builtin(argv: &[String]) -> Option<i32> { .. }
pub fn ids() -> Vec<usize> { .. }
pub fn count() -> usize { .. }
pub fn job_control_enabled() -> bool { .. }        // always false — see below
```

`exec::run_background`'s `#[cfg(not(unix))]` arm becomes `winjob::run_background`
instead of the current `Err(...)`; `builtins.rs`'s `other_is_builtin`/
`other_names` (currently `#[cfg(not(unix))]` → empty) route to `winjob`
instead. `job_control_enabled()` should stay `false` on Windows even after
this lands — that flag gates `fg`/`bg`/Ctrl-Z UI surface this design
deliberately doesn't implement (see below), not background-job capability
itself.

### Per-job state

A `JobEntry`-equivalent needs, per backgrounded job:
- The Job Object `HANDLE` (owns the kill-on-close semantics).
- The directly-spawned process's `HANDLE` and pid (`$!`, `jobs`' own
  listing, matching `job.rs`'s own fields).
- The job number (`%1`, `%2`, …) and original command text (`jobs`' display
  — `job.rs` already tracks this, reusable as-is, it's platform-neutral).
- Running/done state — Windows has no `SIGCHLD` push notification. The
  options are polling (`QueryInformationJobObject` or the simpler
  `GetExitCodeProcess` != `STILL_ACTIVE` on the tracked leader process) or a
  wait on the job's I/O completion port (`AssignProcessToJobObject` +
  `SetInformationJobObject` with `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`,
  which posts a message when the job's process count hits zero — the
  Windows-native "job finished" signal, closer in spirit to `SIGCHLD` than
  polling). The completion-port approach is more work but avoids a polling
  thread; a first cut could reasonably start with polling (bash-for-Windows
  ports like MSYS2's own bash do exactly this for `$!`/`wait` fallbacks) and
  upgrade later — call this out explicitly in whatever PR lands it, don't
  silently ship polling as if it were the final design.

### `wait` semantics

POSIX `wait` blocks on `SIGCHLD` via `waitpid`. Windows has
`WaitForSingleObject`/`WaitForMultipleObjects` on the tracked process
handle(s) directly — arguably *simpler* than the Unix path here, since a
process handle is natively waitable with no pid-reuse race to worry about
(a Windows pid isn't recycled while any handle to it is still open, unlike
POSIX). `wait %n`/`wait pid`/bare `wait` map onto
`WaitForSingleObject`/`WaitForMultipleObjects(..., FALSE, INFINITE)`
directly on the stored handles. `wait -n` (C64 on Unix) is
`WaitForMultipleObjects(..., FALSE, ...)`'s native "any one" mode — also a
closer match than the Unix implementation needed.

### `$!` and exit status

Already-shipped machinery in `vars.rs` for `$!`/`$?`/`${PIPESTATUS[@]}` is
platform-neutral (it just stores integers) — no change needed there.
`GetExitCodeProcess` after the wait maps to the same integer `vars::set_last_status`
already expects.

## Deliberately out of scope, and why each is a separate, harder problem

Listed so a future continuation of this design doesn't quietly assume any of
these come along for free with the above:

- **`fg`/`bg` (terminal hand-off).** POSIX job control's `tcsetpgrp` model —
  "which process group owns the controlling terminal's input" — has no
  Windows equivalent at all in the same shape. Windows' nearest concept is
  console *process groups* (`CREATE_NEW_PROCESS_GROUP`,
  `GenerateConsoleCtrlEvent`) layered on top of the *separate* console
  attach/detach model (`AllocConsole`/`FreeConsole`/`AttachConsole`) — input
  routing on Windows consoles doesn't foreground/background the way a Unix
  tty's line discipline does. Real Windows shells (PowerShell, cmd.exe) don't
  have `fg`/`bg` in the bash sense either — this isn't rush being behind a
  solved problem, it's a genuinely different terminal model. Worth a fully
  separate design pass if ever pursued, likely starting from how WSL's own
  bash fakes it (it doesn't — WSL runs a real Linux kernel with real process
  groups; not a source of prior art for native Windows).
- **Ctrl-Z suspend/resume.** No `SIGTSTP`/`SIGCONT` equivalent. Windows can
  suspend a process's threads (`NtSuspendProcess`, undocumented but stable in
  practice, or `SuspendThread` per-thread) but there's no console-level
  keystroke wired to it the way a tty's line discipline delivers `SIGTSTP`
  from Ctrl-Z — would need a console control handler
  (`SetConsoleCtrlHandler`) intercepting a chosen key combination, which
  fights with Windows' own Ctrl-C/Ctrl-Break handling on the same API.
- **Process substitution (`<(cmd)`/`>(cmd)`).** Needs a real pipe exposed to
  a child as a path (`/dev/fd/N` on Unix). Windows has no fd-namespace
  equivalent; the closest primitive (a named pipe,
  `\\.\pipe\...`) is a different addressing scheme a spawned child would need
  to know to open specially — not a drop-in substitute for a path argument
  arbitrary programs already know how to open. `exec.rs`'s existing
  `#[cfg(not(unix))]` stub (`"process substitution is not supported on this
  platform"`) reflects that this needs its own design, not a corollary of
  background-job support.
- **`coproc`.** Needs everything process-substitution needs (a real
  bidirectional pipe visible to a child) plus the background-job tracking
  this document *does* propose — so it's blocked on both, and still a
  separate follow-up even once this lands.

## Risk: zero interactive verification in this environment

This is the reason implementation was deferred rather than attempted this
session. Concretely:

- CI's `windows-latest` runner gives build success/failure and
  `cargo test --verbose` pass/fail — real signal, but only for whatever the
  existing `tests/exec_behavior.rs` integration suite actually drives via
  subprocess. It won't catch a job that "runs but `jobs`/`wait`/`kill` report
  it wrong" unless a new Windows-specific integration test explicitly checks
  that.
- There's real precedent in this repo for a platform-specific runtime-behavior
  test harness this couldn't get purely from `cargo test`:
  `tests/pty/editor_pty_test.py` (a Python `pty.fork()` harness added for the
  line-editor rewrite) drives the *built binary* under conditions `cargo
  test` alone can't simulate. A Windows equivalent — a small script using
  `pywinpty` or the `ConPTY` API to drive the built `rush.exe` and assert on
  `jobs`/`wait`/`kill` output — is the right shape of test to add alongside
  the implementation, not after it, precisely because this design can't be
  hand-verified interactively first.
- Recommended staging for whoever picks this up: land the `winjob.rs` skeleton
  and `run_background` wiring behind the new integration tests *first* (so
  CI is the safety net from commit one), starting with the single-process
  case (`sleep 5 &` executing a real Windows `ping -n 5 127.0.0.1 >nul`-style
  external command, not a builtin — a background builtin has its own
  separate can of worms around `winstdio`'s process-global std-handle slots
  that a background job would race against the foreground shell for, worth
  flagging as a likely-necessary narrowing: "backgrounding a builtin"
  probably needs to stay unsupported even after this lands, mirroring the
  narrowing bash's own `job.rs` already documents for other edge cases
  (see C37's pipeline-stage narrowing in `docs/CAPABILITY_GAPS.md` for the
  established pattern of shipping a real, narrower slice with the rest
  explicitly documented rather than blocked on).

## Suggested staging (smallest reviewable increments)

1. `winjob.rs` skeleton + `CreateJobObjectW`/`AssignProcessToJobObject`
   FFI declarations (hand-rolled `#[link(name = "kernel32")]`, matching
   `winstdio.rs`'s existing convention — no new crate dependency), wired to
   `exec::run_background` for the single-external-command case only. A new
   Windows-only integration test (subprocess-driven, no ConPTY needed yet)
   asserting `sleep_like_cmd &; echo done` returns immediately and `jobs`
   shows one entry.
2. `wait`/`$!`/exit status, via `WaitForSingleObject` on the tracked handle.
3. `kill %n` via `TerminateJobObject`.
4. `jobs -l`/multi-job listing parity with the Unix builtin's own output
   format (reuse `job.rs`'s display formatting logic where it's already
   platform-neutral, rather than reimplementing it).
5. Only then: evaluate whether the polling-based done-detection from step 1
   is worth upgrading to the I/O-completion-port approach, based on whatever
   real usage/perf signal shows up.

Each step above should be its own PR — small enough for CI's after-the-fact
signal to be a meaningful check, in keeping with why this was scoped as a
design doc rather than one large implementation this session.
