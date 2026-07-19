# Async background-job notification (`set -b`)

A design doc, scoping (and, for stage 1, now implementing) what it takes to
print a background job's completion (`[1]+  Done    sleep 5`) the moment it
finishes, even while the shell is sitting idle at the prompt with a
partially-typed line, instead of only at the next prompt boundary the way
both `job.rs` (Unix) and `winjob.rs` (Windows) work today. Written after a
conversational back-and-forth floated this as a follow-up to the Windows
job-control work and an earlier off-the-cuff answer in that conversation
claimed it would need a background thread (Unix: a real `SIGCHLD` handler
interrupting a blocking `read()`; Windows: a second thread or overlapped
I/O, since Windows has no "a signal interrupts a blocking syscall"
primitive). **That answer was wrong by omission, not merely imprecise, once
the actual line-editor code was read** — see "Starting point" below.
Correcting it here rather than quietly is why this doc exists at all.

**Status: stage 1 is done and merged** (`rusty_lines`'s `prepare_external_output()`
— [PR #28](https://github.com/baileyrd/rusty_lines/pull/28)); stage 2 (`rush`'s
own wiring, behind `set -b`) is not started. See "Suggested staging" below.

**Scope decision:** gate the new behavior behind `set -b`/`set -o notify`
(bash's own real option for exactly this — see below), leaving the default
prompt-boundary-only behavior unchanged. Not a redesign of *when* a job is
considered done, only *when the shell tells you*.

## Starting point: what already exists

Two facts, found by reading rather than assumed, make this far smaller than
the earlier conversational answer suggested:

1. **`rusty_lines` already ticks every 200ms while idle at the prompt**, not
   just once per blocking read. `read_line_raw`'s wait loop
   (`rusty_lines/src/lib.rs:2153-2196`) polls `input_ready(200)`, and on
   every timeout — before looping back to poll again — calls
   `hooks.on_interrupted_read()` (`lib.rs:2169`). Today rush's own hook
   (`ShellHooks::on_interrupted_read`, `rush/src/main.rs:52-57`) uses this
   tick to fire deferred `TERM`/`HUP` traps (`trap::check_pending()`,
   Unix-only). The same tick already self-heals a terminal left in cooked
   mode by an external `stty` and repaints on a detected resize
   (`lib.rs:2170-2196`) — i.e. "notice something changed while idle, print
   or repaint, then keep editing where the user left off" is not a new
   capability being proposed here, it is the *existing, tested* purpose of
   this tick. This works identically on both platforms already:
   `input_ready`'s poll (`rusty_lines/src/term_sys.rs`) has a libc/rusty-libc
   backend using `poll(2)` on fd 0 and a Windows backend
   (`term_sys.rs:409-413`) that already calls
   `rusty_win32::console::wait_readable` (a `WaitForSingleObject` wrapper).
   No signal handler and no second thread does any of this — it's a plain
   bounded-wait poll, called from ordinary (non-signal-handler) function
   context on every platform.
2. **`reap_background` is already cheap, non-blocking, and safe to call
   from anywhere**, not just the top of the main loop. Unix's
   `job::reap_background` (`job.rs:448-464`) is a `waitpid(-1, WNOHANG |
   WUNTRACED | WCONTINUED)` loop — never blocks, no signal-handler-safety
   constraints since it's called from plain function context, not an actual
   `SIGCHLD` handler. Windows' `winjob::reap_background`
   (`winjob.rs:487-488`) is `refresh_all()`, a zero-timeout
   `GetExitCodeProcess`-style poll over each tracked job's handle. Both are
   already called once per prompt (`main.rs:643-648`) specifically *because*
   they're cheap enough to call unconditionally on every loop iteration —
   calling them again every 200ms while idle is the same kind of call, just
   more often, not a different mechanism.
3. **`SIGCHLD` is not currently caught by any handler on Unix** — `job.rs`'s
   `JOB_SIGNALS` (`job.rs:67-73`) is `SIGINT, SIGQUIT, SIGTSTP, SIGTTIN,
   SIGTTOU` only. This turns out not to matter: since (1) already delivers a
   ≤200ms-latency wake-up regardless of signals, there is no need to add a
   `SIGCHLD` handler at all — `trap::record_signal`'s atomic-store-then-poll
   pattern (`trap.rs:36-41`), the natural template for one, isn't needed
   here. (If it ever were needed for some other reason, `trap.rs` already
   has the template — but this design doesn't need it.)
4. **`set -b`/`set -o notify` is already parsed and already inert.**
   `builtins.rs`'s `set` handling explicitly lists `'b'` among the flags
   "accepted but inert (C107)" (`builtins.rs:4462-4464`) and `"notify"`
   among the same for `-o` (`builtins.rs:4493`) — parsed so a script using
   it doesn't hard-error, but backed by no state and no behavior. This is
   bash's own real option for exactly this feature ("report the status of
   terminated background jobs immediately, rather than waiting until just
   before printing the next prompt" — bash's own wording), not an invented
   opt-in switch. Giving it real teeth is this design's actual deliverable,
   not a new flag layered on top.

## Why the idle tick, not threads/signals/IOCP

Given (1)-(3) above, the earlier conversational framing — "needs a
background thread on Unix via `SIGCHLD` + `EINTR`, and Windows has no
signal-interrupts-a-blocking-syscall primitive so it'd need a second thread
or overlapped I/O" — doesn't hold up. Neither backend needs to interrupt a
blocking syscall at all, because neither backend is actually blocked in an
uninterruptible syscall for more than 200ms at a time even today: the "idle
wait" is already a bounded poll loop, not a single indefinite blocking read.
The whole reason true async delivery looked hard was solved already, for an
unrelated reason (resize detection, raw-mode self-healing), before this
question was ever asked.

This also closes the loop on a separate, earlier question in the same
conversation: whether Windows job-tracking should move from polling to I/O
completion ports. It shouldn't, and this design doesn't change that answer —
`winjob::reap_background`'s poll is called at most every 200ms from a normal
prompt-adjacent tick, not in a hot loop; IOCP would add real complexity
(associating every per-pipeline Job Object with one shared completion port,
choosing completion keys, draining `GetQueuedCompletionStatus`) to speed up
something that was never the bottleneck.

The one real cost of the idle-tick approach: latency is bounded by the tick
interval (≤200ms), not truly instantaneous the way a real interrupt would
be. 200ms is imperceptible for this purpose (a job finishing while you're
looking at an idle prompt) and is already the interval the resize/raw-mode
self-heal logic accepts for the same reason — not a new number invented for
this doc.

## Shape of the change

Two repos, two PRs, landed in this order (the second depends on the first
actually existing on `rusty_lines`' published `main`, matching how every
`rusty_win32` primitive earlier in this project's job-control work landed
before the `rush`-side consumer that used it).

### 1. `rusty_lines`: a new `prepare_external_output()`, not a `Hooks` signature change — **done**

Landed as [PR #28](https://github.com/baileyrd/rusty_lines/pull/28), verified
on real CI (`prepare_external_output_prints_cleanly_while_idle_and_keeps_the_line`,
a new pty test proving the notice lands on its own line and an in-progress
buffer survives intact around it) and published on `rusty_lines`' `main`.
`rush` hasn't bumped its pin or consumed this yet — that's stage 2, still
open.

**Revised from this doc's first pass** (below), after actually tracing
`render()`'s own invariant rather than guessing at the API shape: a bare
`bool` returned from `on_interrupted_read` — "I printed, please clean up
after me" — is the wrong shape, not just an unresolved detail. `render()`
(`lib.rs:4319`) unconditionally opens with `\r` then, if
`st.painted_cursor_row > 0`, `ESC[{n}A` to move *up* that many rows
(`lib.rs:4327-4330`) — it assumes the cursor is sitting exactly where the
*previous* `render()` call left it. Anything that writes to the terminal in
between without also resetting `painted_rows`/`painted_cursor_row`/
`fresh_region` first (the existing self-heal branch does this at
`lib.rs:2181-2183`, *before* its own `writeln!`) makes the next `render()`
move up the wrong number of rows against content that's no longer there —
this is what "please repaint" arriving *after* an interleaved `eprintln!`
would actually produce: corrupted movement, not just stale content. The
reset has to happen **before** the host's own print, not signaled after it.

That rules out a `Hooks` signature change entirely, and turns out to need
only a small, freestanding, opt-in primitive instead — a real improvement
on the first pass, not merely a fix:

```rust
/// Interrupt the current prompt/buffer's on-screen paint for output a
/// host wants to print outside the editor's own rendering (e.g. a
/// background job's completion notice printed while idle at the
/// prompt): moves to a fresh line now and marks the region for a full
/// repaint at the next safe point (the end of the current
/// `Hooks::on_interrupted_read` call, or the next one reached, for a
/// caller nested deeper in the read path). Call this *before* printing,
/// every time, even for the first of several notices in a row — cheap
/// and idempotent if the flag's already set. A no-op outside the raw-mode
/// interactive editor (the piped-stdin path has no on-screen paint to
/// protect in the first place).
pub fn prepare_external_output() -> io::Result<()> { .. }
```

Backed by a `thread_local! { static EXTERNAL_OUTPUT_PENDING: Cell<bool> }`
`writeln!`s immediately (matching the self-heal branch's own ordering) and
sets the flag; every call site that already invokes
`hooks.on_interrupted_read()` (`lib.rs:608, 1320, 2169, 2521` — the raw
tick, the piped-stdin EINTR path, `read_byte`'s own EINTR handling, and
`wait_for_key`) checks the flag immediately after and, wherever `st: &mut
LineState` is actually in scope at that point (the tick at `lib.rs:2169`
and `wait_for_key` at `lib.rs:2521` both have it; `read_byte`/
`read_line_plain` don't), performs the same reset-then-`render()` the
self-heal branch already does, reusing that exact code path rather than
adding a new one. A call site without `st` in scope just leaves the flag
set for the next one that has it to pick up — at most one key-read cycle
of deferral, never lost, never wrong. In practice this project's own use
(job notification, wired through the *outer* 200ms tick, deliberately not
a real signal handler — see "Why the idle tick" above) always hits the
tick call site directly, so the deferred case doesn't come up for it.

No `Hooks` trait change at all, so this is fully additive — every existing
`Hooks` impl (including `rush`'s own `NoHooks`/other consumers, if any)
keeps compiling untouched.

### 2. `rush`: wire `reap_background` into the hook, behind `set -b`

`job::notify_and_prune`/`winjob::reap_background`'s printing needs to move
*before* a caller can call `prepare_external_output()` at the right
moment — the ordering problem is symmetric with (1) above: the hook doesn't
know whether anything will be printed until reaping has already happened,
by which point it's too late to prepare first. Fix: keep `job.rs`/
`winjob.rs` themselves unaware `rusty_lines` exists at all (they're already
usable without an `Editor` — script/`-c` mode never constructs one, and
these modules shouldn't need to start caring); have them return *what* to
report instead of printing it directly, and let `main.rs` — which already
owns the `rusty_lines` dependency via `ShellHooks` — decide when to prepare
the terminal:

```rust
// job.rs / winjob.rs: reap_background() keeps its exact existing
// behavior (still prints, still called unconditionally every prompt);
// a new sibling exposes the same detection without printing, for the
// hook path to use instead.
pub fn reap_background_notices() -> Vec<String> { .. } // "[1]+  Done\tsleep 5", one per line

// main.rs
fn on_interrupted_read(&self) {
    #[cfg(unix)]
    trap::check_pending();
    if vars::notify() {
        #[cfg(unix)]
        let notices = job::reap_background_notices();
        #[cfg(not(unix))]
        let notices = winjob::reap_background_notices();
        if !notices.is_empty() {
            let _ = rusty_lines::prepare_external_output();
            for n in notices {
                eprintln!("{n}");
            }
        }
    }
}
```

Guarded by a new `vars::notify()` accessor, matching the existing
one-function-per-option pattern (`vars::errexit()`, `vars::nounset()`,
`vars::xtrace()`, etc. at `vars.rs:237-300`) backed by real state instead
of `set -b`'s current no-op parse. `reap_background`'s existing per-prompt
call site (`main.rs:643-648`) stays exactly as-is regardless — still
correct for `set -b` *off* (the default), and even *on* it remains a
harmless safety net for jobs that finish in the brief window between a
submitted line returning and the next idle tick starting.

## Deliberately out of scope

- **Notification while a foreground command is running.** The idle tick
  only exists inside `read_line_raw`'s wait loop — there's no equivalent
  tick while the shell is blocked on a foreground child via `wait(2)`
  (Unix) or `WaitForSingleObject` (Windows) outside the editor entirely.
  Real bash defers those too; matching that default is free, not a gap.
- **Ctrl-Z stop/continue notifications' timing on Unix.** `job.rs`'s
  `notify_and_prune` already reports `Stopped`/`Running` transitions the
  same way it reports `Done` — this design doesn't change *what* gets
  reported, only *when*, so those ride along with `Done` under the same
  `set -b` gate for free. (Windows has no Stopped state at all — see
  `docs/WINDOWS_JOB_CONTROL.md`'s own note on `jobs -s` — so this is
  Unix-only in practice regardless.)
- **A `rush`-side pty test scenario proving the notification actually
  appears while idle, end to end.** `rusty_lines`' own pty test (stage 1,
  done) proves the *repaint* is clean; it doesn't and shouldn't prove
  `rush`'s specific notice text/`set -b` gating, since that's `rush`'s
  concern, not the editor's. `tests/pty/editor_pty_test.py` is where that
  belongs, once stage 2 lands — not attempted by this doc.
- **Any change to *what* counts as job-control-enabled.** Both
  `job::job_control_enabled()` and Windows's `vars::interactive()` gating
  already restrict this correctly — `on_interrupted_read` only fires from
  the interactive `read_line_timeout` path in the first place (scripts and
  `-c` never construct an `Editor`), so no extra gating is needed beyond
  the new `vars::notify()` check itself.

## Risk

- **Cross-repo sequencing.** This is a `rusty_lines` API change consumed by
  `rush` — the same "verify independently, then bump the pin" pattern every
  `rusty_win32` primitive in the Windows job-control work followed, just
  with a different upstream crate. `rush`'s own CI can't validate the
  `rusty_lines` half at all until that PR merges and the pin bumps.
  Get stage 1 reviewed and merged as a self-contained, testable change in
  `rusty_lines` (its own repaint-contract test, ideally pty-based) *before*
  writing a single line of the `rush`-side wiring — matching this whole
  project's established discipline of small, independently-verifiable
  increments rather than one cross-repo patch nothing can review in
  isolation.
- **Getting the repaint contract wrong is a display-corruption bug, not a
  compile error** — exactly the class of bug this project has repeatedly
  found only real CI (or, here, a real pty session) catches, not `cargo
  check`. Budget for it to take more than one round, the way the Windows
  pipeline stdio-inheritance bug and the `disown` ambient-job caveat both
  did earlier in this project's job-control work.
- **Verifying "notification happened *while idle*, not just eventually"**
  needs a timing-sensitive pty test (type nothing, background a job, assert
  the notice appears without pressing a key) — a harder thing to assert
  reliably than this project's existing `windows_job_control.rs`/`job.rs`
  tests, which all drive a complete, non-interactive `rush -c` script and
  check final output. This is squarely `tests/pty/editor_pty_test.py`'s
  shape of problem, not `exec_behavior.rs`'s.

## Suggested staging

1. **Done.** `rusty_lines`: `prepare_external_output()` and the
   `EXTERNAL_OUTPUT_PENDING` thread-local, checked at each of the four
   `on_interrupted_read` call sites, reusing the existing self-heal
   reset-then-`render()` sequence wherever `st` is in scope. No `Hooks`
   trait change, so fully additive. A pty-based test
   (`prepare_external_output_prints_cleanly_while_idle_and_keeps_the_line`)
   proves a hook that calls it before printing ends up with a clean
   repaint afterward, not corrupted movement — verified on real CI before
   merging. Landed and published independently of `rush`
   ([PR #28](https://github.com/baileyrd/rusty_lines/pull/28)).
2. **Not started.** `rush`: bump the `rusty_lines` pin; add `vars::notify()` backed by
   real `set -b`/`set -o notify` state (replacing the current inert parse);
   add `job::reap_background_notices()`/`winjob::reap_background_notices()`
   (detection only, no printing) alongside the existing, unchanged
   `reap_background()`; wire the notices path into
   `ShellHooks::on_interrupted_read`, gated on `vars::notify()`, calling
   `prepare_external_output()` once before printing any notice. Add an
   integration test — likely a new pty scenario, not a `rush -c` one, given
   the "while idle" requirement above.
3. Only then: decide whether the default should change (bash itself
   defaults `-b` off; no reason found here to diverge from that default).
