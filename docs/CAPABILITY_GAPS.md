# rush capability gaps — vs. dash, bash, ksh93, zsh, fish

A cross-shell capability assessment, verified against source in `src/` (not
README/CHANGELOG claims alone, which can drift) rather than a fresh
install-and-test pass of each comparison shell. Other-shell behavior is drawn
from each shell's documented feature set (POSIX.1-2018 §2, dash(1), bash(1),
the ksh93 reference, zshall(1), fish's docs).

This is a distinct gap set from the one in the (now fully closed) `rushgaps.md`
that drove G1–G11 — those were about *packaging and daily-driver readiness*;
this one is about *language and builtin coverage relative to other shells*.
IDs here are prefixed `C` (capability) to avoid colliding with the old `G`
series.

Items marked **(tracked)** are already named somewhere in this repo's own
docs (`ARCHITECTURE.md`, `CHANGELOG.md`, doc comments) — re-surfaced here with
the cross-shell context that shows why they matter, not newly discovered.

**Bottom line:** rush's actual scope today is closest to **dash** — a solid,
mostly-POSIX execution core (real pipes, real job control, real forked
subshells) with almost none of the bash/ksh/zsh-family conveniences layered
on top. Tier I's original 6 items are done; three more (C35, a real
quoting bug; C37, an unknown-command-aborts-the-script bug; C38, redirects
to fd 3+ silently landing on fd 1) turned up while closing out Tier II,
which is now down to a single open item (C36) — `local`, `getopts`,
`command`/`type`/`hash`, `wait` (with its own prerequisite, `$!`),
`source`/`.`, `eval`, `exec`, and `umask` all landed alongside
`read`/`printf`/`shift`. C36 (a PATH-visibility bug in
`command`/`type`/`hash`) turned up while closing out `source`; C37
while closing out `eval`; C38 while closing out `exec`. Tier III
(`set -euo pipefail`, the header nearly every production shell script
opens with) is now half done — `-e` (already done), `-u` (C18), and
`-o pipefail` (C19) all work; only `-x` (C20) is left of the header itself.

---

## Comparison matrix

A cross-section, not the full 38 below — enough to place rush relative to a
strict POSIX shell (dash), the bash family, and the interactive-first shells
(zsh, fish). ✅ full · 🟡 partial/simplified · ❌ not implemented · — not
applicable to that shell's own model.

| Capability | rush | dash | bash | ksh93 | zsh | fish |
|---|---|---|---|---|---|---|
| Real pipes / job control / forked subshells | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `#`/`##`/`%`/`%%` param. expansion | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| `read` / `printf` / `shift` / `getopts` | ✅† | ✅ | ✅ | ✅ | ✅ | 🟡 |
| `local` function-scoped vars | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `wait` / `disown` | 🟡‡ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `source` / `.` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `eval` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `exec` (process replacement) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `umask` | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `set -e` / `-u` / `-o pipefail` | 🟡§ | 🟡 | ✅ | ✅ | ✅ | — |
| Indexed / associative arrays | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Brace expansion `{a,b,c}` | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Compound as one pipeline stage | 🟡* | ✅ | ✅ | ✅ | ✅ | ✅ |
| Traps beyond EXIT/INT firing | 🟡 | ✅ | ✅ | ✅ | ✅ | — |
| Context-aware completion | ❌ | — | 🟡 | 🟡 | ✅ | ✅ |
| History autosuggestion | ❌ | — | ❌ | ❌ | 🟡 | ✅ |
| Native Windows job control | ❌ | — | — | — | — | 🟡 |

\* Done for the interactive/script job-control path; a compound as one stage
among several *inside* a `$(...)` substitution, or on non-Unix, still errors.

† All four are done, with narrower caveats: `read` (with `-r` and `$IFS`
splitting) and `printf` (sans `%e`/`%f`/`%g`) are otherwise complete;
`shift`/`getopts` are full.

‡ `wait` (`pid`/`%job`, or none) is done, along with its `$!` prerequisite;
`disown` remains missing.

§ `-e`, `-u`, and `-o pipefail` are all done; `-x` (C20) is still missing.

---

## Summary counts

- **Tier I — correctness/POSIX risk:** 9 (6 done)
- **Tier II — missing standard builtins:** 12 (11 done)
- **Tier III — scripting-safety idioms:** 4 (2 done)
- **Tier IV — bash/ksh/zsh language parity:** 10
- **Tier V — interactive UX:** 3

---

## Tier I — Correctness & POSIX risk

These don't just lack a feature — a script that assumes them can silently do
the wrong thing under rush instead of erroring, which is the worse failure
mode.

### C1 — Suffix/prefix parameter expansion: `${v%pat}` `${v%%pat}` `${v#pat}` `${v##pat}` ✅ done
POSIX-mandated; present in dash, bash, ksh93, zsh. This is the standard,
portable way to strip an extension or a path (`${file%.txt}`,
`${path##*/}`) without spawning `basename`/`sed`, and it's everywhere in real
scripts. **Effort: M.**

Implemented: `#`/`%` remove the shortest matching prefix/suffix, `##`/`%%`
the longest, using the same glob matcher `case` patterns use
(`strip_prefix_pattern`/`strip_suffix_pattern` in `expand.rs`). No colon
form — bash doesn't define one for this family either.

### C2 — `for name; do …; done` should iterate `"$@"` ✅ done
POSIX-mandated shorthand, present in dash/bash/ksh/zsh. Omitting the `in`
clause used to leave rush's word list empty, so the loop body silently never
ran — not an error, just quietly wrong. **Effort: S.**

Implemented: the parser now records whether an `in` clause was present at
all (`Compound::For`'s `has_in`), distinct from an *explicit* `in` with zero
words (still a real empty list). No `in` → iterate `vars::args()` (`"$@"`).

### C3 — Compound command as one stage of a larger pipeline: `(cmd) | grep x` (tracked) ✅ done (job control path)
Present in every comparison shell. Rush could already capture a *lone*
compound via `$(...)` and run one as an entire pipeline by itself, but a
compound as one stage among several in a real pipe used to hard-error.
Needed File-based pipe plumbing for a forked compound stage, not the
`Stdio`-based approach `build_stage` uses for external commands.
**Effort: L.**

Implemented for the interactive/script job-control path (`job::spawn_pipeline`,
Unix only): `Pipeline.commands` is now `Vec<Stage>` (`Stage::Simple` or
`Stage::Compound`); a compound stage forks, wiring stdin/stdout via `dup2`
from real fds (`File`, not `Stdio` — a forked child needs something
introspectable to `dup2` from), and joins the pipeline's process group like
any exec'd stage. `(cmd) | grep x`, a compound as the first/middle/last
stage, and forked-subshell isolation (G10) all verified working even when
piped. **Not yet extended** to the capture path (`$(...)`) — a compound as
one stage among several *inside* a substitution, or on non-Unix (no `fork`
there at all), still errors clearly rather than silently misbehaving.

### C4 — `set -e` doesn't match bash/POSIX's exact rule (tracked) ✅ done
Correct in dash, bash, ksh, zsh: a failing command is exempt from errexit
unless it's positionally last in an `&&`/`||` list. Rush's simplified rule
fired on any job's *final* nonzero status instead — `set -e; false && true`
used to exit under rush but not under real bash. A script tested against
bash's actual semantics could abort earlier than its author intended.
**Effort: M.**

Implemented: `run_andor`/`run_job`/`exec_list_impl` (`exec.rs`) now return
whether the textually-last pipeline in a job's `&&`/`||` chain actually ran
(`last_ran`), not merely whichever pipeline happened to run last under
short-circuiting. `errexit` now only fires when a *reached* final pipeline
fails — `set -e; false && true` survives, `set -e; true && false` exits,
matching bash exactly. `if`/`while` conditions remain separately exempt via
the pre-existing `exec_cond` path, unaffected by this change.

### C5 — Real `$IFS`-driven word-splitting ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh. Rush hardcoded ASCII whitespace
as the split set. `IFS=','`-style field splitting — a standard, portable
parsing technique — used to silently do the wrong thing rather than honoring
the variable. **Effort: M.**

Implemented (`expand.rs`'s new `Ifs` type and rewritten `Splitter`): unset
`$IFS` still defaults to space/tab/newline; an *explicit* empty `IFS=`
disables field splitting entirely (matching POSIX, not merely "no-op
default"); any other value splits on exactly its characters, with
space/tab/newline within it forming the collapsing "whitespace" class (runs
collapse, no empty fields) and every other character forming "non-whitespace"
delimiters where *each occurrence* opens a field on its own, even empty
(`IFS=,` on `a,,b` is three fields) — matching bash's asymmetry that a
*leading* delimiter produces a leading empty field but a single *trailing*
one at the very end does not. `$*`/`${*}` now join with `$IFS`'s first
character (space if unset, nothing if IFS is empty) instead of a hardcoded
space; `$@` is unaffected, matching bash.

### C6 — `test`/`[` logical combinators `-a` / `-o` (tracked) ✅ done
POSIX-mandated, present in dash/bash/ksh/zsh (bash discourages but still
ships them). Lower risk than the rest of this tier — absence is a hard usage
error, not silent wrongness — but still a real portability gap for scripts
targeting strict POSIX sh. **Effort: S.**

Implemented: `test_eval` (`builtins.rs`) is now a small recursive-descent
parser (`test_or` → `test_and` → `test_not` → `test_primary`) instead of a
fixed-arity match, matching bash's actual grammar and precedence — `-a`
binds tighter than `-o` (`1 = 2 -o 1 = 1 -a 1 = 2` groups as `(1 = 2) -o ((1
= 1) -a (1 = 2))`), and `!` negates only the next primary, not a whole
trailing `-a`/`-o` chain (verified against real bash directly). All prior
single-expression forms (`-z`, `a = b`, `! EXPR`, a lone string) are
unaffected.

### C35 — Backslash-escaped `$` inside double quotes isn't literal (tracked)
POSIX-mandated; present in dash/bash/ksh/zsh: inside `"..."`, `\$` shall
produce a literal `$` (suppressing expansion of whatever follows), same as
`\"`/`\\` are already handled. Rush currently drops the backslash but still
expands the parameter anyway — `echo "\$?"` prints the exit status instead
of the literal text `$?`, and `echo "\$FOO"` prints `$FOO`'s *value*
instead of the literal text `$FOO`. Silent wrongness, not an error, so it
fits this tier; found while verifying C13's `$!` against real bash (not
specific to `$!` — reproduces for `$?`, a plain `$FOO`, everything).
**Effort: S.**

### C37 — An unknown command name aborts the whole script instead of failing with status 127 (tracked)
POSIX-mandated in every comparison shell here: running a command that
doesn't resolve — a typo, something not on `$PATH` — prints an error to
stderr (bash: `command not found`) and continues the script with `$?` set
to 127. Rush instead prints the raw OS spawn error (`No such file or
directory (os error 2)`) and **aborts the entire script right there** — an
`echo` placed right after the bad command never even runs. Found while
diffing `eval "nonexistent_cmd"` against bash (C15), but reproduces for any
top-level mistyped command — not specific to `eval` at all, and arguably
the highest-impact item in this tier, since it fires on the single most
common shell-scripting mistake there is. **Effort: S** — `build_stage`'s
spawn-failure path (`exec.rs`) needs to turn a not-found spawn error into
an ordinary exit-127 result instead of the `Result::Err` it propagates
today, matching how every other non-zero exit status is already handled.

### C38 — Redirects to any fd other than 0/1/2 silently collapse to fd 1 (tracked)
POSIX-mandated: `cmd 3>file`, `cmd 4<&5`, `exec 3>file` (holding a
descriptor open for later) are all ordinary, if less common, shell idioms.
Rush's whole redirect machinery — both `redirect_stdio` (builtins) and
`build_stage` (real spawned children) — collapses any `fd` that isn't
literally `0` or `2` into fd **1** (`target_fd`'s `_ => 1` arm), so `cmd
3>file` today silently redirects the command's *stdout*, not a real fd 3.
Silent wrongness, not an error. Found while implementing `exec` (C16),
which is the first place this blocks a headline idiom (`exec 3>file`)
rather than being an edge case, but it's general — reproduces for any
command, builtin or external. **Effort: M** — needs real per-fd tracking
(open the target, `dup2` onto the actual requested fd) in both code paths,
plus, for `exec`'s permanent form specifically, a way to keep an arbitrary
fd open across the rest of the script rather than just 0/1/2.

---

## Tier II — Missing standard builtins

POSIX-mandated in every comparison shell here. Each one blocks a whole
category of otherwise-ordinary scripts outright, rather than just being an
inconvenience.

### C7 — `read` ✅ done
Arguably the single highest-value missing builtin. Without it: no `while
read line; do …; done < file`, no prompting for input, no parsing
`IFS`-delimited fields from a line. Blocks an entire class of everyday
scripts on its own. **Effort: M.**

Implemented: `read [-r] [name...]` (`builtins.rs`), reading one logical line
directly off fd 0 a byte at a time (never over-consuming past the newline,
so a loop of calls sharing one fd — `while read line; do …; done < file` —
picks up exactly where the last call left off) and splitting it into fields
on `$IFS`, using the same whitespace/non-whitespace classification and
trailing-delimiter asymmetry as word-splitting (C5). A name past the last
field gets `""`; the *last* name absorbs any extra fields verbatim (original
separators intact), not re-split. Without `-r`, `\<newline>` is a line
continuation and `\<char>` escapes a separator; `-r` disables both. Exit
status is 0 for a newline-terminated line, 1 on EOF (even if a trailing
unterminated partial line was still read and assigned) — all verified
against real bash directly across two dozen field-splitting/escaping/EOF
scenarios.

Landing this exposed a real, separate pre-existing gap it needed to be
useful for its headline idiom: rush's parser silently dropped any redirect
trailing a compound command's close (`while …; done < file`, `{ …; } > log`)
— the tokens were simply left to become a stray no-op command afterward, so
`done < file` never wired the file to fd 0 at all (a lone `while read …`
with no pipe would silently read the shell's real stdin instead — a hang in
a script, not an error). Fixed alongside `read`: the parser now attaches
trailing redirects to a compound (new `RawCompound`/`exec::CompoundStage`),
applied for the compound's whole duration via the same `redirect_stdio`
(renamed from `redirect_builtin_stdio`, since it's no longer builtin-only)
a lone builtin already used — including a compound as one stage of a real
pipeline (`job::spawn_compound_stage`) and a compound captured via
`$(...)` (`capture_compound`), with the same "explicit redirect overrides
implicit pipe/capture wiring" precedence `build_stage` already uses for
simple commands. A here-doc trailing a compound's close (`while …; done
<<EOF`) works the same way, fed through a `CLOEXEC`-marked pipe from a
background thread — the fix for a real deadlock found while testing this:
without `CLOEXEC`, a real child spawned from the compound's body before the
writer thread finished would inherit its own copy of the write end, so the
reader never saw EOF.

### C8 — `printf` ✅ done
The portable, correct way to emit formatted output — real scripts avoid
`echo` for exactly this reason, and rush's own `echo` has no `-e` at all,
making this more urgent than usual. **Effort: M.**

Implemented (`builtins.rs`'s `printf_cmd` and `printf` submodule): `%s`/`%b`
(string, `%b` also processing backslash escapes in its argument),
`%d`/`%i`/`%o`/`%u`/`%x`/`%X` (integer, decimal/octal/unsigned/hex — a
negative number reinterpreted as unsigned, matching real `printf`'s two's
complement behavior), `%c`, `%%`, the `-`/`0`/`+`/` ` flags, and a width
and/or `.precision`. Format-string escapes (`\n`/`\t`/`\\`/`\a`/`\b`/`\f`/
`\r`/`\v`/`\NNN` octal) are resolved once, up front. If there are more
arguments than the format consumes, the whole format repeats against the
rest (`printf "%s-%d\n" a 1 b 2 c` → `a-1`, `b-2`, `c-0`), matching real
bash exactly; missing arguments mid-format default to `""`/`0` rather than
erroring. Not yet implemented: `%e`/`%f`/`%g` (floating point) and `*`
(width/precision taken from an argument) — narrower, separate remaining
pieces (rush's arithmetic is integer-only, so the former is lower-value
here than in a shell with float support).

### C9 — `shift [n]` ✅ done
The missing piece connecting positional parameters and `case` (both already
supported) into the ubiquitous `while [ $# -gt 0 ]; do case $1 in …; esac;
shift; done` argument-parsing loop. **Effort: S.**

Implemented (`vars::shift`, `builtins::shift_cmd`): drops the first `n`
(default 1) positional parameters. A negative or non-numeric `n` is a hard
usage error (status 1, with a message); `n` greater than `$#` fails
*silently* — no message, just status 1 — matching a real bash quirk
verified directly: that's the everyday way an argument-parsing loop notices
it's out of arguments, so bash doesn't warn about it the way it does for a
genuinely malformed count.

### C10 — `local` (function-scoped variables) ✅ done
Near-universal extension (dash, bash, ksh, zsh); fish scopes by default.
Right now every rush function shares the caller's entire variable
namespace — a function's own `i=0` silently clobbers the caller's `i`.
Functions already work; using them safely for anything nontrivial doesn't.
**Effort: M.**

Implemented (`vars::push_local_frame`/`pop_local_frame`/`declare_local`,
`builtins::local_cmd`): each function call gets a stack frame recording,
for every name `local` shadows *in that call*, whatever the name was before
(or its absence) — restored automatically when the call returns
(`exec::call_function`), so nesting falls out for free: an inner call's own
`local x` shadows further and restores to the *enclosing* call's local
value on return, not the top-level one (verified against real bash
directly). A bare `local x` (no `=value`) leaves `x` genuinely unset within
the function — `${x-default}` inside it sees it as unset, not merely set to
`""` — matching bash exactly. `local` outside any function call is a usage
error and does not fall through to setting a plain global variable.

### C11 — `getopts` ✅ done
The portable way to parse `-a`, `-b value`, combined short flags. Without
it every rush script hand-rolls option parsing from scratch. **Effort: M.**

Implemented (`builtins::getopts_cmd`): `-a`, `-b value` (from the rest of
the same word or the next one), and combined short flags (`-ab` = `-a
-b`) — `$OPTIND` (1-based index of the next word) stays put while still
inside a combined-flag word, advancing only once it's exhausted (an
internal `(optind, char_pos)` cursor tracks the within-word position,
mirroring bash's own private state — not a shell-visible variable). A
leading `:` in `optstring` enables silent mode (`name` set to `?`/`:` with
`$OPTARG` the offending character, no diagnostic) instead of the default
(a diagnostic, `name` set to `?`, `$OPTARG` unset). `$OPTIND`/`$OPTARG` are
ordinary shell variables; resetting `OPTIND=1` starts a fresh pass. A lone
`--` or the first non-option word ends option processing without being
consumed. All verified against real bash directly, including the full
`while getopts ...; do case $opt in ...; esac; done; shift $((OPTIND-1))`
idiom this and `shift` (C9) together unlock.

### C12 — `command` / `type` / `hash` ✅ done
`command -v foo` is the standard portable existence check used constantly
in install scripts and shell-form Makefiles. Without it, scripts fall back
to fragile `which`-based checks. **Effort: S–M.**

Implemented (`builtins.rs`'s `command_cmd`/`type_cmd`/`hash_cmd`/`Kind`
classifier, plus `exec::command_bypass`): `command -v`/`-V name...`
describes how each name would resolve — alias, function, builtin, or
`$PATH` executable, in that precedence order (`-v`: terse, the standard
existence-check form; `-V`/`type`: a human-readable sentence) — without
running anything, failing if none resolve. `type` additionally recognizes
shell keywords and has a `-t` form for just the one-word classification
(`function`/`builtin`/`keyword`/`file`/`alias`). Plain `command name
[args...]` (no `-v`/`-V`) actually *runs* `name`, bypassing a shadowing
shell function of the same name — the headline reason `command` exists —
handled at the exec dispatch level so it composes with real redirects and
external spawns; a function's own reconstructed source (as bash prints
after "is a function") isn't reproduced, a documented narrowing since rush
functions store parsed `CommandList`, not original source text.
`hash` is a genuine stub (rush never caches `$PATH` lookups, so there's
nothing to actually hash): `-r` and a bare call are accepted no-ops,
`hash name` at least reports via exit status whether it currently
resolves. All verified against real bash directly.

### C13 — `wait [pid|%job]` ✅ done
A surprising gap given how much job-control machinery already exists (`&`,
`fg`, `bg`, `jobs`, `kill`) — `job.rs` already tracks pids/pgids, so this
mostly needs to expose `waitpid` on a selected job. `cmd & ; wait` is the
entire point of backgrounding something you need later. **Effort: S.**

Implemented (`job::wait_cmd`/`wait_all`/`wait_job_pgid`/`wait_one`): with no
operands, blocks until every job this shell knows isn't finished has
finished (always succeeding, POSIX's rule); with one or more `pid`/`%job`
operands, blocks on each in turn and reports the *last* one's own exit
status. A pid/job already reaped — by an earlier `wait`, by `fg`, or by the
interactive prompt's own background polling — still reports its remembered
status rather than erroring, via a new `REAPED: HashMap<pid_t, i32>` that
`update_by_pid` populates whenever a tracked pid actually exits (verified
against a real bash quirk: waiting twice on the same pid still works).

Landing this exposed `$!` (the most recently backgrounded job's pid) was
entirely unimplemented — a real prerequisite, since `p=$!; wait $p` is the
standard way to capture a specific background job to wait on later. Added
(`vars::last_bg_pid`/`set_last_bg_pid`, wired into `job::run_background`
and `expand.rs`'s `$`-scanner): `$!` is the *last* stage's own pid (not the
pgid) for a piped background job, matching bash exactly; unset until
something's been backgrounded. Also fixed along the way: `run_background`'s
`[id] pgid` announcement was printed unconditionally, but real bash (and
rush's own `job_control_enabled` flag, already meant to track exactly this)
only shows it interactively — a non-interactive script now prints nothing
there either, matching bash.

Found but **out of scope** here: backslash-escaping a `$` inside double
quotes (`"\$?"`, `"\$FOO"`) doesn't produce a literal `$` in rush the way
POSIX requires — the backslash is dropped and the parameter still expands.
Pre-existing, general (not specific to `$!`), and unrelated to job control;
worth its own future item.

### C14 — `source` / `.` — ✅ done
Rush already had the machinery — it sources `~/.rushrc` internally via its
own `run_source` helper — but exposed none of it as a user-invokable
command. Splitting a script into a reusable library via `. lib.sh` is one
of the most basic shell idioms there is.

Added `exec::source_file` (`.`/`source` are exact synonyms, both wired to
the same `source_cmd` builtin): runs the file's commands in the *current*
environment, no new variable scope, matching every verified bash behavior —
a bare filename is searched on `$PATH` for a *readable* file (checking the
file, not the execute bit, unlike `command`'s executable-only search); with
no extra args the caller's own positional params show through unchanged;
extra args temporarily replace them and are restored afterward; `return`
inside the sourced file ends only the sourcing (the caller keeps running);
`break`/`continue` are *not* consumed and propagate transparently to an
enclosing loop back in the calling context; a missing file fails with
status 1.

Found and fixed along the way: the new `resolve_source_path`'s first draft
read `$PATH` via `std::env::var_os`, the raw OS process environment — so a
plain (or even `export`ed) in-shell `PATH=$PATH:dir` assignment was
invisible to it, since rush only threads exported vars into a *spawned
child's* environment (`exec::build_stage`'s `command.envs(...)`) rather than
syncing them back into this process's own env. Switched to the same
`vars::get("PATH").or_else(|| std::env::var("PATH").ok())` fallback
`expand.rs` already uses for `$PATH` expansion, so `source`'s own PATH
search now sees the shell's actual PATH. The same root-cause bug still
affects `command -v`/`type`/`hash` (C12, already shipped) — left alone here
as out of scope for this item; worth its own future fix.

### C36 — `command -v`/`type`/`hash` don't see in-shell `PATH` changes (tracked)
Found while fixing C14's own PATH search (see above): `builtins::resolve_in_path`
(backing `command -v`/`command -V`/`type`/`hash`) and `completion.rs`'s
`$PATH`-scanner all call `std::env::var_os("PATH")` directly — the *real*
OS process environment — rather than the shell's own `PATH` variable. A
script that does a plain (or even `export`ed) `PATH=$PATH:dir` assignment
and then runs `command -v tool`/`type tool`/`hash tool` for something in
`dir` gets a false "not found", even though actually *running* `tool`
works fine (spawning goes through `exec::build_stage`, which correctly
threads exported vars into the child's environment). Silent wrongness for
any script that extends `PATH` before checking a command's availability.
**Effort: S** — same one-line fix as C14's (`vars::get("PATH").or_else(||
std::env::var("PATH").ok())`), applied at each of the two remaining call
sites.

### C15 — `eval` ✅ done
Needed for constructing and running commands dynamically. Rush's
command-substitution path already re-parses and re-runs strings internally
— `eval` reuses that exact mechanism, exposed as a builtin.

Added `exec::eval_cmd`/`builtins::eval_cmd`: joins its arguments with a
single space, parses the result, and runs it in the *current* shell —
unlike `source` (C14), `eval` establishes no scope of any kind. There's no
filename/PATH search and no positional-parameter swap, and — verified
directly against real bash — a `return`/`break`/`continue` inside the
evaluated text is *not* consumed; it propagates straight to whatever
function/loop is actually enclosing the `eval` call, exactly as if the text
had been typed inline. No arguments (or all-empty ones) is a no-op that
succeeds; a parse error fails with status 2, matching rush's own existing
convention for a top-level syntax error.

Found but **out of scope** here, and not specific to `eval`: running any
unknown command name anywhere in a rush script — not just inside `eval` —
prints a raw OS error and *aborts the entire script* instead of reporting
exit status 127 and continuing, the way every POSIX shell does. Discovered
while diffing `eval "nonexistent_cmd"` against bash, but reproduces with a
plain top-level typo too. Tracked separately as C37 — likely higher-impact
than most of this tier, since it affects *any* mistyped command, not one
particular feature.

### C16 — `exec` ✅ done
Two standard idioms currently impossible in rush: `exec cmd` (process
replacement — common in container entrypoints) and `exec 3>file` (holding a
descriptor open for the rest of the script).

Added `exec::exec_cmd` (Unix only, registered as a normal builtin so its
redirects flow through the existing `run_builtin_foreground`/
`redirect_stdio` machinery unchanged):
- **With a command** (`exec cmd args...`): replaces the current process
  image via `execvp` (`std::os::unix::process::CommandExt::exec`) — no
  fork, so on success this never returns; it inherits whatever fds 0/1/2
  the caller's own redirects already left them as, plus the shell's
  exported environment, exactly like a normal spawned child. On failure
  (command not found) — verified directly against real bash — a
  non-interactive shell exits immediately with status 127 (the *whole
  script* stops right there, not just this command), while an interactive
  one just reports 127 and keeps running with its redirects restored as
  normal.
- **With no command** (bare `exec`, or `exec` followed only by redirects,
  e.g. `exec > file`, `exec 0<file`): a no-op that always succeeds, except
  the redirects that `run_builtin_foreground` already applied are made
  *permanent* — a new `StdioGuard::disarm` closes the saved originals
  instead of restoring them on drop, the one case where a builtin's
  redirects are meant to outlive the call.

Found but **out of scope** here, and not specific to `exec`: rush's
redirect machinery (`redirect_stdio` *and* `build_stage`, i.e. builtins and
real spawned children alike) only ever wires up fd 0/1/2 — any other
target `fd` (`cmd 3>file`, `exec 3>file`) silently collapses to fd 1
(`target_fd`'s `_ => 1` arm) instead of actually opening fd 3. Pre-existing
across the whole shell, not introduced by `exec` — just the first item
where it blocks a headline idiom (`exec 3>file` holding an arbitrary
descriptor open) rather than being an edge case. Tracked separately as C38.
**Effort: M.**

### C17 — `umask` ✅ done
Needed by any script that creates files or directories with specific
permissions — previously no way to influence default permissions from
inside a rush script at all.

Added `builtins::umask_cmd` (Unix only): a real `libc::umask()` call, so
it actually changes the permissions every subsequent file/directory this
process (or anything it execs/spawns) creates — not just a shell-internal
display value. No argument reports the current mask (plain 4-digit octal,
e.g. `0022`, or `u=rwx,g=rx,o=rx`-style with `-S`, both verified directly
against real bash); reading it without changing it means setting it right
back, since `umask()` itself only ever *sets*, returning the previous
value. One argument sets it from an octal string; an out-of-range or
malformed mode fails with status 1 without touching the mask. Symbolic
*setting* (`umask u=rwx,g=rx,o=`) isn't supported, only octal — the
overwhelming common case in real scripts, matching this item's **Effort:
S** scope.

---

## Tier III — Scripting-safety idioms

The `set -euo pipefail` header is close to universal in production shell
scripts. Rush currently implements one third of it, and a simplified third
at that.

### C18 — `set -u` (nounset) ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh. Referencing an unset or
misspelled variable used to expand silently to an empty string — `-u`
turns that into an immediate, loud error instead.

Added `vars::set_nounset`/`nounset` (mirroring `errexit`'s own thread-local
flag) plus two new checked lookups in `expand.rs` — `var_lookup_checked`,
`arg_checked` — used everywhere a plain value is needed: `$name`/`${name}`,
`${#name}`, and the `#`/`##`/`%`/`%%` pattern-removal operators, plus
numbered positional parameters (`$1`, `${10}`). All verified directly
against real bash, including the exact exemptions: the `:-`/`:=`/`:+`/`:?`
default/alternate family defines its own unset-variable handling and stays
untouched (`:?` still fires its own, different error either way); `$@`/
`$*`/`$#`/`$?`/`$$` are always considered set, even with zero positional
parameters, while a specific numbered one (`$1`, `${10}`) *is* still
subject to the check when it doesn't exist; a set-but-empty variable is
fine (the test is "unset", not "empty"); `set +u` turns it back off.

One caveat, shared with the pre-existing `${VAR:?msg}` error rush already
had: bash exits a non-interactive shell with status 127 for an unbound
reference specifically, but rush's exits with 1 like most of its other
expansion errors — the script still aborts right there either way (the
part that actually matters), just with a different code. Not introduced
by this change; not worth its own tracked item given how minor it is next
to `set -u` actually existing at all.

### C19 — `set -o pipefail` ✅ done
Present in bash/ksh/zsh (notably *not* dash — bash-family parity, not
strict POSIX). Without it, a pipeline's exit status was always just its
last stage's: `false | true` "succeeds," masking real failures anywhere
earlier in the chain.

Added `vars::set_pipefail`/`pipefail` (mirroring `errexit`/`nounset`'s own
thread-local flags), `set`'s new `-o`/`+o` two-token parsing (`set -o
pipefail`, `set +o pipefail`; an unrecognized `-o` name is an error, not a
silent no-op), and a shared `exec::pipeline_status` helper called from both
places a pipeline's stages get reduced to one exit code: the non-Unix/
capture runner (`exec::run`, used for both a non-Unix foreground pipeline
*and* `$(...)` command substitution — pipefail applies inside a
substitution too, verified directly) and the Unix job-control runner
(`job::wait_pgid`, which now tracks every stage's own exit code by position
instead of only the last). Without pipefail, still just the last stage's
status; with it, the *rightmost* non-zero status among all stages (not
"the first failure", not "any failure" — verified directly against real
bash with a distinct exit code at each position to disambiguate), or 0 if
every stage succeeded.

### C20 — `set -x` (xtrace)
POSIX-mandated; present in dash/bash/ksh/zsh. The standard way to debug a
misbehaving script — echoes each command before it runs. Rush has no
debugging aid like this at all today. **Effort: S–M.**

### C21 — Trap signals beyond `EXIT`/`INT` actually firing (tracked)
`TERM`/`HUP` are POSIX-mandated; `ERR`/`DEBUG` are bash/ksh/zsh extensions.
Rush's `trap` builtin will happily *register* a handler for any name, but
only ever *fires* `EXIT` and `INT` — a script trapping `TERM` for graceful
shutdown (the standard container/daemon pattern) silently never gets
called. **Effort: M.**

---

## Tier IV — Bash/ksh/zsh language parity

Not POSIX-mandated, but rush's own README calls it "bash-compatible" —
these are the extensions real bash scripts lean on most.

### C22 — Indexed arrays: `arr=(a b c)`, `${arr[@]}`, `${#arr[@]}`
Present in bash/ksh93/zsh (not POSIX sh/dash — bash-family parity, not
POSIX parity). Heavily used in modern bash scripts; currently fails outright
rather than degrading gracefully. Touches the lexer, parser, expander, and
`vars`' storage model. **Effort: L.**

### C23 — Associative arrays: `declare -A`
Present in bash 4+/ksh93/zsh. Common in modern tooling/config-processing
scripts; a natural follow-on once indexed arrays exist. **Effort: L.**

### C24 — Brace expansion: `{a,b,c}`, `{1..5}`
Present in bash/ksh/zsh/fish (not POSIX sh/dash). The most dangerous
*silent* gap in this whole document: rush doesn't error on `mkdir
{a,b,c}` — it creates one literally-named directory called `{a,b,c}`
instead of three. A bash script relying on this produces the wrong result
under rush with no warning at all. **Effort: M.**

### C25 — `case` fallthrough: `;&` / `;;&`
Present in bash 4+/ksh93/zsh (not POSIX). Moderate-value convenience; the
lexer already recognizes `;;`, so this is an incremental addition rather
than new machinery. **Effort: S.**

### C26 — `select` (numbered-menu prompt)
Specified by POSIX and implemented by bash/ksh93/zsh — though dash,
otherwise a fairly complete POSIX subset, omits it too, so rush would be in
reasonable company either way. **Effort: M.**

### C27 — C-style `for (( i=0; i<n; i++ ))`
Present in bash/ksh93/zsh (not POSIX sh/dash). A very common counted-loop
idiom in bash-family scripts; needs a new parser variant and reuses the
existing arithmetic evaluator. **Effort: M.**

### C28 — Standalone arithmetic command: `((expr))`
Present in bash/ksh93/zsh. The idiomatic way to write `((i++))` or `((count
+= 1))` as a statement instead of wrapping it in `$(( ))` and discarding the
value. Pairs naturally with C27 and C29. **Effort: S–M.**

### C29 — Richer arithmetic: `++`/`--`, `+=`, `**`, bitwise ops, ternary `?:` (tracked)
Present in bash/ksh93/zsh (POSIX arithmetic is more minimal, closer to
rush's current scope). `arith.rs`'s own doc comment already flags "no
assignment/increment inside the expression yet" — this rounds that out.
Without it, `$((i++))` and `$((a > b ? a : b))` simply don't parse.
**Effort: M.**

### C30 — Here-strings: `<<<`
Present in bash/ksh/zsh (not POSIX sh/dash). A small, extremely convenient
shorthand for `cmd <<< "$var"` instead of a full heredoc — low effort
relative to how often it shows up, and reuses the heredoc-feeding mechanism
already in `exec.rs`. **Effort: S.**

### C31 — Process substitution: `<(cmd)`, `>(cmd)`
Present in bash/ksh/zsh (not POSIX sh/dash). Treats a command's output as a
file — `diff <(cmd1) <(cmd2)`. Genuinely advanced, and a bigger lift than
most items here: needs named-pipe or `/dev/fd`-style plumbing. Lowest
priority in this tier. **Effort: L.**

---

## Tier V — Interactive UX

Where zsh and especially fish differentiate from bash/dash/ksh — and where
rush, having already written its own `rustyline` completion `Helper`, has a
real head start.

### C32 — History expansion: `!!`, `!$`, `!n`
Present in bash/zsh/ksh (csh-style recall). Rush already has persistent
history storage via `rustyline`; it has no bang-history recall syntax on
top of it yet. **Effort: S–M.**

### C33 — History-based autosuggestions
Native in fish; common via plugin in zsh. Shows a greyed-out completion of
the current line based on history as you type. A strong, well-scoped
differentiator for rush: its custom `RushHelper` already implements
rustyline's `Hinter` trait as a no-op — this is exactly the extension point
that trait exists for. **Effort: M.**

### C34 — Argument- and context-aware completion
Native and rich in fish; rich in zsh via compsys; bash gets it only via the
separate bash-completion project. Rush's completion is file/PATH/builtin-
name only today — it has no notion that a command's second word should
complete differently than its first. The single biggest interactive gap
versus fish/zsh specifically (not versus dash, which doesn't attempt this
either). **Effort: L.**

---

## Sequencing notes

Not formally tiered by dependency the way `rushgaps.md`'s G-series was, but
some natural orderings:

- **C1 (`#`/`##`/`%`/`%%`) and C7 (`read`) are the two highest-leverage single
  items** — they unblock the most common "why doesn't this basic script
  work" complaints a POSIX-shell user would hit first.
- **C9 (`shift`) + C11 (`getopts`) + existing positional-param/case support**
  together unlock real CLI-argument-parsing scripts — worth doing as a
  small group.
- **C18/C19/C20 (the rest of `set -euo pipefail` plus `-x`)** are a natural
  follow-on to the already-shipped `set -e`, reusing the same `vars.rs`
  flag-storage pattern.
- **C22 (indexed arrays) gates C23 (associative arrays)** — do C22 first.
- **C27/C28/C29 (C-style `for`, `((expr))`, richer arithmetic)** all extend
  `arith.rs` and the parser together — likely one combined pass rather than
  three separate ones.
- **C33 (autosuggestions)** is the standout cheap win in Tier V given
  `completion.rs` already has the `Hinter` trait wired up as a no-op.
