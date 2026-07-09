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

### Compound command as one stage of a real pipeline (C3)
- `(cmd) | grep x`, `if ...; fi | wc -l`, a compound in the middle of a
  3-stage pipeline — all now work, for the interactive/script job-control
  path (`job::spawn_pipeline`, Unix only). `Pipeline.commands` is now
  `Vec<Stage>` (`Stage::Simple` or `Stage::Compound`) instead of
  `Vec<Command>`; a compound stage forks (`spawn_compound_stage`), wiring
  stdin/stdout via `dup2` from real fds (`File`, not `Stdio` — a forked
  child needs something introspectable to `dup2` from) and joining the
  pipeline's process group like any exec'd stage. Forked-subshell isolation
  (G10) verified to still hold even when the subshell is a pipeline stage,
  not just the whole pipeline.
- Not extended to the capture path (`$(...)`): a compound as one stage among
  several *inside* a substitution, or on non-Unix (no `fork` there at all),
  still errors clearly — a narrower, separate remaining limitation.

### `set -e` matches bash's positionally-last rule, not "whichever pipeline ran last" (C4)
- A failing pipeline is now exempt from errexit unless it's positionally last
  in its `&&`/`||` list — `set -e; false && true` survives (`false` isn't
  last), `set -e; true && false` exits (`false` is), matching real bash.
  `run_andor`/`run_job`/`exec_list_impl` (`exec.rs`) now report whether the
  textually-last pipeline in a job's and-or chain actually ran (`last_ran`),
  so short-circuiting an earlier failure no longer trips errexit. `if`/`while`
  conditions remain separately exempt via the pre-existing `exec_cond` path.

### Real `$IFS`-driven word-splitting (C5)
- Field splitting of an unquoted expansion now honors `$IFS` instead of a
  hardcoded whitespace set. Unset `IFS` still defaults to space/tab/newline;
  an explicit empty `IFS=` disables splitting entirely (the whole expansion
  is one field); any other value splits on exactly its characters —
  space/tab/newline within it collapse like the default (no empty fields
  from a run), while every other character is a "non-whitespace" delimiter
  where each occurrence opens a field on its own, even empty (`IFS=,` on
  `a,,b` is three fields, not two) — except a single trailing one at the
  very end, which produces no trailing empty field, matching a real
  asymmetry in bash's own behavior. New `Ifs` type and rewritten `Splitter`
  in `expand.rs`. `$*`/`${*}` now join positional parameters with `$IFS`'s
  first character (space if unset, nothing if IFS is empty) instead of a
  hardcoded space; `$@` is unaffected, matching bash.

### `test`/`[` logical combinators `-a` / `-o` (C6)
- `test`/`[` now understand `EXPR1 -a EXPR2` (AND) and `EXPR1 -o EXPR2` (OR),
  with `-a` binding tighter than `-o` and `!` negating only the next
  expression rather than a whole trailing `-a`/`-o` chain — both verified to
  match real bash exactly. `test_eval` (`builtins.rs`) is now a small
  recursive-descent parser (`test_or` → `test_and` → `test_not` →
  `test_primary`) instead of a fixed-arity match; all prior single-expression
  forms are unaffected.

This closes out **Tier I** (correctness/POSIX risk) — see
`docs/CAPABILITY_GAPS.md` — entirely: C1 through C6 are all done.

### `read` builtin, and redirects trailing a compound command's close (C7)
- `read [-r] [name...]` (`builtins.rs`) reads one logical line directly off
  fd 0 a byte at a time (never over-consuming past the newline, so a loop of
  calls sharing one fd — `while read line; do …; done < file` — picks up
  exactly where the last call left off) and splits it into fields on `$IFS`,
  using the same whitespace/non-whitespace classification and
  trailing-delimiter asymmetry word-splitting uses (C5). A name past the
  last field gets `""`; the *last* name absorbs any extra fields verbatim
  (original separators intact), not re-split. Without `-r`, `\<newline>` is a
  line continuation and `\<char>` escapes a separator; `-r` disables both.
  Exit status is 0 for a newline-terminated line, 1 on EOF (even if a
  trailing unterminated partial line was still read and assigned) — all
  verified against real bash directly.
- Landing `read` exposed a real, separate, pre-existing gap it needed to be
  useful for its headline idiom: a redirect trailing a compound command's
  close (`while …; done < file`, `{ …; } > log`) was silently dropped by the
  parser — the tokens just became a stray no-op command afterward, so `done
  < file` never wired the file to fd 0 at all (a lone `while read …` with no
  pipe would silently read the shell's real stdin instead of the file — a
  hang in a script, not an error). Fixed: the parser now attaches trailing
  redirects to the compound itself (new `RawCompound`/`exec::CompoundStage`,
  alongside a here-doc body, mirroring `Command`'s own `redirects`/`heredoc`
  split), applied for the compound's whole duration via the same
  `redirect_stdio` (renamed from `redirect_builtin_stdio`, since it's no
  longer builtin-only) a lone builtin already used — including a compound as
  one stage of a real pipeline (`job::spawn_compound_stage`) and a compound
  captured via `$(...)` (`capture_compound`), with the same "explicit
  redirect overrides implicit pipe/capture wiring" precedence `build_stage`
  already uses for simple commands.
- A here-doc trailing a compound's close (`while …; done <<EOF`) works the
  same way, fed through a `CLOEXEC`-marked pipe (`set_cloexec`) from a
  background thread — the fix for a real deadlock found while testing this:
  without `CLOEXEC`, a real child spawned from the compound's body before
  the writer thread finished would inherit its own copy of the write end via
  fork/exec, so the reader never saw EOF.

### `printf` builtin (C8)
- `printf FORMAT [args...]` (`builtins.rs`'s `printf_cmd` and `printf`
  submodule) — the portable, correct way to emit formatted output, unlike
  `echo`, whose formatting is whatever the platform's convention happens to
  be (rush's own `echo` has no `-e` at all). Supports `%s`/`%b` (string,
  `%b` also processing backslash escapes in its argument),
  `%d`/`%i`/`%o`/`%u`/`%x`/`%X` (integer, decimal/octal/unsigned/hex — a
  negative number reinterpreted as unsigned, matching real `printf`'s two's
  complement behavior), `%c`, `%%`, the `-`/`0`/`+`/` ` flags, and a width
  and/or `.precision`. Format-string escapes (`\n`/`\t`/`\\`/`\a`/`\b`/`\f`/
  `\r`/`\v`/`\NNN` octal) are resolved once, up front. If there are more
  arguments than the format consumes, the whole format repeats against the
  rest (`printf "%s-%d\n" a 1 b 2 c` → `a-1`, `b-2`, `c-0`), matching real
  bash exactly; missing arguments mid-format default to `""`/`0` rather than
  erroring. Not yet implemented: `%e`/`%f`/`%g` (floating point, lower-value
  here since rush's arithmetic is integer-only) and `*` (width/precision
  taken from an argument).

### `shift [n]` builtin (C9)
- The missing piece connecting positional parameters and `case` (both
  already supported) into the ubiquitous `while [ $# -gt 0 ]; do case $1 in
  …; esac; shift; done` argument-parsing loop. `vars::shift` drops the first
  `n` (default 1) positional parameters; `builtins::shift_cmd` wires up its
  exit status: a negative or non-numeric `n` is a hard usage error (status 1
  with a message), but `n` greater than `$#` fails *silently* (status 1, no
  message) — a real bash quirk verified directly, since running past the
  end this way is the everyday way an argument-parsing loop notices it's
  done.

### `local` builtin — function-scoped variables (C10)
- Every rush function used to share the caller's entire variable namespace,
  so a function's own `i=0` permanently clobbered the caller's `i`. Fixed:
  each function call now gets a stack frame (`vars::push_local_frame`/
  `pop_local_frame`, wired into `exec::call_function`) recording, for every
  name `local` shadows in that call, whatever the name was before (or its
  absence) — restored automatically when the call returns. Nesting falls
  out for free: an inner call's own `local x` shadows further and restores
  to the *enclosing* call's local value on return, not the top-level one
  (verified against real bash directly). A bare `local x` (no `=value`)
  leaves `x` genuinely unset within the function — `${x-default}` inside it
  sees it as unset, not merely set to `""` — matching bash exactly. `local`
  outside any function call is a usage error and doesn't fall through to
  setting a plain global variable.

### `getopts` builtin (C11)
- `getopts optstring name [arg...]` (`builtins::getopts_cmd`) — the
  portable way to parse `-a`, `-b value`, and combined short flags (`-ab`
  means `-a -b`). `$OPTIND` (1-based index of the next word) stays put
  while still inside a combined-flag word, advancing only once it's
  exhausted — tracked via an internal `(optind, char_pos)` cursor
  (`vars::getopts_char_pos`/`set_getopts_char_pos`), mirroring bash's own
  private state rather than exposing an extra variable. A leading `:` in
  `optstring` enables silent mode (`name` set to `?`/`:` with `$OPTARG` the
  offending character, no diagnostic) instead of the default (a diagnostic,
  `name` set to `?`, `$OPTARG` unset). `$OPTIND`/`$OPTARG` are ordinary
  shell variables — resetting `OPTIND=1` starts a fresh pass. A lone `--`
  or the first non-option word ends option processing without being
  consumed. This and `shift` (C9) together unlock the standard `while
  getopts ...; do case $opt in ...; esac; done; shift $((OPTIND-1))`
  argument-parsing idiom, verified end-to-end against real bash.

### `command` / `type` / `hash` builtins (C12)
- `command -v`/`-V name...` (`builtins::command_cmd`/`command_v`, shared
  `Kind` classifier) describes how each name would resolve — alias,
  function, builtin, or `$PATH` executable, in that precedence order
  (`-v`: terse, the standard existence-check form used constantly in
  install scripts; `-V`: a human-readable sentence) — without running
  anything, failing if none resolve. `type` (`type_cmd`) shares the same
  classifier, additionally recognizing shell keywords, and has a `-t` form
  for just the one-word classification.
- Plain `command name [args...]` (no `-v`/`-V`) actually *runs* `name`,
  bypassing a shadowing shell function of the same name — the headline
  reason `command` exists. Handled at the exec dispatch level
  (`exec::command_bypass`, wired into `run_foreground`) rather than purely
  inside the builtin, so it composes with real redirects and external
  spawns exactly like an ordinary simple command would.
- `hash` (`hash_cmd`) is a genuine stub: rush never caches `$PATH` lookups
  (every spawn just searches fresh), so there's nothing to actually hash.
  `-r` and a bare call are accepted no-ops; `hash name` at least reports via
  exit status whether it currently resolves.
- A function's own reconstructed source (as bash prints after "is a
  function") isn't reproduced by either `command -V` or `type` — a
  documented narrowing, since rush functions store a parsed `CommandList`,
  not original source text. All other cases verified against real bash
  directly.

### `wait` builtin, and `$!` (C13)
- `wait [pid|%job ...]` (`job::wait_cmd`/`wait_all`/`wait_job_pgid`/
  `wait_one`): with no operands, blocks until every job this shell knows
  isn't finished has finished (always succeeding, POSIX's rule); with one
  or more `pid`/`%job` operands, blocks on each in turn and reports the
  *last* one's own exit status. A pid/job already reaped — by an earlier
  `wait`, by `fg`, or by the interactive prompt's own background polling —
  still reports its remembered status rather than erroring, via a new
  `REAPED: HashMap<pid_t, i32>` that `update_by_pid` populates whenever a
  tracked pid actually exits (a real bash quirk verified directly: waiting
  twice on the same pid still works).
- Landing this exposed that `$!` (the most recently backgrounded job's pid)
  was entirely unimplemented — a real prerequisite, since `p=$!; wait $p`
  is the standard way to capture a specific background job to wait on
  later. Added (`vars::last_bg_pid`/`set_last_bg_pid`, wired into
  `job::run_background` and `expand.rs`'s `$`-scanner): `$!` is the *last*
  stage's own pid (not the pgid) for a piped background job, matching bash
  exactly; unset until something's been backgrounded.
- Also fixed along the way: `run_background`'s `[id] pgid` announcement
  used to print unconditionally, but real bash only shows it interactively
  — a non-interactive script now prints nothing there either, gated on the
  same `job_control_enabled` flag that already tracked exactly this
  distinction.
- Found but out of scope here, and not specific to `$!`: backslash-escaping
  a `$` inside double quotes (`"\$?"`, `"\$FOO"`) doesn't produce a literal
  `$` the way POSIX requires — the backslash is dropped and the parameter
  still expands. Tracked separately as C35 in `docs/CAPABILITY_GAPS.md`.

### `.` / `source` builtin (C14)
- `exec::source_file`, wired to both `.` and `source` (exact synonyms, one
  `source_cmd` builtin): runs a file's commands in the *current* shell
  environment — no fork, no new variable scope. A bare filename (no `/`) is
  searched on `$PATH` for a *readable* file (checked via `is_file()`, not
  the execute bit — sourcing works on a file lacking `+x`, unlike running it
  directly); a name containing `/` is used as a literal path.
- Positional-parameter handling matches bash exactly: with no extra
  arguments, the caller's own `$1`… show through unchanged inside the
  sourced file; with extra arguments, they temporarily replace the
  caller's, restored after the file finishes.
- `return` inside the sourced file ends only the sourcing — the calling
  context keeps running; `break`/`continue` are *not* consumed and
  propagate transparently to an enclosing loop back in the caller, both
  verified directly against real bash.
- Found and fixed along the way: `resolve_source_path`'s PATH search
  initially read `std::env::var_os("PATH")` — the raw OS process
  environment — so an in-shell `PATH=$PATH:dir` assignment (exported or
  not) was invisible to it, since rush only threads exported variables into
  a *spawned child's* environment rather than syncing them back into this
  process's own. Switched to the same `vars::get("PATH").or_else(||
  std::env::var("PATH").ok())` fallback `expand.rs` already uses for `$PATH`
  expansion. The same root-cause bug still affects `command -v`/`type`/
  `hash` (C12, already shipped) — tracked separately as C36 in
  `docs/CAPABILITY_GAPS.md`.

### `eval` builtin (C15)
- `exec::eval_cmd`/`builtins::eval_cmd`: joins its arguments with a single
  space, parses the result, and runs it in the *current* shell — unlike
  `source` (C14), `eval` establishes no scope at all. No filename/PATH
  search, no positional-parameter swap, and — verified directly against
  real bash — a `return`/`break`/`continue` inside the evaluated text is
  *not* consumed; it propagates straight to whichever function/loop
  actually encloses the `eval` call, exactly as if the text had been typed
  inline.
- No arguments (or all-empty ones) is a no-op that succeeds; a parse error
  fails with status 2, matching rush's own existing convention for a
  top-level syntax error.
- Found but out of scope here, and not specific to `eval`: running any
  unknown command name anywhere in a rush script — not just inside `eval`
  — prints a raw OS spawn error and aborts the *entire script*, instead of
  reporting exit status 127 and continuing like every other POSIX shell.
  Tracked separately as C37 in `docs/CAPABILITY_GAPS.md`.

### `exec` builtin (C16)
- `exec::exec_cmd` (Unix only), registered as a normal builtin so its
  redirects flow through the existing `run_builtin_foreground`/
  `redirect_stdio` machinery unchanged.
- With a command (`exec cmd args...`): replaces the current process image
  via `execvp` (`CommandExt::exec`) — no fork, so on success this never
  returns. Inherits whatever fds 0/1/2 the caller's own redirects already
  left them as, plus the shell's exported environment, exactly like a
  normal spawned child. On failure (command not found) — verified directly
  against real bash — a non-interactive shell exits immediately with
  status 127 (the *whole script* stops there), while an interactive one
  just reports 127 and keeps running, redirects restored as normal.
- With no command (bare `exec`, or `exec` followed only by redirects, e.g.
  `exec > file`): a no-op that always succeeds, except the redirects are
  made *permanent* — a new `StdioGuard::disarm` closes the saved originals
  instead of restoring them on drop, the one case where a builtin's
  redirects are meant to outlive the call.
- Found but out of scope here, and not specific to `exec`: rush's redirect
  machinery (`redirect_stdio` *and* `build_stage` — builtins and real
  spawned children alike) only ever wires up fd 0/1/2; any other target
  `fd` (`cmd 3>file`, `exec 3>file`) silently collapses to fd 1 instead of
  actually opening fd 3. Pre-existing across the whole shell — `exec` is
  just the first place it blocks a headline idiom rather than being an
  edge case. Tracked separately as C38 in `docs/CAPABILITY_GAPS.md`.

### `umask` builtin (C17)
- `builtins::umask_cmd` (Unix only): a real `libc::umask()` call, so it
  actually changes the permissions every subsequent file/directory this
  process (or anything it execs/spawns) creates, not just a shell-internal
  display value.
- No argument reports the current mask — plain 4-digit octal (`0022`), or
  `u=rwx,g=rx,o=rx`-style with `-S` — both verified directly against real
  bash. Reading it without changing it means setting it right back, since
  `umask()` itself only ever *sets*, returning the previous value.
- One argument sets it from an octal string; an out-of-range or malformed
  mode fails with status 1 without touching the mask. Symbolic *setting*
  (`umask u=rwx,g=rx,o=`) isn't supported, only octal.

### `set -u` (nounset) (C18)
- `vars::set_nounset`/`nounset` (mirroring `errexit`'s own thread-local
  flag) plus two new checked lookups in `expand.rs` — `var_lookup_checked`,
  `arg_checked` — used everywhere a plain value is needed: `$name`/
  `${name}`, `${#name}`, the `#`/`##`/`%`/`%%` pattern-removal operators,
  and numbered positional parameters (`$1`, `${10}`). Referencing an unset
  one is now an error that aborts the rest of the script, instead of
  silently expanding to an empty string.
- Exemptions verified directly against real bash: the `:-`/`:=`/`:+`/`:?`
  default/alternate family defines its own unset-variable handling and
  stays untouched; `$@`/`$*`/`$#`/`$?`/`$$` are always considered set, even
  with zero positional parameters, while a specific numbered one is still
  subject to the check; a set-but-empty variable is fine (the test is
  "unset", not "empty"); `set +u` turns it back off.
- One caveat, shared with the pre-existing `${VAR:?msg}` error rush already
  had (not introduced by this change): bash exits a non-interactive shell
  with status 127 for an unbound reference specifically, but rush's exits
  with 1 like most of its other expansion errors — the script still aborts
  right there either way, just with a different code.
