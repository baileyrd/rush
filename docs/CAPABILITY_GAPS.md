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
on top. Tier I (correctness/POSIX-risk) is now fully closed; the remaining
gap is Tier II's missing standard builtins — `read` above all, since nothing
else in that tier blocks as much everyday scripting on its own.

---

## Comparison matrix

A cross-section, not the full 32 below — enough to place rush relative to a
strict POSIX shell (dash), the bash family, and the interactive-first shells
(zsh, fish). ✅ full · 🟡 partial/simplified · ❌ not implemented · — not
applicable to that shell's own model.

| Capability | rush | dash | bash | ksh93 | zsh | fish |
|---|---|---|---|---|---|---|
| Real pipes / job control / forked subshells | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `#`/`##`/`%`/`%%` param. expansion | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| `read` / `printf` / `shift` / `getopts` | 🟡† | ✅ | ✅ | ✅ | ✅ | 🟡 |
| `local` function-scoped vars | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `wait` / `disown` | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `set -e` / `-u` / `-o pipefail` | 🟡 | 🟡 | ✅ | ✅ | ✅ | — |
| Indexed / associative arrays | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Brace expansion `{a,b,c}` | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Compound as one pipeline stage | 🟡* | ✅ | ✅ | ✅ | ✅ | ✅ |
| Traps beyond EXIT/INT firing | 🟡 | ✅ | ✅ | ✅ | ✅ | — |
| Context-aware completion | ❌ | — | 🟡 | 🟡 | ✅ | ✅ |
| History autosuggestion | ❌ | — | ❌ | ❌ | 🟡 | ✅ |
| Native Windows job control | ❌ | — | — | — | — | 🟡 |

\* Done for the interactive/script job-control path; a compound as one stage
among several *inside* a `$(...)` substitution, or on non-Unix, still errors.

† `read` (with `-r` and `$IFS` splitting), `printf` (sans `%e`/`%f`/`%g`),
and `shift` are done; `getopts` remains missing.

---

## Summary counts

- **Tier I — correctness/POSIX risk:** 6 (6 done — complete)
- **Tier II — missing standard builtins:** 11 (4 done)
- **Tier III — scripting-safety idioms:** 4
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

### C11 — `getopts`
The portable way to parse `-a`, `-b value`, combined short flags. Without
it every rush script hand-rolls option parsing from scratch. **Effort: M.**

### C12 — `command` / `type` / `hash`
`command -v foo` is the standard portable existence check used constantly
in install scripts and shell-form Makefiles. Without it, scripts fall back
to fragile `which`-based checks. **Effort: S–M.**

### C13 — `wait [pid|%job]`
A surprising gap given how much job-control machinery already exists (`&`,
`fg`, `bg`, `jobs`, `kill`) — `job.rs` already tracks pids/pgids, so this
mostly needs to expose `waitpid` on a selected job. `cmd & ; wait` is the
entire point of backgrounding something you need later. **Effort: S.**

### C14 — `source` / `.`
Rush already has the machinery — it sources `~/.rushrc` internally via its
own `run_source` helper — but exposes none of it as a user-invokable
command. Splitting a script into a reusable library via `. lib.sh` is one
of the most basic shell idioms there is. **Effort: S.**

### C15 — `eval`
Needed for constructing and running commands dynamically. Rush's
command-substitution path already re-parses and re-runs strings internally
— `eval` would reuse that exact mechanism, exposed as a builtin.
**Effort: S.**

### C16 — `exec`
Two standard idioms currently impossible in rush: `exec cmd` (process
replacement — common in container entrypoints) and `exec 3>file` (holding a
descriptor open for the rest of the script). **Effort: M.**

### C17 — `umask`
Needed by any script that creates files or directories with specific
permissions — currently no way to influence default permissions from
inside a rush script at all. **Effort: S.**

---

## Tier III — Scripting-safety idioms

The `set -euo pipefail` header is close to universal in production shell
scripts. Rush currently implements one third of it, and a simplified third
at that.

### C18 — `set -u` (nounset)
POSIX-mandated; present in dash/bash/ksh/zsh. Referencing an unset or
misspelled variable currently expands silently to an empty string — `-u`
turns that into an immediate, loud error instead. **Effort: M.**

### C19 — `set -o pipefail`
Present in bash/ksh/zsh (notably *not* dash — bash-family parity, not
strict POSIX). Without it, a pipeline's exit status is always just its last
stage's: `false | true` "succeeds," masking real failures anywhere earlier
in the chain. **Effort: M.**

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
