# Async background-job notification (`set -b`)

A design doc, not yet implemented — scoping what it would take to print a
background job's completion (`[1]+  Done    sleep 5`) the moment it finishes,
even while the shell is sitting idle at the prompt with a partially-typed
line, instead of only at the next prompt boundary the way both `job.rs`
(Unix) and `winjob.rs` (Windows) work today. Written after a conversational
back-and-forth floated this as a follow-up to the Windows job-control work
and an earlier off-the-cuff answer in that conversation claimed it would need
a background thread (Unix: a real `SIGCHLD` handler interrupting a blocking
`read()`; Windows: a second thread or overlapped I/O, since Windows has no
"a signal interrupts a blocking syscall" primitive). **That answer was wrong
by omission, not merely imprecise, once the actual line-editor code was
read** — see "Starting point" below. Correcting it here rather than quietly
is why this doc exists at all.

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

### 1. `rusty_lines`: let a hook signal "I printed something, repaint around it"

`Hooks::on_interrupted_read` currently returns `()`
(`rusty_lines/src/lib.rs:138`). The minimal change: return a `bool` (`true`
= "I wrote to stdout/stderr outside the editor's own rendering, please
repaint"), default `false` for source compatibility with every existing
`Hooks` impl that doesn't care. The idle-wait loop
(`lib.rs:2153-2196`) already has an unconditional-detection branch that does
exactly the repaint dance needed (`writeln!(io::stdout())?;
render(&mut st, history)?;` at `lib.rs:2184-2185`, currently gated on "raw
mode got clobbered" or "columns changed") — extending that branch's
condition to also trigger on the hook's return value reuses the existing,
already-tested code path rather than adding a new one.

Open question this doc doesn't resolve, left for whoever picks up that PR:
whether the host needs to move the cursor to a fresh line *before* printing
(so the notice doesn't land mid-way through the currently-rendered
prompt+buffer) via a new method the hook can call first (something like
`Editor`/a passed context exposing `prepare_external_output()`, doing what
the self-heal branch's `writeln!` already does), or whether it's acceptable
for the host to print first and rely on the post-hook repaint to clean up
any visual mess — the existing self-heal branch gets away with the latter
only because raw-mode breakage and resize don't themselves print anything
mid-screen the way an interleaved `eprintln!` would. This needs an actual
prototype against a real terminal (or the pty harness — see "Risk" below)
to settle, not a guess in a design doc.

### 2. `rush`: wire `reap_background` into the hook, behind `set -b`

`ShellHooks::on_interrupted_read` (`main.rs:52-57`) gains, guarded by a new
`vars::notify()` accessor (matching the existing one-function-per-option
pattern — `vars::errexit()`, `vars::nounset()`, `vars::xtrace()`, etc. at
`vars.rs:237-300` — backed by real state instead of `set -b`'s current
no-op parse):

```rust
fn on_interrupted_read(&self) -> bool {
    #[cfg(unix)]
    trap::check_pending();
    if vars::notify() {
        #[cfg(unix)]
        return job::reap_background_now(); // returns true if it printed anything
        #[cfg(not(unix))]
        return winjob::reap_background_now();
    }
    false
}
```

`reap_background`'s existing per-prompt call site (`main.rs:643-648`) stays
exactly as-is regardless — it's still the correct behavior for `set -b`
*off* (the default), and even with it *on* it remains a harmless safety net
for the brief window between a submitted line returning and the next idle
tick starting. `job::reap_background`/`winjob::reap_background` likely need
a `_now`-style variant (or an added return value on the existing function)
that reports whether it actually printed a notice, to answer the `bool` the
new `on_interrupted_read` contract requires — a small, mechanical change to
each, not a new mechanism.

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
- **A `rusty_lines` pty test scenario proving the repaint is actually
  clean** (not just that the hook fires). `tests/pty/editor_pty_test.py`
  is `rush`'s own harness, but the repaint contract itself belongs to
  `rusty_lines` and should be proven there, with its own scenario, before
  `rush` ever wires a real notification through it — otherwise a broken
  repaint only surfaces as "the Windows/Linux CI screen looks wrong" noise
  in `rush`'s own end-to-end suite, several layers away from the actual
  bug.
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

1. **`rusty_lines`:** change `Hooks::on_interrupted_read` to return `bool`;
   extend the existing self-heal repaint branch to also trigger on it; add
   a pty-based (or equivalent real-terminal) test proving a hook that
   prints mid-idle-tick ends up with a clean repaint afterward, not
   corrupted output. Land and publish independently of `rush`.
2. **`rush`:** bump the `rusty_lines` pin; add `vars::notify()` backed by
   real `set -b`/`set -o notify` state (replacing the current inert parse);
   wire `job::reap_background`/`winjob::reap_background` (each gaining a
   "did I print anything" return value) into `ShellHooks::on_interrupted_read`,
   gated on `vars::notify()`. Add an integration test — likely a new pty
   scenario, not a `rush -c` one, given the "while idle" requirement above.
3. Only then: decide whether the default should change (bash itself
   defaults `-b` off; no reason found here to diverge from that default).
