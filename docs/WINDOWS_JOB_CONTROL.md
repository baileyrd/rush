# Windows background jobs

Originally written as a design doc, not yet implemented — scoping closing
Windows' job-control gap without writing the implementation itself, staged
so review and CI feedback could happen incrementally rather than in one
large unverifiable patch. **Milestones 1–4 of the staging plan below have
since landed, plus `disown` and backgrounded pipelines of external
commands**: `src/winjob.rs`, backed by
[rusty_win32](https://github.com/baileyrd/rusty_win32)'s `job`/`process`
modules (Job Objects, `CreateProcessW`-with-`CREATE_SUSPENDED`) rather than
the originally-sketched hand-rolled FFI, since that crate now exists and
already provides exactly these primitives, verified independently against
real `windows-latest` CI. `cmd &`/`jobs`/`wait`/`kill`/`disown`/`$!` all
work now, for external commands, single-stage or piped together; see
`winjob.rs`'s own module doc for its one remaining, *permanent* narrowing
(not a staging gap) — a builtin, function, or compound command as a
pipeline stage, since Windows has no `fork()` for it to run in a
background child the way `job.rs`'s Unix `spawn_builtin_stage`/
`spawn_compound_stage` do. The rest of this document — the design
rationale, "Deliberately out of scope", and the staging plan's own
step-by-step detail — is otherwise unchanged from the original design
pass.

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

## Shape as implemented: `src/winjob.rs`, mirroring `job.rs`'s surface

Not a shared module with `job.rs` — the implementations have nothing in
common at the syscall level, same reasoning `sys.rs` splits by backend
already documents. `#[cfg(not(unix))] pub mod winjob;` sits alongside the
existing `#[cfg(unix)] pub mod job;` in `lib.rs`, matching the same public
surface `exec.rs`/`builtins.rs` call through so the call sites don't need
`cfg`-splitting themselves beyond what they already have. Milestone 1's
actual surface (see `winjob.rs` for the up-to-date signatures as later
milestones grow it):

```rust
pub fn run_background(pipeline: &Pipeline) -> Result<(), String> { .. }
pub fn is_builtin(name: &str) -> bool { .. }      // "jobs" so far; wait/kill/disown land with their milestones
pub fn builtin(argv: &[String]) -> Option<i32> { .. }
pub fn ids() -> Vec<usize> { .. }
pub fn count() -> usize { .. }
pub fn reap_background() { .. }                    // called each prompt, main.rs's non-Unix counterpart to job::reap_background
```

`exec::run_background`'s `#[cfg(not(unix))]` arm calls `winjob::run_background`
instead of the original `Err(...)` stub; `builtins.rs`'s `other_is_builtin`/
`other_names`/`other_builtin` route to `winjob` instead of their original
empty/`None` stubs. No `job_control_enabled()` was added to `winjob`'s own
surface: nothing outside `job.rs` itself calls that function generically
(every call site is already `#[cfg(unix)]`-gated), so there was nothing to
route to it — whether to announce `[id] pid` interactively is decided
directly from `vars::interactive()` at the one call site that needs it
instead. That flag gates `fg`/`bg`/Ctrl-Z UI surface this design
deliberately doesn't implement (see below), not background-job capability
itself, so its absence here doesn't foreclose adding it later if `fg`/`bg`
ever do land.

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
  silently ship polling as if it were the final design. **As implemented**:
  `winjob.rs` took the polling first cut (`GetExitCodeProcess` via
  `rusty_win32::process::wait` with a zero timeout, on the directly-spawned
  process only — not yet the whole job subtree via `process_ids`), exactly
  as this section anticipated; the completion-port upgrade remains a
  follow-up, unstarted.

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

1. **Done.** `winjob.rs` skeleton, wired to `exec::run_background` for the
   single-external-command case only. Built on `rusty_win32::job`/
   `rusty_win32::process` (that crate's own `job`/`process` modules,
   verified independently on real `windows-latest` CI) rather than the
   hand-rolled FFI originally sketched here — it now exists and already
   provides `CreateJobObjectW`/`AssignProcessToJobObject`/
   `CreateProcessW`-with-`CREATE_SUSPENDED`/`ResumeThread`, so duplicating
   those declarations in rush itself would've been pure redundancy.
   `$!`/`jobs`/`\j` work; `wait pid`/`wait %n`/`kill`/`disown` don't exist
   yet at this step (`jobs` is the only builtin `winjob::NAMES` lists so
   far — steps 2–3 and the "Still open" note below add the rest).
   `tests/windows_job_control.rs` covers this: backgrounding returns
   immediately and is listed, `$!` is the backgrounded pid, and background
   pipelines/builtins are rejected outright rather than silently doing the
   wrong thing (the narrowing this section anticipated, confirmed
   necessary and implemented as such — a background builtin would indeed
   race `winstdio`'s process-global std-handle slots against the
   foreground shell).
2. **Done.** `wait [-n] [-p var] [pid|%job ...]`, via
   `WaitForSingleObject` (`rusty_win32::process::wait` with an infinite
   timeout) on the tracked process handle directly — mirrors
   `job.rs::wait_cmd`'s own argument handling almost exactly. `wait -n`
   blocks on every not-yet-finished job's handle at once via
   `rusty_win32::process::wait_any` (`WaitForMultipleObjects(...,
   FALSE, ...)`, added specifically for this) rather than the short-sleep
   polling loop the first cut used — the follow-up this section originally
   flagged as open, now closed. `WaitForMultipleObjects` caps at 64
   handles per call; `wait_next` batches the first 64 tracked jobs into
   one blocking call and falls back to a short-sleep poll across sweeps
   only in that (realistically never-hit) overflow case, rather than
   silently ignoring anything past the 64th. A `REAPED` map (matching
   `job.rs`'s own) lets a second `wait` on an already-settled pid still
   report its status.
3. **Done.** `kill [-SIG|-s SIG] %n` via `TerminateJobObject`, with a fixed
   conventional exit code (128+15) reported back through `wait`/`$?` for
   every kill — Windows has no real signal delivery, so *which* signal was
   requested can't actually be honored, only "terminate it can"; the flag
   is still accepted (not rejected) for script portability. Only `%n`
   targets are supported, not a bare pid: `rusty_win32` has no raw
   `TerminateProcess`, only `TerminateJobObject`, which needs the job
   handle a `%n`-tracked entry has and an arbitrary pid doesn't.
4. **Done** (with one intentional narrowing). `jobs` gained `-l`/`-p`/
   `-r`/`-s`, matching `job.rs::jobs_cmd`'s flags with one exception:
   `-n` (changed-since-last-notification) isn't implemented, since it
   needs per-job "already told you" bookkeeping this module doesn't keep
   (see `winjob.rs::jobs_cmd`'s own doc comment). `-s` (stopped-only)
   is accepted but always prints nothing — Windows background jobs have
   no Stopped state (no Ctrl-Z).
5. Only then: evaluate whether the polling-based done-detection from step 1
   is worth upgrading to the I/O-completion-port approach, based on whatever
   real usage/perf signal shows up. Still open; the `wait -n`
   polling-vs-`WaitForMultipleObjects` question step 2 flagged is no
   longer part of this — `wait_any` closed it.

**`disown`** (never explicitly staged above as its own numbered step, but
listed in the original `winjob.rs` surface sketch) is **done**, and turned
out to need a real primitive addition, not just table bookkeeping: a job
created with kill-on-close ties its member process's lifetime to the job
handle staying open in *this* process, which closes implicitly at the
owning process's own exit — so simply dropping a `winjob.rs` table entry
and closing its handles the way `job.rs`'s own Unix `disown_cmd` does
(where a pid is already independent of anything the shell holds) would
kill the process on the spot, or at the latest when the shell itself
exits. `rusty_win32` gained `job::clear_kill_on_close` (the reverse of
`set_kill_on_close`) specifically so `disown` can reverse that limit
before releasing the handles — the actual "detach" operation here.
`tests/windows_job_control.rs::disown_detaches_the_job_while_the_shell_is_still_running`
proves the reversal actually takes effect, checked from *within* the
still-running shell right after `disown` runs.

**Known caveat, found via real CI rather than assumed:** an earlier
version of that test tried to prove survival from *outside* the `rush -c`
process, after it had already exited — the stronger claim `disown` is
really for. It failed consistently on `windows-latest`, even though
`clear_kill_on_close` never reported an error and the process was
confirmed alive moments earlier from inside the shell. The likely
explanation: `clear_kill_on_close` only clears kill-on-close on the job
*this shell created*; it can't detach a process from an *ambient* job the
shell's own process might already be nested in (Windows automatically
nests every child a job member spawns into that same job too), and
GitHub Actions' Windows runners are documented to wrap each step's
process tree in exactly such a job for orphan cleanup. There's no
portable way to detect or opt out of this from inside the shell, so
`disown`'s test coverage only asserts what's actually attributable to
`winjob.rs`/`rusty_win32`'s own code, not the sandbox a background job
happens to run under — see `winjob.rs::disown_cmd`'s own doc comment.

**Backgrounded pipelines of external commands** (`cmd1 | cmd2 | ... &`, the
narrowing milestone 1 originally accepted — "no pipelines" — as a first
cut) are **done**. `winjob::spawn_pipeline_into_job` spawns every stage
suspended, connected by real anonymous pipes
(`rusty_win32::handle::create_pipe`), assigns each to the *same* Job
Object as it goes, and resumes it — the same "assign before resume"
guarantee the single-stage case always had, just applied per stage. `$!`
and the tracked process for `wait`/`jobs` polling are the pipeline's
*last* stage, matching bash's own convention (verified directly, per
`job.rs`'s own comment on the same rule); `kill %n` reaches every stage at
once via the shared job, not just the last one, since `TerminateJobObject`
terminates every process currently assigned to a job in one call.
Tracking each stage's own individual exit status (a Windows
`${PIPESTATUS[@]}` equivalent) isn't attempted — a possible follow-up, not
needed for `wait`/`kill`/`jobs` to work correctly on the pipeline as a
whole.

A pipeline stage that's a builtin, function, or compound command remains
rejected — explicitly confirmed as the **permanent** limitation the
original design doc's own "worth flagging as a likely-necessary
narrowing" comment anticipated, not a gap staged for later: Windows has
no `fork()`, so there's no way to run one of those in a background child
process the way `job.rs`'s Unix `spawn_builtin_stage`/
`spawn_compound_stage` do. `winjob::run_background` validates every stage
of a pipeline up front, before touching any OS resources, so this fails
loudly and immediately rather than silently running only the external
stages or leaving the pipeline half-wired.

Real `windows-latest` CI caught a genuine bug in the first cut of this:
a stage's own explicit redirect (`findstr ... > file`) silently never
reached the spawned child, even though the pipe-to-pipe wiring between
stages worked fine. The cause: a Windows `HANDLE` starts non-inheritable
regardless of how it was created (`rusty_win32::handle`'s own doc
comment) — `spawn_stage` already marked the pipe-boundary handles
(`stdin_src`/`stdout_dst`) inheritable before spawning, but never did the
same for a handle `exec::redirect_stdio` itself opened for an explicit
stage redirect, since that mechanism was previously only ever exercised
for builtins (no child process, so inheritability was moot) or the
foreground path (which spawns via `std::process::Command`, a completely
different, already-correct mechanism). This was the first test to check
a backgrounded pipeline stage's actual redirected *content* rather than
just its exit code, which is why it went uncaught through every earlier
milestone. Fixed by having `spawn_stage` capture each std slot's value
before this stage touches anything, then — right before spawning — mark
inheritable whichever slot(s) now differ from that baseline, covering
both the pipe ends and anything `redirect_stdio` opened, while leaving a
slot still at its baseline (the shell's own real stdio, untouched by
this stage) alone.

Each step above should be its own PR — small enough for CI's after-the-fact
signal to be a meaningful check, in keeping with why this was scoped as a
design doc rather than one large implementation this session.
