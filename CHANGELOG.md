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

### `set -o pipefail` (C19)
- `vars::set_pipefail`/`pipefail` (mirroring `errexit`/`nounset`'s own
  thread-local flags), `set`'s new `-o`/`+o` two-token parsing (`set -o
  pipefail`, `set +o pipefail`; an unrecognized `-o` name is an error, not
  a silent no-op), and a shared `exec::pipeline_status` helper called
  wherever a pipeline's stages get reduced to one exit code: the non-Unix/
  capture runner (`exec::run` — used for both a non-Unix foreground
  pipeline *and* `$(...)` command substitution, which is also subject to
  pipefail, verified directly) and the Unix job-control runner
  (`job::wait_pgid`, which now tracks every stage's own exit code by
  position instead of only the last).
- Without pipefail, a pipeline's status is still just its last stage's;
  with it, the *rightmost* non-zero status among all stages — not "the
  first failure", not "any failure", specifically the one closest to the
  end (verified directly against real bash with a distinct exit code at
  each position to disambiguate) — or 0 if every stage succeeded.

### `set -x` (xtrace) (C20)
- `vars::set_xtrace`/`xtrace` (mirroring the other `set` flags' own
  thread-local state) and `exec::trace_pipeline`, called from the one place
  both the foreground and `$(...)`-capture paths funnel every
  already-expanded `Pipeline` through (`run_foreground`/`capture_pipeline`)
  — covers a plain command, each stage of a real pipeline, an
  assignment-only statement, and a compound's own condition (`if`/`while`/
  `until`, which run through this same machinery), all from one hook.
- Each traced line is prefixed with `$PS4` (default `+ `, falling back to
  the environment like `$PS1` does); a leading `NAME=value` assignment
  traces on its own line before the command it applies to; a word
  containing whitespace or a shell-special character is re-quoted with
  single quotes for display.
- Nesting inside `$(...)` repeats `$PS4`'s first character once per level
  (`vars::with_deeper_trace`, wrapping `expand::command_substitute`) — `++`
  one level down, `+++` two, matching real bash exactly, verified directly
  including two-deep nesting and a custom `$PS4`.
- Known gap, accepted for this scope: a compound's own *header* line (`for
  i in 1 2`, `case a in`) isn't traced, only the commands actually inside
  its body — which do trace correctly, per iteration/branch.

### `TERM`/`HUP` traps actually firing (C21)
- Real signal handlers (`trap::install_signal_handlers`), installed once at
  startup in every mode — interactive or not, since the target use case (a
  container's PID 1 catching `TERM` to shut down gracefully) has no
  terminal at all. The handler itself only stores which signal arrived in
  a plain `AtomicI32` (safe from signal context: no heap, no locks); `trap::
  check_pending`, called back from ordinary code, does the real work —
  firing the registered trap, or, if none is registered, terminating with
  the conventional `128 + signal` status (still running any `EXIT` trap
  first, exactly like real bash).
- The headline behavior, verified directly against real bash: a trapped
  signal interrupts a blocking wait *immediately*, not just once the
  foreground job finishes on its own. `job::wait_pgid`/`wait_job_pgid`/
  `wait_one`'s blocking `waitpid` loops now distinguish `EINTR` (retry
  after handling the pending signal) from `ECHILD` (really done) — if the
  trap body calls `exit`, the process is gone before the loop ever
  resumes; if it doesn't, the wait simply resumes, exactly reproducing
  bash's own "the sleep picks up where it left off" behavior.
- `check_pending` is also called at every ordinary command boundary
  (`exec::exec_list_impl`'s per-job loop, covering every script, loop
  body, function body, sourced file, and `eval`'d string) and before each
  interactive prompt, for signals that arrive when nothing is blocking at
  all.
- Out of scope: `ERR`/`DEBUG` (bash/ksh/zsh extensions, not POSIX-mandated)
  remain unimplemented.

### Indexed arrays (C22)
- **Storage** (`vars.rs`): a variable's payload is now `enum VarValue {
  Scalar(String), Array(BTreeMap<usize, String>) }` — `BTreeMap` for real
  sparse-array semantics (`arr[5]=x` on a 2-element array doesn't create
  indices 2–4), with free sorted iteration for `${arr[@]}`/`${!arr[@]}`.
  Every existing scalar function branches on this now, alongside new
  array-specific ones (`set_array`, `array_get`/`array_set`/`array_append`/
  `array_append_index`, `array_values`/`array_indices`/`array_len`,
  `array_unset_index`, `declare_local_array`) and a shared `assign(name,
  &AssignOp)` covering all four assignment shapes (scalar/array × set/
  append) plus the two indexed ones (`arr[i]=`/`arr[i]+=`).
- **Lexer**: a new `WordPart::ArrayLiteral(Vec<Word>)` — `arr=(a b c)`
  needed a lexer-level heuristic recognizing a word ending in `=`/`+=` with
  no space before an immediately-following `(`, consuming the whole
  parenthesized list (spanning newlines, each element its own `Word`) as
  one `WordPart` instead of breaking the word at the paren the way a
  subshell/case-group `(`/`)` normally would.
- **Expansion**: `assignment_split` recognizes `NAME=(...)`/`NAME+=(...)`
  (elements individually glob/command-substitution-expanded), plain
  `NAME=value`/`NAME+=value`, and `NAME[subscript]=value`/
  `NAME[subscript]+=value` — the subscript evaluated as arithmetic via the
  same two-step pipeline `$((...))` itself uses, so both `${arr[i+1]}`
  (bare) and `${arr[$i]}`/`arr[$i]=x` (`$`-prefixed) resolve. `expand_braced`
  gained `${arr[N]}`, `${arr[@]}`/`${arr[*]}` (mirroring `$@`/`$*`'s own
  join-vs-preserve distinction, including a `"${arr[@]}"`-is-like-`"$@"`
  special case for quoted whole-array expansion), `${#arr[@]}`/`${#arr[N]}`,
  and `${!arr[@]}`. `arr=x` on an *existing* array targets element 0 only,
  leaving the rest alone (lives in the ordinary `set()`, so it applies
  anywhere a scalar assignment targets an already-array name).
- **`local`**: `local arr=(a b c)` needed special handling since a plain
  `Vec<String>` argv can't carry an array literal — `expand_simple`
  recognizes the command word "local" and parses its declarations into a
  new `Command::local_decls` field, dispatched via a new
  `builtins::local_from_decls` straight from `exec::dispatch_builtin`
  rather than through the ordinary string-argv builtin path.
- Every case — literal assignment, all three read forms, sparse arrays,
  element/whole-array set and append, `unset` (whole array and single
  index, including `unset 'arr[$i]'`'s own independent subscript
  evaluation), scalar↔array promotion, and `local` — was verified directly
  against real bash.
- Explicitly out of scope, each a documented, accepted gap: negative
  indices, `${arr[@]:offset:length}` slicing, a subscript combined with
  pattern-removal/default-alternate operators, `declare -a`/`declare -p`
  (no `declare` builtin exists at all), `local arr[i]=x`, exporting an
  array to a spawned child's environment, arithmetic side effects inside a
  subscript.

### Associative arrays (C23)
- **`declare` builtin** (new, `builtins.rs`): bash requires `declare -A
  name` before `name[key]=val` treats `key` as a literal string key rather
  than an arithmetic expression; rush's `declare` is a narrow subset —
  `-a`/`-A` plus an optional `=(...)` initializer — dispatched through the
  same `Command::local_decls` mechanism C22 built for `local`. Not
  implemented: `-p`, `-x`, `-r`, `-i`, `-f`, or bash's "`declare` acts like
  `local` inside a function" nuance (rush's `declare` is always
  global/current-scope).
- **Storage** (`vars.rs`): `VarValue` gained `Assoc(BTreeMap<String,
  String>)` alongside `Scalar`/`Array`, plus `is_assoc(name)` and
  assoc-specific `set_assoc`/`assoc_get`/`assoc_keys`/`assoc_unset_key`/
  `assoc_merge` (upsert-by-key for `+=`)/`declare_local_assoc`.
- **Type-aware subscript retrofit**: C22 always evaluated a subscript as
  arithmetic; associative arrays need `arr[a+b]=x` on a `-A` array to use
  the *literal* key `"a+b"` while `arr[$k]=x` still `$`-expands `$k` — only
  resolvable once the target's runtime type is known. `AssignOp`'s indexed
  variants changed from `SetIndex(usize, String)`/`AppendIndex(usize,
  String)` to `SetKey(String, String)`/`AppendKey(String, String)` (raw
  subscript, deferred evaluation), with `vars::key_set`/`key_append`
  dispatching to the array or assoc path based on `is_assoc`.
  `expand::eval_subscript` split into `resolve_subscript_text` ($-expand
  only, always applied) and a narrower arithmetic-only `eval_subscript`
  used only once a name is confirmed not-assoc.
- **Expansion**: `${arr[key]}`, `${!arr[@]}` (keys), `${arr[@]}`/
  `${arr[*]}` (values), and `${#arr[@]}` all dispatch on `is_assoc`.
  `"${!arr[@]}"`/`"${arr[@]}"` preserve each key/value as its own field
  when quoted, the assoc analogue of indexed arrays' `"$@"`-like handling.
  `arr+=([k1]=v1 [k2]=v2)` merges/upserts by key rather than positionally
  appending — both the `local`/`declare`-prefixed literal path and the
  ordinary top-level `NAME+=(...)` path now check `is_assoc(&name)` before
  parsing elements as plain words vs. `[key]=value` pairs.
- **`local`/`declare`**: the `local`-only special-casing `expand_simple`
  built for C22 is now shared by `declare`, scanning both for `-A`/`-a`
  flags to decide array-vs-assoc-vs-scalar before parsing declarations.
- Every behavior — `declare -A`, literal assignment, all read forms,
  `arr[k]=`/`arr[k]+=`, merge-by-key `+=`, `unset 'arr[k]'`, scalar↔assoc
  promotion, and `local`/`declare -A arr=(...)` — was verified directly
  against real bash.
- Explicitly out of scope, each a documented, accepted gap: a literal
  multi-word key written directly inside `[...]` in an assignment
  (`arr[key with spaces]=val`, `arr["b c"]=2`; the `k="b c"; arr[$k]=val`
  idiom works); `declare -p`/`-x`/`-r`/`-i`/`-f`; `declare`'s
  function-local scoping; bash's explicit-index syntax for *indexed*
  arrays (`arr=([5]=x [2]=y z)`); a subscript combined with
  pattern-removal/default-alternate operators (the same pre-existing C22
  gap, not newly introduced).

### Brace expansion (C24)
- **`BraceAtom`** (`expand.rs`, new): re-represents a `Word`'s content for
  scanning purposes — `Ch(char)` for a character from an `Unquoted` part
  (eligible to be `{`/`,`/`.`, or ordinary text) or `Opaque(WordPart)` for
  a `Quoted`/`Literal`/`ArrayLiteral` chunk, inert to brace syntax but
  still carried through verbatim into whichever alternative it lands in
  (`pre{"a,b",c}post` splits on the *unquoted* comma only).
- **Scanning** (`brace_expand_atoms`): scans left to right for the first
  valid `{...}` group (depth-tracked bracket matching via
  `matching_close`) and expands it, recursing into the suffix for any
  further group (`{a,b}{c,d}` is a cross product); an invalid group (no
  top-level comma and not a valid range) is left as a literal `{` and the
  scan resumes right after it, so one invalid group doesn't block a valid
  one later in the same word (`{{a,b}` → `{a`, `{b`).
- **Comma-lists and ranges** (`expand_group`/`expand_range`): a
  comma-list splits only on *top-level* commas (one inside a nested
  `{...}` doesn't count), and each segment is itself recursively
  brace-expanded. A range is numeric (`{1..5}`, `{-3..3}`) or
  single-letter (`{a..z}`, stepping raw ASCII code points even across a
  mixed-case pair like `{A..z}`), both with an optional third `..step`
  field (sign ignored, direction inferred from the endpoints, an explicit
  step of `0` treated as `1`). Zero-padding: a leading `0` on either
  endpoint (after an optional sign, more than one digit) pads every
  generated term to that endpoint's own total literal width, sign
  included (`{-01..05}` → `-01 000 001 002 003 004 005`); a leading `+`
  never triggers it.
- **Wiring**: hooked into `expand_argv_word` (covering ordinary command
  arguments, `for`-loop word lists, and array-literal elements, which all
  already funnel through it) and into `local`/`declare`'s own
  argument-parsing loop — verified directly that `local x={a,b}`
  brace-expands into two words (`x=a` then `x=b`, applied in order,
  leaving `x=b`), since bash treats `local`'s arguments as ordinary
  command words, not assignment-statement syntax. Deliberately *not*
  wired into real assignment-statement values: a bare `x={a,b}` or a
  prefix `FOO={a,b} cmd` keeps the literal text, matching bash exactly.
- Runs on a word's raw, unexpanded text, before `$`/glob expansion — same
  order as real bash: `{$x,y}` expands the braces into two words first,
  then `$x` resolves normally in whichever one it lands in; `{1..$n}` is
  an invalid range at brace-expansion time (`$n` isn't yet a literal
  integer) and stays literal text even though `$n` itself still expands
  afterwards.
- Explicitly out of scope, each a documented, accepted gap: redirect
  targets and case subjects/patterns aren't brace-expanded (real bash
  *does* brace-expand a redirect target, erroring "ambiguous redirect" on
  more than one resulting word — rush's redirect-target expansion doesn't
  go through this path at all); a generated range element that happens to
  itself be a shell metacharacter (a bare `\` from a mixed-case ASCII
  range crossing code point 92, e.g. one term of `{A..z}`) doesn't get
  real bash's own post-generation backslash-consumption quirk.
- Verified directly against real bash across more than 60 scenarios —
  comma-lists, nesting, cross products, quoting interactions,
  numeric/letter ranges with and without an explicit step, zero-padding,
  the assignment-vs-argument-word distinction, and the `$`-expansion
  ordering — matching exactly except the one documented backslash corner
  above.

### `case` fallthrough (C25)
- **Lexer**: two new tokens, `Token::SemiAmp` (`;&`) and
  `Token::DSemiAmp` (`;;&`), alongside the existing `Token::DSemi`
  (`;;`).
- **Parser**: a new `CaseTerm` enum (`Break`/`FallThrough`/`Continue`)
  recorded per `Compound::Case` item, defaulting to `Break` when the last
  item before `esac` omits a terminator, same as before.
- **Execution** (`exec.rs`): `;;` (`Break`) stops. `;&` (`FallThrough`)
  unconditionally runs the *next* item's body too, with no pattern test,
  chaining through that item's own terminator in turn. `;;&`
  (`Continue`) resumes *pattern* testing at the next item — not
  unconditional — running the first one (if any) whose pattern matches,
  same as if the whole `case` restarted from there. `$?` after the whole
  `case` is always the last body that actually ran.
- Verified directly against real bash across 10 scenarios: a single
  `;&`, a chain of several, `;;&` finding a later match vs. finding none,
  a trailing `;;&` on the last item (nothing left to resume into — stops,
  same as `;;`), exit-status propagation through a fallthrough chain, and
  the terminator-less last-item default — matching exactly.

### `select` (C26)
- **Grammar** (`parser.rs`): `select NAME [in WORDS]; do BODY; done` — a
  new `Compound::Select` variant, same grammar/`has_in` convention as
  `for` (an omitted `in` iterates `"$@"`; an explicit `in` with no words
  is a real empty list).
- **Execution** (`exec.rs`): an empty word list is a no-op, same as
  `for`. Otherwise prints a numbered menu to *stderr*, then loops:
  prints `$PS3` (default `"#? "`, `$`-expanded via `expand::
  expand_dollars` — now `pub(crate)`, since `$PS1`'s own prompt
  expansion is a separate, bespoke backslash-escape scheme in
  `main.rs`), reads a line into `$REPLY` *raw* (no `$IFS`
  splitting/trimming at all, unlike ordinary `read`, though it does
  share `read`'s own backslash-continuation processing via a new
  `builtins::read_reply_line`). A genuinely empty line redisplays the
  menu and prompts again without running `BODY`; otherwise the line,
  parsed as a base-10 integer in `1..=len(words)`, sets `NAME` to that
  word (or `""` if out of range/unparseable) and `BODY` runs once, with
  the same `break`/status machinery `for`/`while` use. EOF ends the
  whole construct with status `1`, overriding `BODY`'s last status — a
  real bash quirk, verified directly.
- Explicitly out of scope, an accepted cosmetic (not functional)
  narrowing: real bash lays the menu out in columns sized to
  `$COLUMNS`; rush always prints one entry per line.
- Verified directly against real bash across more than a dozen
  scenarios — index parsing (whitespace/sign/leading-zero tolerance,
  out-of-range/unparseable → empty `NAME`), blank-line redisplay vs. an
  all-whitespace line (which *does* run the body, unlike blank), `$PS3`
  default/custom/`$`-expansion, `break`'s exit status, and the
  EOF-forces-status-1 override — matching exactly.
- Found while verifying this item: `set -- args…`/`set args…` doesn't
  reassign positional parameters at all in rush — general, not specific
  to `select`, tracked as C39 (open, not fixed in this change).

### C-style `for`, standalone `((expr))`, richer arithmetic (C27–C29)
Done together in one pass — all three needed the same lexer/`arith.rs`
groundwork.

- **`Token::DblParen(String)`** (new, `lexer.rs`): `((` with no space is
  always an arithmetic command or `for` header, never two nested
  subshells (which need an explicit space, `( (cmd) )`, to get that
  reading instead) — verified directly (`((echo hi))` fails as invalid
  arithmetic rather than falling back to running `echo hi` in nested
  subshells). Emitted wherever the lexer would otherwise tokenize a bare
  `(`, via a new `take_double_paren` mirroring `expand::take_arith`'s
  identical depth-tracking algorithm.
- **`Compound::Arith(String)`** (C28, new, `parser.rs`/`exec.rs`): a
  standalone `((expr))` command at command position. `$`-expands `expr`
  then evaluates it via `arith.rs` for its side effects; exit status `0`
  if the result is nonzero, `1` if zero (or if `expr` is empty — `(( ))`
  evaluates as `0` rather than erroring, a real bash asymmetry with
  `$(( ))`, which does error on empty, verified directly).
- **`Compound::CFor { init, cond, update, body }`** (C27, new): `for
  ((init; cond; update)); do BODY; done`. `parse_for` checks for
  `Token::DblParen` right after `for` (unambiguous — a `NAME` can never
  start with `(`), splitting the raw header on `;` into the three
  clauses, `None` for an empty one (`for ((;;))` is a real infinite
  loop). Execution evaluates `init` once, then loops testing `cond` (or
  always-true if absent), running `body`, then `update` — except when
  `body` ended via `break` (here or propagating outward), in which case
  `update` doesn't run; a local `continue` still runs it first — real C
  `for` semantics, verified directly.
- **`arith.rs` rewrite** (C29): previously a combined parse-and-evaluate
  pass; now parses into an `Expr` tree first, then a separate `eval_expr`
  walks it — needed so `&&`/`||`/`?:` can genuinely short-circuit
  (`0 && (i=5)` must never run the assignment, verified directly, which
  a combined pass can't skip once already recursed into the right side).
  Adds, in precedence order (lowest to highest, every boundary verified
  directly against real bash): assignment (`=`, `+= -= *= /= %= <<= >>=
  &= ^= |=` — no `**=`, matching bash exactly, which doesn't have one
  either), right-associative; ternary `?:`, right-associative in its
  `else` position; bitwise `| ^ &` (looser than comparisons — `2+3 & 1`
  is `(2+3)&1`, not `2+(3&1)`); shift `<< >>`; `**` (right-associative,
  tighter than `*` but looser than unary — `2*3**2`=18, `-2**2`=4);
  prefix `- + ! ~ ++ --`; postfix `++`/`--` (only valid on a plain
  variable name). Assignment/`++`/`--`'s lvalue must be a variable name —
  no array-element lvalues (`arr[i]++`) or comma operator.
- Adds 7 new `arith.rs` unit tests, 2 new parser unit tests, and 8 new
  `exec_behavior.rs` integration tests; full suite and clippy stay clean.

### Here-strings `<<<` (C30)
- New `RedirOp::HereString` (lexer): checked for right after `<<`, before
  falling into ordinary heredoc-delimiter reading, so `<<<` is never
  misread as `<<` followed by a stray `<`.
- New `RawRedirect::HereString(Word)` (parser): the word is read exactly
  like any other redirect's filename.
- Expansion (`expand.rs`) treats that word as an ordinary redirect
  target — single-word expansion, no splitting/globbing (verified
  directly: `x="a b"; cat <<< $x` still feeds `a b` as one line) —
  appends exactly one `\n` (always, even if the text already ends with
  one, matching real bash), and drops the result into the same
  `heredoc: Option<String>` slot a real here-document's body already
  uses. `exec.rs` needed zero changes: the feeding path
  (`redirect_stdio`/`feed_heredoc`) was already generic over "some
  string feeds stdin." A later `<<`/`<<<` on the same command overwrites
  an earlier one, same "last redirect for a given fd wins" rule as any
  other redirect, verified directly.
- Adds 1 new integration test; full suite and clippy stay clean.

### Process substitution `<(cmd)` / `>(cmd)` (C31)
The biggest lift in Tier IV — real fork/pipe plumbing, not just
parser/`arith.rs` work — and the item that closes it out completely.

- **`exec::process_substitute(src, write_side)`** (new, `#[cfg(unix)]`):
  parses `src` (`parser::parse`), forks it hooked up to one end of a
  `make_pipe()` pipe (`run_list` runs it in the child) — its stdout for
  `<(cmd)`, its stdin for `>(cmd)` — and returns a `/dev/fd/<n>` path for
  the *other* end, kept open in the shell process itself. Never blocks
  waiting for `cmd` (verified directly: `diff <(sleep 1; echo a) <(sleep
  1; echo b)` takes ~1s total, not ~2s serialized). `$!` reflects the
  substitution's own pid (verified directly against real bash), but it's
  deliberately not added to the job table, matching real bash's own
  `jobs -l` not listing one either.
- **Lifecycle**: the kept-open fd survives, unclosed, until the caller
  has finished spawning whatever the substitution's path expanded into
  (fork+exec inherits open, non-`CLOEXEC` fds unchanged — `make_pipe`'s
  raw `libc::pipe` already doesn't set `CLOEXEC`), then a new
  `close_pending_proc_subs` (backed by a `PENDING_PROC_SUBS` thread-local)
  closes it and non-blocking-reaps its child. Called once at each of the
  handful of places a whole pipeline actually gets spawned
  (`run_foreground`, backgrounding in `run_job`, `capture_pipeline`),
  covering every path a substitution's word could expand into (builtin,
  function call, or a real spawned child) without duplicating this at
  each of *those* individually.
- **Lexing/expansion**: no new `WordPart` variant needed — `<(cmd)`/
  `>(cmd)` is captured as raw text embedded in a `WordPart::Unquoted`
  string, exactly like `$(cmd)` already is, via the existing
  `consume_balanced_paren`. The lexer checks for `(` immediately after
  `<`/`>` (never a real redirect there, verified directly) both at the
  top level and inside `lex_word`'s own per-character loop, so
  `pre<(cmd)post` concatenates the same way `$(...)` does. A new
  `expand::expand_unquoted` (`expand_dollars` plus this same `<(`/`>(`
  recognition) is used specifically for genuinely unquoted text —
  quoting fully suppresses process substitution in real bash (unlike
  `$(...)`, which still expands inside double quotes, verified directly)
  — so it's a separate function, not a flag threaded through
  `expand_dollars` itself. Assignment RHS *does* get it, a real
  asymmetry with brace expansion, verified directly.
- **A real, general bug found and fixed along the way, unrelated to
  process substitution itself**: Rust's runtime sets `SIGPIPE` to
  `SIG_IGN` at startup, so any builtin's `print!`/`println!` surfaced a
  write to an already-closed pipe as an `Err` that those macros then
  panic on — reproduced with no process substitution involved at all
  (`rush -c 'while true; do echo x; done' | head`). Fixed by resetting
  `SIGPIPE` to its default disposition once at startup (`main.rs`),
  matching real bash's own behavior exactly (verified directly: bash's
  own builtin `echo` hits the identical race against a `>(...)` whose
  reader exits without reading, and just dies silently there too).
- Explicitly out of scope: real bash's own `/dev/fd` fd-numbering
  convention (a fixed high range, counting down per substitution) — rush
  just uses whatever fd the OS returns; combining a substitution with an
  explicit non-standard redirect-target fd inherits the pre-existing C38
  gap, not a new one; a write-side substitution whose reader exits
  without reading races the main command's own write against the
  reader's exit — confirmed to reproduce identically, and just as
  unpredictably, in real bash itself under load, not something to paper
  over with synchronization real bash doesn't have either.
- Verified directly against real bash across more than a dozen scenarios
  — read side, write side, concatenation, quoting suppression, nesting,
  piping inside a substitution, assignment RHS, redirect targets, `$!`,
  concurrent timing, and status independence. Adds 7 new integration
  tests; full suite and clippy stay clean.

### History expansion `!!` / `!n` / `!$` / `!*` / `!:n` (C32)
The first item in Tier V (interactive UX) — bash/ksh/zsh's csh-style
bang-history recall, layered on top of the persistent history `rustyline`
already provided.

- **New `history_expand` module** — `expand(line, history) -> Result<Option<String>, String>`,
  a plain textual preprocessing pass run in `main.rs`'s `interactive()`
  loop before the line reaches `parser::parse` or `rl.add_history_entry`,
  matching where real bash's own readline/history layer does this.
  `Ok(None)`: nothing to expand, pass the line through unchanged (the
  common case). `Ok(Some(expanded))`: something changed — echoed to
  stdout before running, matching real bash. `Err(message)`: an
  unresolvable reference — printed to stderr, and the line runs nothing
  at all, matching real bash's own "a failed reference blocks execution"
  behavior.
- **Interactive-only**, matching real bash's own `histexpand` default (on
  interactively, off in scripts) — `rush -c`/`rush file` never runs this
  pass.
- **Whole-event recall**: `!!` (last command), `!n`/`!-n` (absolute/
  relative event number, matching `history`'s own 1-based numbering),
  `!string`/`!?string?` (backward prefix/substring search).
- **Word designators**, on the previous command only: `!$` (last word),
  `!^` (first argument), `!*` (all arguments), `!:n` (word `n`, 0-based,
  `n=0` the command name itself).
- **Quoting/escaping**, verified directly against real bash: single
  quotes suppress expansion; double quotes do *not*; `\!` de-escapes to a
  literal `!` with no echo (bash's own history file stores the still-
  backslashed raw line here — passing the untouched line through
  unexpanded and relying on rush's own lexer's already-generic `\X` →
  literal `X` handling produces the identical result, so no duplicate
  logic needed). A bare `!` before whitespace, end of line, or `=` (so
  `test`'s `!=` is never misread) is left untouched, no error.
- Explicitly out of scope: combining an explicit event specifier with a
  word designator in one reference (`!2:1`, `!echo:$`) — the two forms
  above cover the overwhelming majority of real usage (`sudo !!`, reusing
  `!$`) on their own; quote-aware word splitting for the designators
  (real bash treats a quoted phrase as one word for `!:n`, rush uses a
  plain `split_whitespace`).
- Verified directly against real bash (`bash -i`, isolated `HISTFILE`s)
  across more than a dozen scenarios. Adds 10 unit tests plus 9
  integration tests exercising the compiled binary in piped/interactive
  mode; full suite and clippy stay clean.

### History-based autosuggestions (C33)
A dimmed, greyed-out inline suggestion of the current line, completed
from history as you type — fish's signature feature, common via plugin
in zsh.

- **`RushHelper` (`src/completion.rs`)** now holds a
  `rustyline::hint::HistoryHinter` and delegates its own `Hinter::hint`
  straight to it, replacing the previous no-op impl. `HistoryHinter` is
  `rustyline`'s own ready-made hinter: searches history backward from the
  current entry for the most recent one starting with what's typed so
  far, offering the remainder as the hint (no suggestion on an empty line
  or an exact match).
- **`Highlighter::highlight_hint`** dims the suggestion (ANSI
  `\x1b[2m...\x1b[0m`) so it reads as a suggestion rather than text
  already on the line — the only genuinely new code this item needed;
  accepting it (right arrow at end of line) is rustyline's own
  pre-existing key binding, unmodified.
- Verified end-to-end against the compiled binary under a real
  pseudo-terminal (`pty.fork()`): typing `echo he` after `echo hello
  world` is in history renders the dimmed suggestion `llo world`;
  accepting it and pressing Enter runs the full command. Inherently a
  live-terminal feature — `Editor::readline` falls back to plain
  file-style reading (no hints at all) when stdin isn't a real TTY, so
  it's covered by unit tests exercising `RushHelper`'s `Hinter`/
  `Highlighter` impls directly against a `DefaultHistory` and
  rustyline's own `Context::new` testing constructor, rather than the
  piped-stdin integration pattern used elsewhere. Adds 3 unit tests; full
  suite and clippy stay clean.

### Argument- and context-aware completion (C34)
Closes out Tier V completely, 3 of 3. Rush's completion used to be file/
PATH/builtin-name only; it now recognizes a fixed set of the highest-value
cases where that's rarely what's actually wanted, rather than a full
fish/zsh completion-spec engine.

- **Variable names** — a bare, still-open `$name`/`${name}` completes
  every shell + environment variable name, reconstructing `$name` or
  `${name}` (auto-closing the brace) in the replacement. New
  `vars::names()` enumerates the shell-variable side.
- **`cd`'s argument** completes directories only — reuses rustyline's own
  `FilenameCompleter` for the actual matching, then filters to
  directories via a plain filesystem check. Confirmed directly that this
  isn't bash's own default behavior either (bare readline, no
  bash-completion, still lists files alongside directories) — a genuine
  fish/zsh-parity addition.
- **`export`/`unset`/`local`/`declare`'s arguments** complete variable
  names (same enumeration as above); a word starting with `-` is left
  uncompleted rather than nonsensically offering variable names for a flag.
- **`alias`/`unalias`'s arguments** complete existing alias names
  (`alias::all()` already existed) — only before an `=`, which starts a
  new alias's value instead.
- **(Unix only) `fg`/`bg`/`kill`/`wait`'s arguments** complete `%n` job
  specs from the live job table, in the exact plain `%N` format those
  builtins already parse. New `job::ids()` enumerates the job table.
- Explicitly out of scope: flag completion for any builtin; variable
  completion when `$`/`${` isn't the start of the word being completed, or
  unwrapped out of an open double quote; per-external-command argument
  specs (`git <TAB>` subcommands, `ssh <TAB>` known hosts) — the rest of
  what a real fish/zsh completion *system* provides beyond this fixed
  case list.
- Verified end-to-end against the compiled binary under a real
  pseudo-terminal (`pty.fork()`) across all five cases, including that
  `cd`'s directory filtering excludes a real file sitting next to a real
  directory and that a job spawned with `sleep 100 &` completes `fg %`
  to `fg %1`. Adds 8 new unit tests exercising the pure completion
  functions directly; full suite and clippy stay clean.

### Fix: `\$` inside double quotes wasn't staying literal (C35)
POSIX-mandated, present in dash/bash/ksh/zsh: inside `"..."`, `\$` must
produce a literal `$` (suppressing expansion of whatever follows), the
same as `\"`/`\\` already do. `echo "\$?"` used to print the exit status
instead of the literal text `$?`; `echo "\$FOO"` printed `$FOO`'s value
instead of the literal text `$FOO`.

- **Root cause**: the lexer stripped the backslash and pushed the bare
  `$` straight into the double-quoted text, which becomes a
  `WordPart::Quoted` string re-scanned for `$`/`$(...)` later — by then, a
  bare `$` left behind this way is indistinguishable from a real,
  unescaped one.
- **Fix (`lexer.rs`)**: `\$` now flushes whatever quoted text came before
  it and emits a separate `WordPart::Literal("$")` instead — never
  re-expanded, by definition, the same guarantee `'...'` already gives —
  then keeps lexing the rest of the double-quoted span as before. No new
  escape-recognition logic needed in `expand.rs` itself.
- Verified directly against real bash, including composing an escaped
  `\$` with a real, still-expanding `$FOO` in the same string
  (`"pre\$mid$FOO"` → `pre$midbar`), and that the easy-to-conflate `\\$FOO`
  (a literal backslash, followed by an ordinary, unescaped, still-
  expanding `$FOO`) keeps expanding correctly. Adds 3 new lexer unit tests
  plus 2 integration tests; full suite and clippy stay clean (down to 9
  pre-existing warnings from 10 — an incidental improvement from
  restructuring the affected code, not a new fix).

### Fix: `command -v`/`type`/`hash` (and spawning) not seeing an in-shell `PATH` change (C36)
Closes out Tier II completely. `builtins::resolve_in_path` and
`completion.rs`'s `$PATH`-scanner called `std::env::var_os("PATH")`
directly — the real OS process environment — instead of the shell's own
`PATH` variable, so a plain `PATH=$PATH:dir` was invisible to `command
-v`/`type`/`hash`. Fixed with the same fallback `expand.rs` and `source`
(C14) already use: `vars::get("PATH").or_else(|| std::env::var("PATH").ok())`.

- **A deeper root cause turned up while re-verifying this doc's own
  original claim that actually *running* the command already worked** —
  it didn't, for the case that matters most: a bare `PATH=$PATH:dir` (no
  `export`). Rush never seeded its own variable table from the inherited
  process environment at startup, so the first assignment to `PATH` (or
  any other already-exported, OS-inherited name) created a brand-new
  internal entry marked `exported: false` — nothing to preserve, since
  `PATH` had never been recorded as exported to begin with. Internal
  lookups saw the update; `exec::build_stage`'s `command.envs(vars::exported())`
  (which only adds/overrides on top of `Command`'s default full-
  environment inheritance, never removes) fed the spawned child the
  *original*, unextended `PATH` instead.
- **Fix (`main.rs`)**: at startup, before any rc file or script runs,
  every inherited environment variable is registered via
  `vars::set_exported` — matching real bash's own rule that an
  environment-inherited variable stays exported through a later plain
  reassignment (`vars::set`'s existing-entry path already correctly
  preserves whatever `exported` flag is there; there was just nothing to
  preserve).
- Verified directly against real bash: `PATH=$PATH:dir; command -v tool;
  type tool; hash tool; tool` now matches bash's output and exit status
  exactly, including the actual spawn. Adds 1 new integration test
  (real temp directory + executable script + plain PATH extension across
  all four); full suite and clippy stay clean.
- Chasing this down turned up one further, narrower bug, deliberately
  left for its own item: `unset` of an inherited/exported variable
  doesn't stop a spawned child from still seeing it either — see C40
  below.

### Tracked: `unset` of an inherited/exported variable still reaches a spawned child (C40)
Newly discovered while fixing C36, not yet fixed: `command.envs(vars::exported())`
only adds/overrides entries on top of `Command`'s default full-
environment inheritance — it never removes a key. So `unset PATH` (or any
other exported name) only deletes rush's own internal record; a spawned
child still inherits the original OS-level value regardless, since
nothing calls `std::env::remove_var` or blocks default inheritance. Real
bash's child genuinely no longer has the variable at all. See
`docs/CAPABILITY_GAPS.md`'s C40 entry for the full writeup and fix
options under consideration.

### Fix: an unknown command name aborted the whole script instead of failing with status 127 (C37)
Running a command that doesn't resolve — a typo, something not on
`$PATH` — used to print the raw OS spawn error and abort the entire
script right there; an `echo` placed right after it never even ran. The
single most common shell-scripting mistake there is, now handled like
every other failing command.

- **New `exec::spawn_failure_status(name, &io_error)`** prints the usual
  message and returns the right POSIX status: 127 for
  `io::ErrorKind::NotFound`, 126 for anything else (permission denied, is
  a directory, …) — verified directly against real bash's own convention.
- Wired in at both `Command::spawn()` call sites — `job::spawn_pipeline`
  (the Unix job-control path, used for virtually everything on this
  shell's primary platform) and `exec::run` (command substitution, and
  the non-Unix foreground fallback). For a **standalone command**, a
  spawn failure now returns this status as an ordinary result instead of
  a hard error, so the rest of the script keeps running — including
  still triggering `set -e`, verified directly.
- Explicitly out of scope, a deliberate scope-narrowing: a command that
  fails to spawn as one stage *within* a multi-command pipeline still
  aborts the script, as before — real bash's fork-then-exec model always
  gives every stage a real (if short-lived) process, so the exec failure
  happens *inside* an already-forked, already-grouped child; Rust's
  `Command::spawn` hides that distinction and reports it atomically with
  no pid ever exposed, so there's no cheap way to synthesize a per-stage
  status and unwind an already-established process group here. The same
  limitation means backgrounding a standalone unknown command (`badcmd
  &`) no longer aborts the script (the actual fix), but has no synthetic
  pid for `$!`/`jobs` either.
- Verified directly against real bash across a standalone typo, a
  found-but-not-executable file/directory (126 vs. 127), `set -e`,
  command substitution, and backgrounding. Adds 5 new integration tests;
  full suite and clippy stay clean.

### Fix: redirects to any fd other than 0/1/2 silently collapsed onto fd 1 (C38)
`cmd 3>file`, `cmd 4<&5`, `exec 3>file` (holding a descriptor open for
later) are all ordinary shell idioms. Both of rush's redirect code paths
used to force any fd that wasn't literally 0 or 2 onto fd **1** — `cmd
3>file` silently redirected the command's *stdout*, not a real fd 3.

- **`redirect_stdio` (builtins, in-process)**: the fd-collapsing closure
  is simply gone — `StdioGuard`'s own save/restore bookkeeping was
  already keyed by plain `i32`, needing no structural change. This also
  fixed two related bugs the collapse was silently responsible for: a
  `Dup`'s *source* side collapsed exactly like its destination
  (`4>&3` botched fd 3 too, not just an unusual destination), and a
  `Read`-mode redirect to fd 1 or 2 was silently dropped entirely rather
  than merely collapsed.
- **`build_stage` (real spawned children)**: `std::process::Command` has
  no generic "set fd N" API, so fd 3+ redirects are now collected into a
  new `FdAction` list (`Open(File, fd)` / `Dup { source, dest }`, in
  their own source order) and applied via one `pre_exec` `dup2` sequence
  — the same `CommandExt::pre_exec` mechanism `job.rs` already uses for
  process groups, composing cleanly with it.
- **A real, general bug found and fixed along the way**: a freshly
  opened file's own fd is often *exactly* the fd being redirected to
  (its own "lowest available fd" landing on the requested number) —
  overwhelmingly likely for fd 3+, since 0/1/2 are essentially always
  already open but 3+ usually isn't. `dup2` on identical fds is a
  defined no-op, so the file *is* the live redirect already at that
  point — letting it drop normally (as the original code did) closed the
  very fd the redirect had just set up. Fixed by detecting this and
  `mem::forget`-ing the file instead in `redirect_stdio`;
  `build_stage`'s `pre_exec`-closure design was naturally immune (its
  captured files are never dropped in the parent before `exec` replaces
  the child's whole process image).
- **Also fixed**: the lexer's `<&n` (read-side fd duplication, e.g.
  `4<&5`) didn't parse at all — only `>&n` did. A new, shared
  `lex_lt_op` (mirroring `lex_gt_op`) fixes both places `<` is lexed.
- Verified directly against real bash across a standalone `3>file`,
  multi-hop `<&`/`>&` chains, `exec 3>file`'s permanent form, and the
  self-dup coincidence bug, for both a builtin and a real external
  command. Adds 2 lexer unit tests plus 3 integration tests; full suite
  and clippy stay clean (Windows cross-compile checked too).

### Fix: `set -- args…` / `set args…` didn't reassign positional parameters (C39)
Closes out Tier III completely, 5 of 5. The standard way to reassign
`$1`/`$2`/…/`$#` mid-script — the textbook idiom right after `getopts`
finishes — used to be rejected outright by `set` rather than actually
reassigning anything.

- **New `vars::set_positional(args)`** reassigns just the positional
  parameters, leaving `$0` untouched (unlike `set_args`, used only for a
  script's own initial argv).
- **`set_cmd` (`builtins.rs`)** now recognizes two triggers, matching
  real bash exactly: an explicit `--` (everything after is positional,
  even flag-looking text — `set -- -x` makes `$1` the literal `-x`), and
  a bare first word that isn't `-`/`+`-prefixed (`set a b c`, no `--`
  needed). A preceding flag still applies first (`set -e -- a b c`).
- **A real bug fixed along the way**: an unrecognized flag or invalid
  `-o`/`+o` name used to set an error flag and keep looping rather than
  stopping immediately — harmless before (nothing past it to trigger),
  but would have let `set -z a b` silently reassign `$1`/`$2` from `a b`
  once positional reassignment existed. Both error paths now return
  immediately, verified directly against real bash to leave `$1`/`$2`
  completely untouched.
- Verified directly against real bash across `set -- args`, the bare
  form, `set --` alone, `$0` staying untouched, the textbook
  post-`getopts` idiom, and both hard-error cases. Adds 1 new
  integration test covering all of the above; full suite and clippy stay
  clean.

### Fix: `unset` of an inherited/exported variable still reached a spawned child (C40)
Closes out Tier I completely, 10 of 10 — every tier tracked in
`docs/CAPABILITY_GAPS.md` is now done. `unset`-ing a variable that came
from the inherited process environment (`PATH`, or anything else
genuinely exported) used to only delete rush's own internal record of
it; a spawned child still inherited the original OS-level value
regardless.

- **Child environment**: `command.env_clear()` before
  `command.envs(vars::exported())`, at both spawn sites (`build_stage`
  and `exec_cmd`). Since `main.rs` already seeds every inherited
  environment variable into `vars` at startup (C36), `vars::exported()`
  is a complete, accurate picture of what a child's environment should
  be — rebuilding it from scratch, instead of layering onto `Command`'s
  default full-environment inheritance, is what makes `unset` actually
  take effect.
- **A deeper piece the environment fix alone didn't cover**:
  `Command::new(name)` resolves a bare program name using the *real*
  process environment's `PATH` at spawn time, entirely independent of
  whatever's configured for the child — so rush's own attempt to
  *locate* the command still silently succeeded via the untouched real
  environment even with the child's own environment now correct. New
  `exec::resolve_program` resolves a bare name via
  `builtins::resolve_in_path` (rush's own, `vars`-aware `$PATH` search)
  first, so `Command`'s built-in search never runs; a name that doesn't
  resolve there gets a trailing `/` appended (guaranteed to fail with
  `NotFound`, and — having a `/` — also skips `Command`'s own search),
  routing it through C37's existing not-found handling rather than a
  second error path. A direct path is left alone, preserving C37's
  126-vs-127 distinction for that case unchanged.
- **A related, broader cleanup surfaced by the same investigation**:
  half a dozen other "read this variable" call sites (`expand.rs`'s
  central `var_raw`, `arith.rs`'s arithmetic lookup, `PS1`/`PS3`/`PS4`,
  `IFS`, and a bare `cd`'s `$HOME` fallback, which previously didn't
  check `vars` at all) had the identical `vars::get(name).or_else(||
  std::env::var(name).ok())` pattern — harmless before C36, actively
  wrong after it (silently resurrecting an unset variable's original
  value). All now use `vars::get` alone. `~` (tilde expansion) keeps a
  deliberate exception: verified directly that real bash's own `~`
  *does* follow a plain, unexported `HOME=` reassignment (previously
  rush didn't honor this either) but *keeps resolving* even after
  `unset HOME`, unlike an ordinary variable.
- Explicitly out of scope: `unset PS4` still traces with rush's
  hardcoded `+ ` default rather than real bash's genuinely empty prefix
  — a different root cause (a shell-internal default bash treats as
  real, seeded-at-startup state, not environment inheritance), left for
  its own follow-up.
- Verified directly against real bash across: `unset PATH` breaking a
  bare command name while a direct path keeps working, `PATH`
  reassignment still resolving new commands, `unset IFS` reverting to
  default splitting, a bare `cd` following a plain `HOME=` reassignment
  and erroring after `unset HOME`, and `~` still resolving after `unset
  HOME` while following an assignment. Adds 2 new integration tests;
  full suite and clippy stay clean (Windows cross-compile checked too).

### Fix: `$$`, `$PPID`, and `$-` didn't expand at all (C41)
First item landed from the fresh C41–C73 comparison pass. `echo $$`
printed the literal two-character text `$$` (breaking the ubiquitous
`tmpfile=/tmp/x.$$` idiom silently), and `$PPID`/`$-` expanded to empty.

- **`$$` / `${$}`** — the shell's own pid, from `std::process::id()`,
  as new arms in `expand.rs`'s `$`-scanner and `expand_braced`'s
  special-parameter table.
- **`$PPID`** — seeded once at startup (`libc::getppid()`, Unix) as an
  ordinary non-exported shell variable, *after* the environment-seeding
  loop so a stale exported `PPID` from a parent process can't shadow the
  real value (verified: bash wins that same race the same way).
- **`$-` / `${-}`** — assembled by the new `vars::option_flags()` from
  the tracked option flags: `e` (errexit), `i` (interactive, a new flag
  set on REPL entry), `u` (nounset), `x` (xtrace). `set -o pipefail`
  contributes no letter, matching real bash.
- **A real adjacent bug found while verifying `$-`**: `set` never
  parsed clustered short flags — `set -eu` and even `set -euo pipefail`
  (the near-universal script header) errored with `not supported`.
  `set_cmd` now applies a flag word's letters in sequence, with `o`
  consuming the next word even mid-cluster — and, matching real bash
  exactly (verified directly), applies *nothing* when any flag in the
  invocation is invalid: partial application would have errexit-killed
  the shell on `set`'s own failure for `set -eu -z`.
- Verified directly against real bash (plus dash/ksh, installed and
  invoked directly) across all of the above. Adds 1 unit test and 5
  integration tests; full suite and clippy stay clean.

### Fix: POSIX bracket character classes (`[[:alpha:]]`, `[[:digit:]]`, …) misparsed as literal characters (C42)
`case 5 in [[:digit:]])` silently never matched and `ls [[:alpha:]]*`
silently matched nothing — the bracket parser only understood single
characters and `c-c` ranges, so a `[:name:]` member was read as its own
literal characters. One fix covers `case` patterns, filename globbing,
and the `${v#pat}`-family pattern-removal operators, which all share
`glob.rs`'s matcher.

- **`parse_class` (`glob.rs`)** now recognizes `[:name:]` members; the
  member list generalized from `(char, char)` ranges to a `ClassItem`
  enum (`Range` | `Named(predicate)`), so named classes mix freely with
  ordinary members (`[[:alpha:]5]`) and negate correctly
  (`[![:digit:]]`). All twelve standard names are mapped;
  `digit`/`xdigit` stay ASCII-only even in a Unicode locale, matching
  bash.
- **Edge cases probed char-by-char against real bash** rather than
  assumed: a properly-delimited unknown name (`[[:bogus:]]`) is a member
  matching nothing, not an error; an unclosed `[:` (`a[[:digit]`) hits a
  real bash quirk — bash drops the `[` member itself and keeps `:digit`
  as ordinary members (dash keeps the `[` too; rush follows bash, the
  reference).
- Verified against real bash (and dash) on identical fixture files
  across all twelve classes, mixed/negated forms, both edge cases,
  `case`, and pattern removal — byte-identical output on every pattern.
  Adds 2 unit tests and 2 integration tests; full suite and clippy stay
  clean.

### Fix: `declare -u` / `-l` / `-i` attributes were silently ignored (C43)
The `declare`/`local` flag parser only recognized `-a`/`-A`; any other
flag was misparsed as a bare variable name to declare, so `declare -u
u=hello; echo $u` printed `hello` and `declare -i n; n=2+3` stored the
literal text `2+3` — wrong values with no diagnostic.

- **New `Attrs` (`vars.rs`)**, kept in their own `ATTRS` map rather than
  on `Var`: an attribute can be declared on a name with no value yet,
  and bash keeps the variable genuinely unset in that state — `VARS`
  has no unset-but-existing representation. Transforms hook the central
  assignment paths (`set`, `set_exported`, `append_scalar`, array/assoc
  element writes), so every assignment form transforms.
- **Semantics probed against real bash case-by-case**: not retroactive;
  `-u`/`-l` displace each other across separate declarations but cancel
  when clustered (`declare -lu w=Abc` leaves `Abc` — a real bash quirk,
  matched); under `-i`, `+=` is arithmetic *addition*, an unresolvable
  name is 0, and a syntax error keeps the old value (diagnostic matched;
  bash's status-1 there is an accepted, documented simplification);
  `unset` drops attributes; `local -u` starts from its own attributes
  and restores the outer attribute state on return (local frames now
  capture prior attributes alongside prior values).
- **Flag words now cluster** (`declare -ui n`, `local -au arr=(…)`);
  unrecognized letters end flag parsing exactly as before, keeping
  `-r`/`-n`/`-x`/`-p` (C45/C62/C48) no worse than they were.
- Verified against real bash across all of the above (ksh93/zsh
  `typeset -u` agreement spot-checked). Adds 3 unit tests and 3
  integration tests; full suite and clippy stay clean.

### Fix: `trap` with a numeric or `SIG`-prefixed signal spec registered but never fired (C44)
`trap 'cmd' 15` and `trap 'cmd' SIGTERM` stored the spec verbatim, but
delivery only ever looks up the canonical bare name (`TERM`) — the trap
was silently orphaned: the signal arrived, the process took the default
disposition, and no error appeared at registration time either.

- **New `trap::normalize_signal_spec`** collapses numeric (`15` → `TERM`,
  `0` → `EXIT`), `SIG`-prefixed, and lowercase spellings (all accepted by
  real bash, verified) to the canonical bare name, backed by a 22-entry
  name↔number table. Applies to registration *and* removal (`trap - 15`).
- **Invalid specs now error** (`trap: BOGUS: invalid signal
  specification`, status 1) instead of silently registering a dead entry
  — and, matching bash exactly, don't block other specs in the same call
  from registering.
- **Listing format fixed alongside**: `trap` prints real signals
  `SIG`-prefixed with `EXIT` bare (`trap -- 'echo T' SIGTERM`), matching
  bash's own output.
- Verified against real bash across every spelling, `trap - 15` removal
  (shell dies 143), both invalid-spec cases, and listing. Adds 1 unit
  test and 3 integration tests; full suite and clippy stay clean.

### New: `readonly` / `declare -r` / `local -r` — read-only variables (C45)
POSIX-mandated special builtin, present in every comparison shell
including dash — and it wasn't just missing: `readonly x=1` was "command
not found" *and* silently lost the assignment (parsed as an argument to
the missing command).

- **Built on C43's attribute machinery**: `readonly` is a new `Attrs`
  field (so it can mark a still-unset name), enforced by a shared guard
  on every mutation path in `vars.rs` plus a refusal in `unset`. The
  builtin routes through the `local`/`declare` decl path, so array
  literals (`readonly arr=(a b)`) and `-a`/`-A` compose; `declare -r`/
  `local -r` reach the same flag, installing *after* the initializer so
  the declaring assignment itself still works.
- **Fatality split matches bash exactly** (probed case-by-case): a bare
  assignment to a readonly (`x=2`, `x+=2`, `arr[0]=c`, a readonly `for`
  variable) aborts the whole non-interactive script with status 1;
  builtin-mediated attempts (`unset`/`export x=2`/`local`/`readonly
  x=9`) fail with status 1 and continue. Bare `export x` succeeds (flag
  only); a prefix assignment (`x=2 cmd`) errors but still runs the
  command with the refused value dropped from the child env.
- **`readonly`/`readonly -p`** list in bash's own `declare -r x="1"`
  format (`-ar`/`-Ar` for arrays, bare `declare -r name` for unset).
- Verified against real bash across fourteen probe scenarios (dash for
  the POSIX abort). Adds 2 unit tests and 4 integration tests; full
  suite and clippy stay clean.

### New: `ulimit` builtin (C46), and builtins/functions inside `$(...)` actually run
`ulimit` was "command not found", blocking the ubiquitous `ulimit -n`/
`ulimit -c 0` operational-script openers.

- **`ulimit [-SH] [-a | -<letter> [limit]]`** over real `getrlimit`/
  `setrlimit`: 15 resources in bash's own units and `-a` label format,
  `unlimited`, bare `ulimit` = `-f`, soft/hard split (read soft unless
  `-H`; set both unless `-S`/`-H`), children inherit. Unknown flag →
  usage/status 2; bad number → status 1. Linux-only resources cfg-gated.
- **Broader pre-existing gap found while verifying**: a sole builtin or
  shell function inside `$(...)` was spawned as an external —
  `$(umask)`, `$(type x)`, `$(myfunc)`, `$(ulimit -n)` all failed with
  "command not found" unless an external twin existed on PATH (why
  `$(pwd)` had always *seemed* fine). Now captured in-process via the
  same fork/pipe scheme `capture_compound` uses — a real subshell,
  matching bash's `$(...)` semantics (side effects don't escape).
  Documented remaining narrowing: multi-command substitutions still run
  pipelines separately in the parent context (`$(cd /tmp; pwd)` prints
  the parent's cwd), a separate architectural item.
- Verified against real bash (`-a` dump line-identical over the
  implemented set). Adds 1 integration test; full suite and clippy stay
  clean.

### Fix: `command -p` (default-PATH form) treated `-p` as the command name (C47)
`command -p echo hi` reported `-p: command not found`. Now both halves
work: the lookup forms (`command -pv ls`, `command -p -v ls`) resolve
files through the fixed default system path (`/bin:/usr/bin`, bash's
own `confstr(_CS_PATH)` value on Linux), and the execution form pins
the program to its default-path resolution before the spawn, immune to
the shell's `$PATH` (`PATH=/nowhere; command -p ls` works). A builtin
still wins over a default-path file, same as bash. Also fixed alongside:
the synthetic trailing `/` used internally to force a clean NotFound no
longer leaks into "command not found" diagnostics. Verified against
real bash for every form; adds 1 integration test.

### Fix: `type -a` parsed `-a` as a name to look up (C48)
`type -a echo` reported `-a: not found` next to echo's single match,
never showing shadowed alternatives. New `classify_all` lists every
match — alias/keyword/function/builtin in precedence order, then every
`$PATH` directory's hit in order (duplicates not deduped, matching
bash byte-for-byte). Flags cluster (`type -at`). Accepted narrowing:
bash prints a function's full body under `type -a`; rush keeps its
one-line form. Verified against real bash; adds 1 integration test.

### New: `typeset` as a synonym of `declare` (C49)
ksh93 has *only* `typeset` (no `declare`); bash/zsh accept both.
Registered at the decl-word dispatch, the builtin dispatch (same
`declare_from_decls`), and the name table — so the C43 attribute
transforms, both array forms, and C45's `-r` all work under `typeset`
with zero additional code (verified against ksh93/zsh directly). Adds
1 integration test.

### New: `set -C` (noclobber) and the `>|` override (C50)
`set -C` was rejected outright and `>|` didn't lex. Now: a plain `>`
under noclobber refuses to truncate an existing *regular* file (devices
like `/dev/null` stay writable, per POSIX and verified against bash);
`>|` overrides; `>>` is exempt; `&>` honors it too (bash-verified); `C`
appears in `$-`. Enforcement is centralized in `exec::open_write`, so
the explicit-fd form (`2>| file`) rides along free. Inherited (not new)
divergence, documented: rush treats a failed redirect open as fatal
where bash fails the one command and continues. Adds 1 integration
test.

### New: `set -n` (noexec) and `rush -n` syntax-check mode (C51)
The standard `sh -n script.sh` linting idiom: parse everything, report
syntax errors (status 2, same as bash), run nothing (status 0 when
clean). A `NOEXEC` flag checked at `exec::run_andor` — the choke point
every command funnels through — with `rush -n` pre-setting the same
flag `set -n` uses. Matches bash's subtleties: mid-script `set -n` is
one-way (the `set +n` after it never executes), and an interactive
shell ignores it entirely. `n` appears in `$-`. Adds 2 integration
tests.

### New: `set -o` long option names, and bare `set -o`/`set +o` listing (C52)
`set -o errexit`/`nounset`/`xtrace`/`noclobber`/`noexec` now map to the
same flags as the short forms (getting C41's validate-then-apply
rollback for free), and a bare `set -o` lists options in bash's own
table format (byte-identical over the tracked six) while `set +o`
emits directly re-runnable lines — the `saved=$(set +o); eval "$saved"`
round-trip works. Unknown `-o` names stay a hard error. Adds 1
integration test.

### New: `trap ERR` fires (C53), and `!` pipeline negation exists at all
`trap 'cmd' ERR` registered but never fired. It now fires on exactly
errexit's condition — a reached, non-negated final command failing
outside an `if`/`while` condition — whether or not `set -e` is on, and
before the errexit exit when it is. The handler sees the failing status
as `$?`, restored afterward regardless of what the handler ran. Not
fired inside functions (bash's no-`errtrace` default, documented).

Found while landing it: **`! cmd` (POSIX pipeline negation) didn't
parse at all** — and it interacts directly with ERR/errexit (a negated
pipeline is exempt from both). Implemented together: leading `!`
(repeatable) on a pipeline, negated status in run and capture paths
(`$(! true; echo $?)` → 1), exemption threaded through the existing
errexit signal. Verified against bash across fourteen scenarios; adds
2 integration tests.

### New: `${PIPESTATUS[@]}` — per-stage pipeline exit statuses (C54)
Always expanded empty before. Now recorded at the reap point
(`job::wait_pgid`'s per-stage vector, the same one pipefail consumes)
plus a one-element array for every single-stage command (builtins,
compounds, assignments, `cmd &`) — bash updates it for every command,
verified. Matched subtleties: reading it twice shows the first echo's
own `(0)`; `! false` records the un-negated `(1)`; pipefail doesn't
distort the per-stage values; all existing array read forms compose.
Not set inside `$(...)` (a bash substitution is a subshell whose
PIPESTATUS never escapes). Adds 1 integration test.

### New: `[[ ]]` extended test construct (C55)
The largest single gap in the capability document: rush had no
`[[`/`]]` at all — `[[ foo = foo ]]` was command-not-found, and `<`
inside one opened a file. Now implemented in all three layers: a
dedicated lexer mode for the interior (`<`/`>` are comparison words,
`&&`/`||`/`( )` operators, multi-line works, quoting structure
preserved), a genuinely recursive parser production, and an evaluator
whose operands never word-split or glob — `x=; [[ $x = foo ]]`,
`x="a b"; [[ $x = "a b" ]]`, and `[[ $x = *.txt ]]` all behave.
Pattern `==`/`!=` follows bash's per-part quoting rule; `<`/`>` compare
lexicographically; `-eq…-ge` are full arithmetic; `-nt`/`-ot`/`-ef`
compare files; malformed expressions abort with status 2 like bash.
`=~` is recognized and deferred to C56. Verified byte-identical against
bash across 32 scenarios; adds 1 parser unit test + 1 comprehensive
integration test.
