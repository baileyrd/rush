# Changelog

All notable changes to **rush** are documented here. The format is loosely based
on [Keep a Changelog](https://keepachangelog.com/); the project predates a
tagged release, so everything lives under a single development heading.

## [Unreleased] — 0.1.0 (2026-06-16)

The shell grew from a foundation (REPL, pipelines, redirection, three builtins)
into a near-complete POSIX-style shell. Work is grouped by area below; see the
git history for the commit-by-commit narrative.

### Expansion
- **Variables** — `$VAR`, `${VAR}`; shell variables shadow the environment.
- **`${…}` operators** — `:-`/`-`, `:=`/`=`, `:+`/`+`, `:?`/`?`, and `${#name}`
  (length); the default/alternate word is itself expanded.
- **Special parameters** — `$?` (last exit status), `$0`–`$9`, `${10}`, `$#`,
  `$*`, and `$@` (with a standalone `"$@"` keeping each parameter separate).
- **Tilde** — `~` / `~/path` → `$HOME` (falls back to `$USERPROFILE`).
- **Command substitution** — `$(...)`, supporting operators and compounds inside.
- **Arithmetic** — `$((expr))`: `+ - * / %`, comparisons, `&& || !`, parentheses,
  and variables; `$`-references are expanded first (`$(( $1 + $2 ))`).
- **Globbing** — a hand-rolled matcher: `*`, `?`, `[…]` with ranges and `[!…]`,
  multi-component patterns (`src/*.rs`), and the POSIX leading-dot rule.
- **Word-splitting** — unquoted expansions split on whitespace; quotes suppress it.

### Grammar & control flow
- Recursive-descent parser producing a nestable AST.
- **Operators** — `&&`, `||`, `;`, and `&` (background), with exit-status
  short-circuiting.
- **Control flow** — `if`/`elif`/`else`/`fi`, `while`/`until`/`do`/`done`,
  `for … in … do … done`, `case … esac`, and `break`/`continue [n]`.
- **Functions** — `name() { … }` with recursion, own positional parameters, and
  `return [n]`; brace groups `{ …; }`.
- **Subshells** — `( … )` isolating the working directory and variables.
- **Comments** — `#` to end of line.
- **Multi-line input** — a `> ` continuation prompt; unfinished quotes, `$(`,
  `${`, and here-docs all keep reading.

### Redirection & I/O
- File redirection per fd: `<`, `>`, `>>`, `2>`, `2>>`.
- **fd duplication** — `2>&1` / `n>&m` (`> f 2>&1` sends both to one file).
- **Both streams** — `&>` / `&>>`.
- **Here-documents** — `<<EOF`, `<<-EOF` (tab-strip), `<<'EOF'` (no expansion).

### Builtins
- `cd`, `pwd`, `echo`, `export`, `unset`, `test` / `[ ]`, `true`, `false`, `:`,
  `break`, `continue`, `return`, `exit`.
- Unix job control: `jobs`, `fg`, `bg`, `kill [-SIG] %job|pid`.

### Job control (Unix)
- Background jobs (`&`), process groups, terminal hand-off (`tcsetpgrp`), and
  signal handling following the glibc reference.
- Ctrl-Z stop detection, `fg`/`bg` resume, and a job table reaped at each prompt.
- Gated behind `#[cfg(unix)]` (uses `libc`); other platforms run foreground-only.

### Execution modes
- Interactive REPL with line editing and persistent history (`~/.rush_history`).
- Script files: `rush script.sh args…` (sets `$0`, `$1`…).
- Command strings: `rush -c "cmds" [name args…]`.

### Tooling & docs
- GitHub Actions CI: build + test on Linux and Windows, plus clippy on Linux.
- `.gitattributes` normalizing line endings to LF.
- README feature matrix and `docs/ARCHITECTURE.md` kept current throughout.

### Notes & known limitations
- Compound commands can't yet be placed as one stage among several in a
  multi-command pipeline (e.g. `(cmd) | grep x`) — only a pipeline that is a
  single compound is supported today.

## [Unreleased] — since 0.1.1

### Packaging & release (G1–G4)
- MIT `LICENSE` and `license`/`description`/`repository` `Cargo.toml` metadata.
- Tagged releases build a Windows zip artifact (`.github/workflows/release.yml`).
- README status block: explicit "experimental" status, Windows foreground-only
  limitation stated in plain terms.
- `cd -` returns to (and prints) `$OLDPWD`, tracked on every successful `cd`.

### Real fd semantics (G10)
- `2>&1` combined with a pipe or command substitution now genuinely routes
  stderr through: `cmd 2>&1 | next` and `x=$(cmd 2>&1)` both capture the merged
  stream correctly. Fixed by materializing a real OS pipe by hand (Unix only)
  when a `Dup` redirect targets a sink `Stdio::piped()` can't share before
  spawn — see `exec.rs`'s `make_pipe`/`clone_or_materialize`.
- Subshells (`(...)`) now fork a real child on Unix instead of approximating
  isolation via state save/restore: `(cd x; …)`, `(VAR=…; …)`, and `exit`
  inside `(…)` are genuinely isolated and can't leak back to the parent shell.
  The old snapshot/restore approximation remains as the non-Unix fallback
  (still can't contain an `exit`).

### Builtin redirects (found during the G10 review)
- Redirects on a builtin (`echo hi > f`, `pwd 2>e`, `cd < f`, …) used to be
  silently ignored — builtins write via `println!`/`eprintln!` straight to
  the process's real stdio, bypassing the shell's fd resolution entirely.
  Fixed on Unix: the shell's own fd 0/1/2 are temporarily `dup2`'d to match
  before running the builtin, then restored (even if a redirect fails partway
  through). Off Unix, this remains a known limitation (no raw `dup2`
  equivalent). Only covers a builtin as the sole command of a pipeline; one in
  the middle of a multi-stage pipe (`echo hi | cd`) is unaffected — still the
  pre-existing punt (rush tries to exec it as an external program).

### Tab completion (G5)
- A custom rustyline `Helper` (`completion.rs`) replaces `DefaultEditor`. In
  command position (a rough, not lexer-accurate check — see the module doc),
  Tab completes builtin names and executables found scanning `$PATH`;
  elsewhere it defers to rustyline's own `FilenameCompleter` for files.

### Startup file (G6)
- Interactive sessions source `~/.rushrc`, if present, before the REPL loop
  starts — same as a script, so a var/function/alias set there takes effect.
  A missing or unreadable file is silently fine; an error inside it prints to
  stderr but doesn't stop the shell from starting.

### Prompt customization (G7)
- `$PS1` (shell variable or environment) replaces the hardcoded prompt when
  set, with a small escape set: `\w`/`\W` (cwd, cwd basename), `\u`/`\h` (user,
  host), `\$` (`#` for root, else `$`), `\?` (last exit status — a
  rush-specific extension, not a real bash escape), `\n`, `\\`. Falls back to
  the original `cwd $ ` when unset. Settable persistently via `~/.rushrc`.

### Aliases, `set -e`, `trap` (G8)
- **Aliases** — `alias name=value` / `alias` (list) / `alias name` (show) /
  `unalias name` / `unalias -a`. A single, non-recursive substitution at the
  start of a simple command, so `alias ls='ls --color=auto'` can't self-loop.
- **`set -e` / `set +e`** — errexit: a failing command exits the shell.
  Exempts `if`/`while`/`until` conditions (bash does too). A simplification of
  bash's finer "except a command that isn't positionally last in an `&&`/`||`
  list" rule — see `exec.rs`'s `exec_list_impl` doc comment. Naming any other
  `set` flag is an error, not a silently-ignored no-op.
- **`trap`** — `trap 'command' NAME` / `trap` (list) / `trap - NAME` (reset).
  Only `EXIT` (every exit path — the `exit` builtin, `errexit`, a forked
  subshell's own exit, and script/`-c`/interactive-Ctrl-D completion) and
  `INT` (Ctrl-C at an idle prompt only — a running foreground job is a child
  process under job control and never delivers `SIGINT` to the shell itself)
  are fired. Guarded against re-entrancy, so an `EXIT` trap that itself calls
  `exit` can't recurse forever.

### Test coverage for `exec.rs` and `job.rs` (G9)
- `exec.rs` — previously zero tests on the runtime core — is now covered
  black-box in `tests/exec_behavior.rs`, against the compiled binary rather
  than an in-crate module: pipeline wiring, redirection routing, exit-status
  propagation and short-circuiting, compound status, and two G10 regression
  locks (`2>&1` into a pipe, and a forked subshell's `exit` not killing the
  outer shell). The in-process alternative (`parser::parse` +
  `run_list`/`capture_list`) turned out to have real footguns: `capture_list`
  never tracks `$?` across jobs and rejects any compound command outright,
  and a builtin's redirects are wired up via a process-wide `dup2` that races
  across `cargo test`'s concurrent threads. A real subprocess per test
  sidesteps all of that.
- `job.rs` — also previously zero tests — gets an in-crate `#[cfg(test)]`
  module: `run_foreground`'s exit-status reporting (single command,
  multi-stage pipeline, signal death), and the job-table bookkeeping
  (`update_by_pid`/`notify_and_prune`) that backs `jobs`/`fg`/`bg`.
- Two narrower gaps `capture_list` surfaced along the way — it didn't track
  `$?` across jobs within a substitution, and it rejected *any* compound
  command, even a lone one — are now fixed; see below.

### `capture_list` fixes: `$?` tracking and capturing a compound (follow-up to G9)
- `$(false; echo $?)` now correctly sees `1` from *within* the substitution:
  `capture_pipeline` updates `$?` after every pipeline, mirroring
  `run_andor`. A plain assignment with no substitution (`x=5`) still resets
  `$?` to `0` rather than leaking a stale value from before it.
- `$(if ...)` / `$(while ...)` / `$( (...) )` — capturing a *sole* compound
  command — now works. It never went through `build_stage`/`Stdio` (only the
  multi-stage-pipeline case was documented as unsupported; a lone compound
  was silently rejected too, via the same hard error). Fixed by forking
  (Unix only) and redirecting the child's fd 1 to a pipe before running
  `run_compound` there, so everything the child writes — in-process
  (builtins) or via a further spawn that inherits its stdout — is captured.
- `x=$(false); echo $?` now correctly prints `1` (was `0`): a
  variable-assignment-only command takes the exit status of the last command
  substitution performed while expanding it, per POSIX, rather than always
  `0`. A new one-shot marker in `vars.rs`
  (`reset_last_subst_status`/`set_last_subst_status`/`take_last_subst_status`)
  — deliberately *not* the same thread-local as `$?` itself, since reusing
  `$?`'s slot as a sentinel would corrupt a direct `x=$?` read happening in
  the same expansion — carries the substitution's status from `capture_list`
  up to the assignment-only branch in `run_foreground`/`capture_pipeline`.
  Composes correctly with multiple assignments on one line (the last
  substitution wins), an assignment prefixed onto a real command (the
  command's own status counts, unaffected), and nested substitutions (each
  level sees its own last command's status, not an inner one's).

### Windows/MSYS2 build strategy (G11)
- Validated, not just documented: cross-compiled rush for
  `x86_64-pc-windows-gnu` with the same mingw-w64 toolchain MSYS2 packages —
  it builds and links into a genuine `PE32+` Windows executable, and
  `cargo tree` confirms rush's own `libc` dependency (and so `job.rs`) is
  excluded for that target. This corrects the gap's original framing: there
  is no "MSYS2 build with full job control" — `cfg(unix)`/`cfg(windows)`
  are decided by the target triple, not the build environment, and no
  Rust-supported Windows target sets `cfg(unix)`. Every Windows build is
  foreground-only, unconditionally, by construction — see `docs/
  ARCHITECTURE.md`'s `job.rs` section for the full writeup. Not validated:
  actually running the cross-compiled binary (no Windows machine in this
  environment, and a Wine install hit an unrelated package error) —
  unnecessary for the conclusion above, since it's decided by what compiles
  in, not by anything only observable at runtime.

### Prefix/suffix parameter expansion: `${v#pat}` `${v##pat}` `${v%pat}` `${v%%pat}` (C1)
- `#`/`%` remove the shortest matching prefix/suffix; `##`/`%%` remove the
  longest. The operand is a glob pattern — the same matcher `case` patterns
  already use — matched by trying candidate cut points (shortest-first or
  longest-first) and taking the first one that fully matches. No colon form,
  matching bash (which doesn't define one for this family either).

### `for name; do` (no `in`) iterates `"$@"` (C2)
- Per POSIX: omitting the `in` clause now iterates the positional parameters,
  as if `in "$@"` had been written, instead of silently running the loop body
  zero times. Distinct from an *explicit* `in` with no words (`for x in; do
  ...`), which is still a real empty list — the parser records whether `in`
  was present at all (`Compound::For`'s new `has_in` field).
