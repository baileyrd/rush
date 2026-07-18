# Windows backend analysis: what rush needs, and what `rusty_win32` must provide

Inventory of rush's exact platform-primitive needs on Windows, mirroring
`docs/LIBC_DEPENDENCY_ANALYSIS.md`'s role for the Unix/`rusty_libc` side.
Companion design/requirements doc lives in the `rusty_win32` repo (its
handoff prompt, which explicitly required this document to exist before any
`rusty_win32` code gets written — this is that document).

## 1. What "Windows support" already means for rush, and what this scopes

Unlike the Unix side, there is no single `libc`-shaped dependency to
replace. Rush's Windows build already compiles and runs today — CI's
`windows-latest` job in `.github/workflows/ci.yml` proves it — using two
things that need no `rusty_win32` work at all:

- **`std::process::Command`** for spawn/wait (`src/exec.rs:2421`'s shared
  `run()`), which resolves to `CreateProcessW`/`WaitForSingleObject`
  under the hood. This is the foreground-only baseline
  `docs/ARCHITECTURE.md`'s "Windows strategy (G11)" section documents.
- **`src/winstdio.rs`**, a hand-rolled `extern "system"` facade over
  `GetStdHandle`/`SetStdHandle`, already the Windows counterpart of the
  Unix-only `src/sys.rs` for exactly the fd-0/1/2 redirect case.
- **`rusty_lines`** (a separate crate, not this repo), which owns raw
  terminal mode, Ctrl-C-during-input, and `$COLUMNS`/`$LINES` window-size
  queries. Its own Windows story is out of scope for this document and for
  `rusty_win32`'s first cut — noted here only so its gaps aren't
  double-counted below.

So the goal of `rusty_win32` is **not** "make rush's Windows build exist" —
it already does. The goal is to close the specific, catalogued gaps between
that foreground-only baseline and Unix parity: job control, coprocess/
process-substitution pipe tricks, signal/trap delivery beyond idle-prompt
Ctrl-C, and the fd-3-and-up handle bookkeeping Windows has no kernel
primitive for at all.

## 2. Inventory of the current surface

52 `#[cfg(not(unix))]`/`#[cfg(windows)]` attribute sites across 8 files
(`builtins.rs`: 18, `exec.rs`: 22, `vars.rs`: 4, `main.rs`: 3, `expand.rs`:
2, `lib.rs`/`completion.rs`/`winstdio.rs`: 1 each), plus two structurally
Unix-only modules (`src/job.rs` gated at the `mod` declaration in `lib.rs`,
and `src/trap.rs`'s signal-delivery half gated function-by-function rather
than at the module level). Grouped by theme, with a verdict on whether
`rusty_win32` work would actually change anything:

| Theme | Sites | Verdict |
|---|---|---|
| Process spawn/wait (baseline) | 2 | Already fine — `std::process::Command`, no gap |
| Job control / background jobs / pgid | 7 | **Real gap** — hard stubs or silently absent; scoped by `docs/WINDOWS_JOB_CONTROL.md` |
| Subshell / capture isolation (no `fork`) | 8 | Working alt-impl (self-re-exec) for the common case; gap for a compound command captured mid-pipeline |
| Coprocess (`coproc`) | 1 | **Real gap** — hard stub |
| Process substitution (`<(cmd)`/`>(cmd)`) | 5 | **Real gap** — hard stub, no `/dev/fd` equivalent |
| Signals / traps / Ctrl+C | 9 | Idle-prompt Ctrl-C already works (via `rusty_lines`); `TERM`/`HUP` trap registration is a **silent no-op** (accepted, never fires); foreground-child Ctrl-C behavior unverified |
| fds / pipes / dup / redirects | 9 | fd 0-2 fine (`winstdio`); fd 3+ / `{name}>` varfd / coprocess pipe-sharing = **real gap** |
| `rlimit`/`ulimit`/`umask` | 2 | Real gap, but mostly non-portable by nature |
| File metadata / ownership (uid/gid, `-ef`, fifo/socket tests) | 6 | Honest degradation (false/0/None); one real fix available (`same_file`) |
| Terminal/TTY queries (`isatty`, `read -t`) | 3 | `isatty` fine; `read -t N`-with-timeout degrades to "always ready" |
| `PATH`/executable resolution | 3 | Working alt-impl (`%PATHEXT%`), no gap |
| Identity/prompt vars (uid, prompt char) | 3 | Honest degradation, non-portable, low priority |
| `exec` argv0 surgery (`-a`/`-l`) | 1 | Permanent limitation — no Win32 equivalent exists |

## 3. Categorization against the handoff doc's primitive table

`rusty_win32`'s own design doc opens with a table mapping each rush
primitive to a Linux mechanism, a Windows reality, and a verdict ("no
mapping" / "different mechanism" / "close analog" / "genuine parallel").
Checked against the inventory above, that table holds, with one addition
and one correction:

- **Spawn a child** — confirmed no mapping, but with a wrinkle the handoff
  doc didn't anticipate: `std::process::Command` already covers *ordinary*
  foreground spawn/wait completely (§1). The actual `rusty_win32` need is
  narrower — a raw `CreateProcessW` wrapper is required specifically
  because **std discards the child's thread handle**, and starting a
  process suspended (`CREATE_SUSPENDED`) so it can be assigned to a Job
  Object *before* its main thread runs (`AssignProcessToJobObject`, per
  `docs/WINDOWS_JOB_CONTROL.md`) needs `ResumeThread` on that handle — a
  capability std's `Command` has no way to expose. So `rusty_win32::process`
  isn't "replace `std::process::Command`", it's "provide the raw spawn path
  Job-Object-integrated background jobs specifically require."
- **Signals** — confirmed different mechanism, and the inventory sharpens
  *which* signal-shaped need is real: `SetConsoleCtrlHandler` for
  `TERM`/`HUP`-equivalent graceful shutdown (`trap.rs`'s doc comment at
  line 1-6 already names this exact use case — a container's PID 1 —
  independent of any terminal). Idle-prompt Ctrl-C is not this crate's
  problem (owned by `rusty_lines`); foreground-child Ctrl-C targeting
  (does a Ctrl-C reach rush and the child at once, or just the child?) is
  a genuinely open question the inventory flags as unverified — see §4.
- **`waitpid`/`wait`** — confirmed handle-based, and already proven low-risk
  in practice: `WINDOWS_JOB_CONTROL.md` §"`wait` semantics" already worked
  out that `WaitForSingleObject`/`WaitForMultipleObjects` on stored process
  handles is arguably *simpler* than the Unix path (no pid-reuse race).
- **`rlimit`** — confirmed different granularity, but the inventory adds a
  sharper recommendation: most of rush's actual `ulimit` resources
  (`RLIMIT_CORE`, `RLIMIT_NPROC`, `RLIMIT_MEMLOCK`, …) have no Windows
  concept at all, and `umask` has *no* Windows kernel equivalent
  whatsoever (file creation isn't governed by a process-wide mask on
  Windows; ACLs are per-call). Recommend treating this as a low-priority,
  possibly "document as permanently unsupported" item rather than a real
  `rusty_win32` deliverable — see §7.
- **`dup`/`pipe2`** — confirmed close analog (`DuplicateHandle`/
  `CreatePipe`), but the inventory surfaces the real shape of the work:
  it's not really "port `dup`/`pipe`", it's "give rush a handle table at
  all." Windows has no kernel-level unification of small-integer fds
  beyond the three CRT std-handle slots `winstdio.rs` already owns — fd 3+,
  `{name}>` varfd redirects, and coprocess/process-substitution pipe
  sharing all fail today specifically because nothing maps a rush-chosen
  integer to a `HANDLE`. This is `rusty_win32`'s single largest surface
  once job control is set aside — see §4.
- **Raw terminal mode** — confirmed ConPTY is the right target, but this is
  `rusty_lines`' dependency to add, not something rush's own `cfg` sites
  call for directly (rush never touches terminal mode itself; it's fully
  delegated). Still correct to build in `rusty_win32` as the shared
  primitive, since `rusty_lines` would consume it the same way rush's
  Windows job-control and fd work would consume `rusty_win32::process`/
  `handle` — just not something *this* inventory found a rush-side call
  site for.
- **`clock_gettime` fast path** — no correction needed, but demoted in
  priority: rush uses `std::time` exclusively (no direct
  `QueryPerformanceCounter` call site anywhere in `src/`), and std's own
  Windows backend already uses QPC internally. `rusty_win32::time` is a
  nice-to-have, not something any cataloged gap depends on — matches the
  handoff doc's own "do this last" phasing.
- **Process groups / job-control-owns-the-terminal** — confirmed no
  mapping, and the inventory found the concrete place this bites: nothing
  in `src/exec.rs`'s off-Unix spawn path sets `CREATE_NEW_PROCESS_GROUP`,
  so a Ctrl-C during a foreground external command likely reaches rush and
  the child simultaneously today (unverified — no Windows machine in this
  session either, same constraint `WINDOWS_JOB_CONTROL.md` names). Flagged
  as needs-design in §4, not yet in any implementation's scope.

## 4. The hard parts (in order of danger)

### 4.1 Background jobs need Job Objects *and* a raw spawn path — not just Job Objects

`docs/WINDOWS_JOB_CONTROL.md` already designed the Job Object side in
detail (`CreateJobObjectW`, `AssignProcessToJobObject`,
`SetInformationJobObject` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`,
`TerminateJobObject`, `QueryInformationJobObject`). What it didn't need to
spell out, because it was scoping the *design* rather than the FFI layer:
correctly sequencing "spawn suspended → assign to job → resume" requires
the raw `PROCESS_INFORMATION.hThread` that `std::process::Command` never
surfaces. `rusty_win32::process::spawn_suspended` (or equivalent) needs to
own the full `CreateProcessW` call, not wrap `Command`.

This is the direct blocker for closing Theme 2 of the inventory (7 sites:
`run_background`, `job::is_builtin`/`NAMES`/`builtin`, `job::ids`/`count`,
`job::init`, `job::reap_background`) — none of it can start without this
primitive existing.

### 4.2 fd 3+ has no Windows analog to port — it needs a rush-owned handle table

Nine inventory sites (Theme 7) — `unsupported_fd`, `VarFd` in two separate
call sites, `extra_fds`'s silent-drop inconsistency, `clone_or_materialize`'s
pipe-sharing fallback — all trace to the same root cause: Windows has
`GetStdHandle`/`SetStdHandle` for exactly three slots and nothing else.
There is no kernel-level "fd table" to `dup`/`dup2` into past that.
`rusty_win32::handle` providing `DuplicateHandle`/`CreatePipe` wrappers is
necessary but not sufficient — rush itself will need to grow an internal
integer-to-`HANDLE` map to give `{name}>` varfd redirects and fd-3+
`Dup`/`Move`/`Close` redirects any meaning at all off Unix. Flag this
explicitly in whatever `rusty_win32` phase touches `handle`/`fd`: the crate
provides the primitives, but rush's own redirect machinery needs a
follow-up change to use them for anything beyond fd 0-2.

### 4.3 Process substitution has no path-based handle at all, unlike `dup`/`pipe`

Five sites (Theme 5), all downstream of one fact: bash's own
`<(cmd)`/`>(cmd)` on Linux works by handing the outer command a
`/dev/fd/<n>` path — a magic symlink with no Windows equivalent. Even with
`CreatePipe` + `CreateProcessW` available, there is no path syntax to give
the outer command a live handle by name. Two candidate approaches, neither
free: named pipes (`\\.\pipe\<generated-name>`, which the outer command
would need to open by that exact name rather than an ordinary path an
arbitrary program already knows how to consume) or a real temp file (loses
the never-blocks streaming property the current Unix `process_substitute`
doc comment calls out as load-bearing, verified against `diff <(sleep 1;
echo a) <(sleep 1; echo b)`). Needs its own design pass before
implementation — do not fold this into the Job Objects phase; it's a
separate, harder problem exactly as `WINDOWS_JOB_CONTROL.md`'s "deliberately
out of scope" section already predicted for the related `coproc` case.

### 4.4 `TERM`/`HUP` trap registration is a silent no-op today, not an error

The single most surprising finding in the inventory: `trap 'cmd' TERM` on
Windows is **silently accepted** (`trap::set`/`unset` in `src/trap.rs` are
not `cfg`-gated at all) but can never fire, because every function that
actually delivers a signal (`record_signal`, `install_signal_handlers`,
`check_pending`) is `#[cfg(unix)]`-only with no counterpart. A script
relying on graceful shutdown via `trap 'cleanup' TERM` gets no error and no
cleanup. `SetConsoleCtrlHandler` (`CTRL_CLOSE_EVENT`/`CTRL_LOGOFF_EVENT`/
`CTRL_SHUTDOWN_EVENT`) is the real target — `trap.rs`'s own doc comment
already names "a container's PID 1 catching TERM to shut down gracefully"
as the motivating case, and that case has a genuine Windows story via the
console control handler. Recommend this as an early, self-contained
`rusty_win32` + rush pairing: it needs no Job Object work and no fd-table
work, just `console::install_ctrl_handler` wired to `trap::record_signal`'s
existing (already-correct) atomic-store-then-check-at-safe-points pattern.

### 4.5 Foreground-child Ctrl-C targeting is unverified, not just unimplemented

Distinct from 4.4: today, nothing in the off-Unix spawn path requests
`CREATE_NEW_PROCESS_GROUP`, so a Ctrl-C during a running foreground external
command may reach rush and the child at the same time (both attached to the
same console) rather than being scoped to the child the way a Unix process
group scopes a terminal signal. Not confirmed at runtime — this sandbox has
no Windows machine, the same constraint `docs/ARCHITECTURE.md`'s G11
section and `WINDOWS_JOB_CONTROL.md` both already flag for their own
claims. Whoever picks up `rusty_win32::process` should verify this early,
since it changes whether `CREATE_NEW_PROCESS_GROUP` + targeted
`GenerateConsoleCtrlEvent` belongs in the *first* phase of background-job
work or can wait.

## 5. What `rusty_win32` must export (the contract)

Corrects the handoff doc's first-draft module table against what the
inventory actually shows rush needs, in priority order:

- **`process`** — NOT a general `CreateProcessW` wrapper competing with
  `std::process::Command` (that already works, §1). Scoped to exactly what
  Job Objects need: `spawn_suspended` (returns process handle + thread
  handle + pid), `resume(thread_handle)`. `wait`/`try_wait` via
  `WaitForSingleObject`/`GetExitCodeProcess` on the process handle,
  `current_pid` via `GetCurrentProcessId` — needed for `job.rs`'s existing
  pid-tracking fields to have a Windows equivalent to call.
- **`job`** (Job Objects, no Linux analog) — `CreateJobObjectW`,
  `SetInformationJobObject` (`JOBOBJECT_EXTENDED_LIMIT_INFORMATION`,
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), `AssignProcessToJobObject`,
  `TerminateJobObject`, `QueryInformationJobObject`
  (`JobObjectBasicProcessIdList`) — exactly `WINDOWS_JOB_CONTROL.md`'s
  proposed surface; this document adds nothing new here, just confirms it
  against the code-level inventory.
- **`handle`** (renamed from the handoff's `fd`, since Windows has no fd
  concept to name it after) — `DuplicateHandle`, `CreatePipe`,
  `SetHandleInformation` (`HANDLE_FLAG_INHERIT`, the inheritance-marking
  step Windows requires explicitly since handles are non-inheritable by
  default — the inverse of Unix's `CLOEXEC` default), `CloseHandle`. This
  is the primitive §4.2 and §4.3 both need; rush's own redirect/coproc/
  process-substitution code still has to build a handle table on top, but
  can't start without these.
- **`console`** — `SetConsoleCtrlHandler` first (closes §4.4, the
  highest-value/lowest-risk item in this whole inventory — no dependency
  on job/handle work). ConPTY (`CreatePseudoConsole`) and raw-mode get/set
  are lower priority *for rush itself* (owned by `rusty_lines`, §1) but
  belong in this crate as the shared primitive both consumers would use.
  Window-size query (`GetConsoleScreenBufferInfo`) likewise — `rusty_lines`
  already provides `terminal_size()` today by some means; confirm with that
  crate's own maintainers whether it already has a Windows arm before
  duplicating effort here.
- **`time`** — lowest priority, matching the handoff doc's own phasing;
  no rush call site depends on it (§3). Build it last, if at all, purely
  for `rusty_lines`/completeness rather than an open rush gap.
- **NOT needed, contrary to the handoff doc's first draft**: a general
  `arch`/`raw` FFI layer beyond what each of the above modules declares for
  itself. `winstdio.rs`'s existing pattern (a handful of `#[link(name =
  "kernel32")] extern "system"` declarations per concern, no shared FFI
  module) is the established convention in this codebase already — match
  it rather than introducing a new cross-cutting `arch` module.
- **`Win32Error`** — still needed as designed in the handoff doc
  (`GetLastError()` wrapper, `Result<T, Win32Error>` from every safe
  wrapper), used identically across all the modules above.

## 6. Explicitly out of scope for `rusty_win32` (confirmed by this inventory)

- `rlimit`/`ulimit`/`umask` (§3) — most resources have no Windows concept;
  `umask` has none at all. Don't build a `rlimit` module speculatively; if
  ever pursued, it's Job-Object memory/CPU limits
  (`JOBOBJECT_EXTENDED_LIMIT_INFORMATION`) for the narrow subset that maps,
  not a general `getrlimit`/`setrlimit` port.
- uid/gid/file-mode-bit queries (Theme 9/12) — Windows' SID/ACL model isn't
  a translation of POSIX uid/gid; synthesizing fake values would mislead
  rather than help. Leave as documented "always false/0/None."
  `GetFileInformationByHandle` for `test -ef`/`same_file` (dev+inode
  identity) is the one real, worthwhile exception, but it's a rush-side
  `std::os::windows::fs::MetadataExt` call, not a `rusty_win32` addition.
- `exec -a`/`exec -l` argv0 surgery (Theme 13) — no Win32 equivalent exists
  at all; a spawned process always reports its real command line. Permanent
  limitation, not a gap to close.
- ConPTY/raw-mode implementation itself — belongs in `rusty_lines`, not
  rush or `rusty_win32` directly consuming it on rush's behalf; `rusty_win32`
  provides the primitive, doesn't own the integration.

## 7. Effort estimate and phasing

Revises the handoff doc's proposed phasing using this inventory's priority
signal (§4's danger ordering, §5/§6's scope corrections):

| Phase | Scope | Why this order | Risk |
|---|---|---|---|
| 1 | `Win32Error` + `console::install_ctrl_handler` (§4.4) | Self-contained, no dependency on any other phase, closes the single most surprising gap (silent trap no-op) found in this inventory | low |
| 2 | `handle` (`DuplicateHandle`/`CreatePipe`/`SetHandleInformation`) | Unblocks rush's own fd-3+/varfd/coprocess follow-up work (§4.2); no Job Object dependency | low-medium |
| 3 | `process::spawn_suspended`/`resume` + `job` (Job Objects) | The actual blocker for `WINDOWS_JOB_CONTROL.md`'s staged plan; depends on nothing above except `Win32Error` | medium (well-scoped by the existing design doc) |
| 4 | `console` ConPTY/raw-mode primitives | For `rusty_lines`' benefit, not a rush gap this inventory found; coordinate with that crate before building blind | high (no interactive Windows verification in this environment, same constraint noted throughout) |
| 5 | `time` fast path | No known dependent; do last, matches the handoff doc's own reasoning | low |

Process substitution (§4.3) and the foreground-Ctrl-C-targeting question
(§4.5) are deliberately **not** in this phasing — both need a design pass
before any `rusty_win32` API surface can be proposed for them, the same way
`WINDOWS_JOB_CONTROL.md` scoped `coproc` and process substitution out of
its own plan rather than guessing at a shape.

## 8. Recommendation

Build `rusty_win32` against this document's §5 contract, in the §7 order.
Phase 1 (`console::install_ctrl_handler`) is the highest-value, lowest-risk
place to start: it requires no other `rusty_win32` module, has a
pre-existing, already-correct consumer on rush's side
(`trap::record_signal`'s atomic-store pattern, currently unreachable off
Unix only for lack of an installer), and fixes a bug (accept-but-never-fire)
rather than merely adding a feature. Phases 2-3 then unblock
`WINDOWS_JOB_CONTROL.md`'s already-designed background-job work, which
remains the largest scoped gap in rush's Windows parity. Coprocess and
process substitution (§4.3, and the coprocess case which needs the same
`/dev/fd`-equivalent answer) should stay explicitly deferred, tracked as
open design questions rather than backlog items with a shape assumed in
advance.
