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
while closing out `eval`; C38 while closing out `exec`. `set -euo
pipefail` — the header nearly every production shell script opens with —
now works in full: `-e`, `-u` (C18), and `-o pipefail` (C19) all landed,
and `-x` (C20, xtrace) alongside them. `TERM`/`HUP` traps (C21) now fire
too — including interrupting a blocking wait immediately, the headline
case for a container's graceful-shutdown pattern. (One gap turned up
alongside `-x`: `set --`/`set args…` doesn't reassign positional
parameters at all — tracked as C39, still open.) Tier IV (bash/ksh/zsh
language parity, the least POSIX-y and largest tier) is now underway:
indexed arrays (C22) — `arr=(a b c)`,
`${arr[N]}`/`${arr[@]}`/`${arr[*]}`, sparse arrays, `arr[i]=`/`arr[i]+=`,
`unset 'arr[i]'`, `local arr=(...)` — are done, associative arrays
(C23) — a new `declare -A` builtin, `arr[key]=`/`arr[key]+=`,
`arr+=([k]=v ...)` merge-by-key, `${arr[@]}`/`${!arr[@]}` — followed on
top of them, brace expansion (C24) — `{a,b,c}`, `{1..5}`, `{a..z..2}`,
nesting, cross products — closed out what had been the single most
dangerous *silent* gap in this whole document (`mkdir {a,b,c}` used to
make one wrongly-named directory instead of three, with no warning at
all), `case` fallthrough (C25) — `;&`/`;;&` — rounded out `case` itself,
`select` (C26) — a numbered menu prompt, `$REPLY`, blank-line redisplay,
EOF handling — brought rush's control-flow keyword set to parity with
bash/ksh93/zsh's own, and C-style `for`/standalone `((expr))`/richer
arithmetic (C27–C29, done together in one pass since all three needed
the same lexer/`arith.rs` groundwork) rounded out arithmetic to real
C-like completeness — `++`/`--`, compound assignment, `**`, bitwise
operators, the ternary `?:`, all with genuine short-circuit evaluation.
Here-strings (C30) — `cmd <<< "$var"` — turned out to be exactly the
small, low-effort item predicted, reusing the heredoc-feeding mechanism
already in place with no `exec.rs` changes at all. Process substitution
(C31) — `<(cmd)`/`>(cmd)`, real fork/pipe plumbing behind a `/dev/fd/N`
path, non-blocking and concurrent same as real bash — closes out Tier IV
completely, 10 of 10. Finding it also turned up a real, general bug
unrelated to process substitution itself: Rust's runtime ignores
`SIGPIPE` by default, so any builtin's `print!`/`println!` panicked
instead of quietly dying on a closed pipe (`rush -c 'while true; do echo
x; done' | head` reproduced it with no substitution involved at all) —
fixed by resetting `SIGPIPE` to its default disposition at startup,
matching real bash's own C-program behavior exactly. Tier IV — bash/ksh/
zsh language parity, the least POSIX-y and largest tier — is now the
first fully-closed tier in this document, the biggest dent yet in what's
otherwise still a dash-shaped core. Tier V — interactive UX — is now
underway too: bang-history recall (C32) — `!!`, `!n`/`!-n`,
`!string`/`!?string?`, and the previous command's own `!$`/`!^`/`!*`/
`!:n` word designators, interactive-only exactly like real bash's own
`histexpand` default — reuses the persistent history `rustyline` already
provided, needing only a new textual preprocessing pass ahead of the
parser.

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
| `set -e` / `-u` / `-o pipefail` / `-x` | ✅§ | 🟡 | ✅ | ✅ | ✅ | — |
| Indexed arrays | ✅¶ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Associative arrays (`declare -A`) | ✅** | ❌ | ✅ | ✅ | ✅ | ✅ |
| Brace expansion `{a,b,c}` | ✅†† | ❌ | ✅ | ✅ | ✅ | ✅ |
| `case` fallthrough `;&` / `;;&` | ✅ | ❌ | ✅ | ✅ | ✅ | — |
| `select` (numbered-menu prompt) | ✅‡‡ | ❌ | ✅ | ✅ | ✅ | ❌ |
| C-style `for ((;;))` / `((expr))` / `++`/`--`/bitwise/`?:` | ✅§§ | ❌ | ✅ | ✅ | ✅ | ❌ |
| Here-strings `<<<` | ✅ | ❌ | ✅ | ✅ | ✅ | ❌ |
| Process substitution `<(cmd)` / `>(cmd)` | ✅¶¶ | ❌ | ✅ | ✅ | ✅ | ❌ |
| Compound as one pipeline stage | 🟡* | ✅ | ✅ | ✅ | ✅ | ✅ |
| Traps beyond EXIT/INT firing | 🟡‖ | ✅ | ✅ | ✅ | ✅ | — |
| Bang-history recall (`!!`/`!n`/`!$`/etc.) | ✅‖‖ | ❌ | ✅ | ✅ | ✅ | ❌ |
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

§ `-e`, `-u`, `-o pipefail`, and `-x` are all done; `-x`'s trace doesn't
cover a compound's own header line (`for i in 1 2`, `case a in`), only the
commands actually inside its body.

‖ `EXIT`/`INT`/`TERM`/`HUP` all fire now — including interrupting a
blocking wait immediately, not just once the foreground job finishes on
its own; `ERR`/`DEBUG` (bash/ksh/zsh extensions, not POSIX) remain
unimplemented.

¶ Literal assignment, all read forms (`${arr[N]}`/`${arr[@]}`/`${arr[*]}`/
`${#arr[@]}`/`${!arr[@]}`), sparse arrays, `arr[i]=`/`arr[i]+=`, `unset`
(whole array or one index), and `local arr=(...)` are all done. Not
supported: negative indices, `${arr[@]:offset:length}` slicing, a
subscript combined with pattern-removal/default operators, `declare -a`/
`declare -p` (no `declare` builtin's `-p` flag; `-a` itself is done, see
below).

** `declare -A`, literal assignment, all read forms (`${arr[k]}`/
`${arr[@]}`/`${arr[*]}`/`${#arr[@]}`/`${!arr[@]}`), `arr[k]=`/`arr[k]+=`,
`arr+=([k]=v ...)` merge-by-key, `unset 'arr[k]'`, and `local`/`declare -A
arr=(...)` are all done. Not supported: a literal multi-word key written
directly inside `[...]` in an assignment (`arr[key with spaces]=val`; the
`k="b c"; arr[$k]=val` idiom works); `declare -p`/`-x`/`-r`/`-i`/`-f`;
`declare`'s function-local scoping (always global/current-scope in rush).

†† Comma-lists (`{a,b,c}`), nesting, cross products (`{a,b}{c,d}`), and
numeric/single-letter ranges (`{1..5}`, `{a..z..2}`, zero-padding via a
leading zero on either endpoint) are all done, on ordinary command
arguments, `for`-loop word lists, array-literal elements, and
`local`/`declare`'s own arguments — matching real bash's expansion order
exactly (purely textual, before `$`/glob expansion). Not brace-expanded,
matching an accepted, documented scope narrowing: redirect targets, case
subjects/patterns, and (matching real bash, not a gap) assignment
statements' own values.

‡‡ The numbered menu, `$PS3` prompt (default and custom, `$`-expanded),
`$REPLY`'s raw/untrimmed content, blank-line redisplay, index parsing,
`break`, and the EOF-forces-status-1 quirk are all done. Not done, an
accepted cosmetic (not functional) narrowing: real bash lays the menu out
in columns sized to `$COLUMNS`; rush always prints one entry per line.

§§ `for ((init; cond; update))` (all clauses optional, `for ((;;))` a real
infinite loop), the standalone `((expr))` command (exit status mirrors
`test`'s convention), and `arith.rs`'s full operator set — `++`/`--`
(pre/post), `= += -= *= /= %= <<= >>= &= ^= |=`, `**`, bitwise `& | ^ ~ <<
>>`, and the ternary `?:` (all with real short-circuit evaluation for
`&&`/`||`/`?:`, not just value-discarding) — are all done. Not supported:
an lvalue other than a plain variable name (`arr[i]++`, `arr[i] = x`); the
comma operator.

¶¶ Read side (`<(cmd)`), write side (`>(cmd)`), concatenation with
adjacent text, quoting suppression, nesting, non-blocking/concurrent
timing, `$!`, assignment-RHS support, and redirect-target support are all
done. Not matched: real bash's own `/dev/fd` fd-numbering convention (a
fixed high range, counting down per substitution) — rush just uses
whatever fd the OS returns, functionally equivalent but a different
number. Combining an explicit non-standard redirect-target fd with a
substitution inherits the pre-existing C38 gap (fd 3+ redirects), not a
new limitation.

‖‖ Whole-event recall (`!!`, `!n`, `!-n`, `!string`, `!?string?`) and the
previous command's own word designators (`!$`, `!^`, `!*`, `!:n`) are all
done, interactive-only (matching real bash's own `histexpand` default —
off in scripts), including single-quote suppression, double-quote
non-suppression, and `\!` escaping. Not supported: combining an explicit
event specifier with a word designator (`!2:1`, `!echo:$`); quote-aware
word splitting for the designators (`echo "a b" c` then `!:1` gives
rush's plain-`split_whitespace` `"a` rather than real bash's quote-aware
`"a b"`).

---

## Summary counts

- **Tier I — correctness/POSIX risk:** 9 (6 done)
- **Tier II — missing standard builtins:** 12 (11 done)
- **Tier III — scripting-safety idioms:** 5 (4 done)
- **Tier IV — bash/ksh/zsh language parity:** 10 (10 done — complete)
- **Tier V — interactive UX:** 3 (1 done)

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

### C20 — `set -x` (xtrace) ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh. The standard way to debug a
misbehaving script — echoes each command before it runs. Rush previously
had no debugging aid like this at all.

Added `vars::set_xtrace`/`xtrace` (mirroring the other `set` flags' own
thread-local state) and `exec::trace_pipeline`, called from the one place
both the foreground and `$(...)`-capture paths funnel every already-expanded
`Pipeline` through (`run_foreground`/`capture_pipeline`) — so it covers a
plain command, each stage of a real pipeline, an assignment-only statement,
and (since `if`/`while`/`until` conditions run through this same machinery)
a compound's own condition, all in one hook. Each traced line is prefixed
with `$PS4` (default `+ `, falling back to the environment like `$PS1`
does); a leading `NAME=value` assignment traces on its own line before the
command it applies to; a word containing whitespace or a shell-special
character is re-quoted with single quotes for display. Nesting inside
`$(...)` repeats `$PS4`'s first character once per level (`vars::
with_deeper_trace`, wrapping `expand::command_substitute`) — `++ ` one
level down, `+++ ` two, exactly matching real bash, verified directly
including two-deep nesting and a custom `$PS4`.

Known gap, accepted for this scope: a compound's own *header* line — `for i
in 1 2`, `case a in` — isn't traced, only the commands actually inside its
body (which *do* trace correctly, per iteration/branch). Matching bash's
exact header format for every compound kind was a bigger lift than this
item's effort budget justified next to the headline case (seeing every
command that actually ran).

### C39 — `set -- args…` / `set args…` doesn't reassign positional parameters (tracked)
POSIX-mandated; present in dash/bash/ksh/zsh. The standard way to
reassign `$1`/`$2`/…/`$#` mid-script — the textbook idiom right after
`getopts` finishes (`shift $((OPTIND - 1)); set -- "$@"` to drop the
parsed flags) — or to split a string into positional fields (`set -- $line`).
Rush's `set` builtin only recognizes its flag-toggling forms
(`-e`/`-u`/`-x`/`-o pipefail`); any other argument, including a bare `--`,
is rejected outright (`set: --: not supported`, status 1) rather than
becoming the new `$1`/`$2`/…. Found while verifying C26 (`select`
without an `in` clause iterating `"$@"`) — needed a way to *set* `"$@"`
for the test and discovered there wasn't one; general, not specific to
`select` at all. **Effort: S** — `set_cmd` (`builtins.rs`) needs a
`--`-or-first-non-flag-argument branch that calls into whatever
`vars.rs` mechanism already backs `$1`/`$2`/`$#`/`"$@"` for a script's own
initial argv.

### C21 — Trap signals beyond `EXIT`/`INT` actually firing (tracked) ✅ done
`TERM`/`HUP` are POSIX-mandated; `ERR`/`DEBUG` are bash/ksh/zsh extensions.
Rush's `trap` builtin would happily *register* a handler for any name, but
only ever *fired* `EXIT` and `INT` — a script trapping `TERM` for graceful
shutdown (the standard container/daemon pattern) silently never got
called.

Added real signal handlers for `TERM`/`HUP` (`trap::install_signal_handlers`,
called once at startup in every mode — interactive or not, since the target
use case, a container's PID 1, has no terminal at all). The handler itself
only stores which signal arrived in a plain `AtomicI32` (safe from signal
context: no heap, no locks, nothing Rust-collection-shaped); `trap::
check_pending` — called back from ordinary code — does the real work of
firing the registered trap, or, if none is registered, terminating with the
conventional `128 + signal` status (still running any `EXIT` trap first,
exactly like real bash, verified directly).

The headline behavior, verified directly against real bash in every case:
a trapped signal interrupts a blocking wait *immediately*, not just once
the foreground job finishes on its own. `job::wait_pgid`/`wait_job_pgid`/
`wait_one`'s blocking `waitpid` loops now distinguish `EINTR` (retry after
handling the pending signal) from `ECHILD` (really done); if the trap body
itself calls `exit`, the process is gone before the loop ever resumes — if
it doesn't, the wait simply resumes, exactly reproducing bash's own
"the sleep picks up where it left off" behavior when a trap doesn't exit.
`check_pending` is also called at every ordinary command boundary
(`exec::exec_list_impl`'s per-job loop — covering every script, loop body,
function body, sourced file, and `eval`'d string, since they all funnel
through that one executor) and before each interactive prompt, for signals
that arrive when nothing is blocking at all.

Out of scope for this item, matching its stated boundary: `ERR`/`DEBUG`
(bash/ksh/zsh extensions, not POSIX-mandated) remain unimplemented.

---

## Tier IV — Bash/ksh/zsh language parity

Not POSIX-mandated, but rush's own README calls it "bash-compatible" —
these are the extensions real bash scripts lean on most.

### C22 — Indexed arrays: `arr=(a b c)`, `${arr[@]}`, `${#arr[@]}` ✅ done
Present in bash/ksh93/zsh (not POSIX sh/dash — bash-family parity, not
POSIX parity). Heavily used in modern bash scripts; previously failed
outright rather than degrading gracefully. Touched the lexer, parser,
expander, and `vars`' storage model, exactly as scoped.

**Storage** (`vars.rs`): a variable's payload is now `enum VarValue {
Scalar(String), Array(BTreeMap<usize, String>) }` (`BTreeMap` for real
sparse-array semantics — `arr[5]=x` on a 2-element array doesn't create
indices 2–4 — with free sorted iteration for `${arr[@]}`/`${!arr[@]}`).
Every existing scalar function (`get`/`set`/`unset`/`export`/`exported`/
the `local`-frame shadow-restore mechanism) now branches on this, alongside
new array-specific ones (`set_array`, `array_get`/`array_set`/
`array_append`/`array_append_index`, `array_values`/`array_indices`/
`array_len`, `array_unset_index`, `declare_local_array`) and a shared
`assign(name, &AssignOp)` entry point covering all four assignment shapes
(scalar/array × set/append) plus the two indexed ones (`arr[i]=`/
`arr[i]+=`).

**Lexer** (`lexer.rs`): a new `WordPart::ArrayLiteral(Vec<Word>)` — `(` and
`)` are already lexer-level tokens (used for subshells/case groups), so
`arr=(a b c)` needed a lexer-level heuristic (`looks_like_array_assign_prefix`)
recognizing a word ending in `=`/`+=` with no space before the `(`, at
which point the whole parenthesized list — spanning newlines, each element
its own `Word` so quoting/expansion inside one still works — is consumed
as a single `WordPart` rather than breaking the word at the paren. Every
existing exhaustive `WordPart` match got a defensive arm: `ArrayLiteral`
only ever appears as the part right after an `Unquoted` part ending in
`=`/`+=`, always intercepted by `expand::assignment_split` before reaching
anywhere else — genuinely unreachable outside it.

**Expansion** (`expand.rs`): `assignment_split` now recognizes three shapes
— `NAME=(...)`/`NAME+=(...)` (whole-array literal/append, elements
individually glob/command-substitution-expanded, matching bash exactly,
verified directly), plain `NAME=value`/`NAME+=value` (unchanged), and the
new `NAME[subscript]=value`/`NAME[subscript]+=value` (one element, the
subscript evaluated as arithmetic — same two-step pipeline `$((...))`
itself uses, so both a bare `${arr[i+1]}` and a `$`-prefixed
`${arr[$i]}`/`arr[$i]=x` resolve). `expand_braced` gained subscript
support for reads: `${arr[N]}`, `${arr[@]}`/`${arr[*]}` (the `@`/`*`
join-vs-preserve distinction mirrors `$@`/`$*`'s own, including a new
`"${arr[@]}"`-is-like-`"$@"` special case in `expand_argv_word` so quoted
whole-array expansion preserves each element as its own field), `${#arr[@]}`
(count)/`${#arr[N]}` (that element's length), and `${!arr[@]}` (the
indices actually present — skips gaps). `arr=x` on an *existing* array
targets element 0 only, leaving the rest alone — matching bash exactly,
verified directly (this lives in the ordinary `set()`, so it's not
array-literal-specific: any scalar-shaped assignment to an already-array
name behaves this way).

**`local`** (`builtins.rs`/`exec.rs`): `local arr=(a b c)` needed special
handling — `local`'s own arguments are ordinary argv words, but a plain
`Vec<String>` argv can't carry an array literal at all. `expand_simple` now
recognizes the command word "local" and parses its declarations itself
(reusing `assignment_split`) into a new `Command::local_decls` field,
funneled to a new `builtins::local_from_decls` dispatched directly from
`exec::dispatch_builtin` rather than through the ordinary string-argv
builtin path — scalar `local name`/`local name=value` behavior is
unchanged.

Explicitly out of scope, each a documented, accepted gap: negative indices
(`${arr[-1]}`, a bash 4.3+ feature); `${arr[@]:offset:length}` slicing; a
subscript combined with pattern-removal or a default/alternate operator
(`${arr[0]#pat}`, `${arr[@]:-x}`); `declare -a`/`declare -p` (rush has no
`declare` builtin at all); `local arr[i]=x` (indexing a not-yet-local array
in the same breath — falls back to a bare `local name`); exporting an
array to a spawned child's environment (no portable representation);
arithmetic side effects inside a subscript (`arr[i=1]=x`). Every one of
these was verified directly against real bash to confirm the *behavior*
being skipped, not just assumed from documentation.

Every case in this item — literal assignment, all three read forms,
sparse arrays, element/whole-array set and append, `unset` (whole array
and single index, including `unset 'arr[$i]'`'s own independent subscript
evaluation), scalar↔array promotion, and `local` — was verified directly
against real bash, including exact edge cases (a distinct exit code per
array position, multi-line literals, glob/command-substitution expansion
inside a literal) chosen specifically to disambiguate from a plausible-but-
wrong implementation.

### C23 — Associative arrays: `declare -A` ✅ done
Present in bash 4+/ksh93/zsh (not POSIX sh/dash/ksh88). Common in modern
tooling/config-processing scripts; a natural follow-on once indexed arrays
(C22) existed. Required an entirely new `declare` builtin (rush had none at
all) and a non-trivial retrofit of C22's subscript evaluation, which had
assumed "always arithmetic."

**`declare` builtin** (`builtins.rs`, new): bash requires `declare -A name`
before `name[key]=val` treats `key` as a literal string key rather than an
arithmetic expression (which would evaluate a non-numeric key to 0). rush's
`declare` is a deliberately narrow subset: `-a`/`-A` (type) plus an optional
`=(...)` initializer, dispatched through the same `Command::local_decls`
mechanism C22 built for `local`. Not implemented: `-p` (print), `-x`
(export), `-r` (readonly), `-i` (integer), `-f` (functions), and bash's
"`declare` acts like `local` inside a function" nuance — rush's `declare`
always applies to the global/current scope, an explicit simplification.

**Storage** (`vars.rs`): `VarValue` gained a third variant, `Assoc(BTreeMap<
String, String>)`, alongside `Scalar`/`Array`. `is_assoc(name)` exposes a
variable's runtime type so callers can dispatch on it. New assoc-specific
functions mirror the array ones: `set_assoc`, `assoc_get`, `assoc_keys`,
`assoc_unset_key`, `assoc_merge` (upsert-by-key for `+=`), and
`declare_local_assoc`.

**The type-aware subscript retrofit**: C22 treated every subscript as
arithmetic (`arr[i+1]=x` evaluates `i+1`). Associative arrays need the
opposite: `arr[a+b]=x` on a `-A` array uses the *literal* key `"a+b"`, never
arithmetic — but `arr[$k]=x` still `$`-expands `$k` first. This can only be
resolved at assignment/read time, once the target name's current runtime
type is known, so `AssignOp`'s indexed variants changed from
`SetIndex(usize, String)`/`AppendIndex(usize, String)` to `SetKey(String,
String)`/`AppendKey(String, String)` — raw subscript text, evaluation
deferred — and two dispatchers in `vars.rs` make the call:
```rust
pub fn key_set(name: &str, subscript: &str, value: &str) {
    if is_assoc(name) {
        assoc_set(name, subscript, value);
    } else if let Some(index) = crate::expand::eval_subscript(subscript) {
        array_set(name, index, value);
    }
}
```
(`key_append` mirrors this for `+=`.) `expand.rs` splits the old
`eval_subscript` into `resolve_subscript_text` (`$`-expansion only, always
applied) and a narrower `eval_subscript` (arithmetic, called only once a
name is confirmed *not* assoc).

**Expansion** (`expand.rs`): `${arr[key]}`, `${!arr[@]}` (keys, the assoc
analogue of C22's index-list read), `${arr[@]}`/`${arr[*]}` (values, same
`@`-vs-`*` join/preserve split as indexed arrays), and `${#arr[@]}` all
dispatch on `is_assoc`. `"${!arr[@]}"` and `"${arr[@]}"` both needed the
same per-key field-preservation as indexed arrays' `"$@"`-like handling —
`parse_whole_array_at` became `enum WholeArrayAt { Values(String),
Keys(String) }` to cover both. `arr+=([k1]=v1 [k2]=v2)` merges/upserts by
key rather than positionally appending (`assoc_merge`); this required
teaching *both* the `local`/`declare`-prefixed literal path and the
ordinary top-level `NAME+=(...)` literal path to check `is_assoc(&name)`
before deciding whether elements are plain words or `[key]=value` pairs —
initially only the `local`/`declare` path did this, which silently broke
`arr+=(...)` on an already-`declare -A`'d array from an earlier statement.

**`local`/`declare`** (`builtins.rs`/`exec.rs`): the `local`-only
special-casing `expand_simple` built for C22 is now shared by `declare`,
scanning both for `-A`/`-a` flags to decide array-vs-assoc-vs-scalar before
parsing declarations.

Explicitly out of scope, each a documented, accepted gap: an unquoted or
quoted-literal multi-word key written directly inside `[...]` in an
assignment (`arr[key with spaces]=val`, `arr["b c"]=2`) — rush's lexer
splits assignment words on whitespace with no awareness of "inside an
assignment's brackets," and `assignment_split`'s bracket-scanning doesn't
stitch a quoted-and-unquoted-mixed subscript back into one string; the
working idiom, `k="b c"; arr[$k]=val`, was verified to work correctly and
is the natural way to write this in bash too. Also out of scope: `declare
-p`/`-x`/`-r`/`-i`/`-f`; `declare`'s function-local scoping nuance (rush's
`declare` is always global/current-scope); bash's separate
explicit-index syntax for *indexed* arrays (`arr=([5]=x [2]=y z)`, not an
associative-array feature but easily confused with one); a subscript
combined with pattern-removal or default/alternate operators
(`${arr[k]:-x}`) — confirmed to be the same pre-existing C22 gap, not
newly introduced by associative arrays. Every behavior above — including
the merge-by-key `+=` semantics, the `declare -A` prerequisite, and the
literal-vs-arithmetic subscript split — was verified directly against real
bash.

### C24 — Brace expansion: `{a,b,c}`, `{1..5}` ✅ done
Present in bash/ksh/zsh/fish (not POSIX sh/dash). Was the most dangerous
*silent* gap in this whole document: rush didn't error on `mkdir
{a,b,c}` — it created one literally-named directory called `{a,b,c}`
instead of three, with no warning at all.

**Where it runs, and where it deliberately doesn't**: brace expansion
happens purely on a word's raw, unexpanded text, before `$`/glob
expansion — same order as real bash, verified directly (`{$x,y}` expands
the braces into two words first; `$x` then resolves normally in whichever
one it lands in, and `{1..$n}` is an *invalid* range at brace-expansion
time since `$n` isn't yet a literal integer — the whole group is left as
literal text even though `$n` itself still expands afterwards). It's
wired into `expand_argv_word` (so it covers ordinary command
arguments, `for`-loop word lists, and array-literal elements — all three
already funnel through it) and into `local`/`declare`'s own
argument-parsing loop (verified directly: `local x={a,b}` *does*
brace-expand, becoming two words `x=a` then `x=b` applied in order,
leaving `x=b` — bash treats `local`'s arguments as ordinary command
words, not assignment-statement syntax). It's deliberately *not* wired
into assignment-statement values: a bare `x={a,b}` or a prefix `FOO={a,b}
cmd` keeps the literal text unexpanded, matching real bash exactly (only
`local`/`declare`'s pseudo-assignment words differ, precisely because
they're ordinary argv words under the hood, not real assignment syntax).
Redirect targets and case subjects/patterns are also left un-expanded —
an accepted, documented narrowing (real bash *does* brace-expand a
redirect target, producing "ambiguous redirect" if it comes out to more
than one word; rush's redirect-target expansion doesn't go through this
path at all, so `> {a,b}` still just creates a literally-named file).

**Implementation** (`expand.rs`): a new `BraceAtom` enum re-represents a
`Word`'s content for scanning purposes — `Ch(char)` for a character from
an `Unquoted` part (eligible to be `{`/`,`/`.`, or ordinary text) or
`Opaque(WordPart)` for a `Quoted`/`Literal`/`ArrayLiteral` chunk, inert to
brace syntax but still carried through verbatim into whichever
alternative it lands in (`pre{"a,b",c}post` splits on the *unquoted*
comma only — the quoted one is just literal content — verified directly
against bash). `brace_expand_atoms` scans left to right for the first
*valid* `{...}` group (depth-tracked bracket matching via
`matching_close`) and expands it, recursing into the suffix for any
further group (`{a,b}{c,d}` is a cross product); an invalid group (no
top-level comma and not a valid range — `{a}`, `{1..$n}`, unterminated)
is left as a literal `{` and the scan resumes right after it, so one
invalid group doesn't block a valid one later in the same word (`{{a,b}`
→ `{a`, `{b`: the outer `{` is unterminated as its own group since the
first `}` closes the inner one instead, falls back to literal, and the
scan finds `{a,b}` starting one character later — verified directly).
`expand_group` tries a comma-list first (splitting only on *top-level*
commas — one inside a nested `{...}` doesn't count, and each segment is
itself recursively brace-expanded, so `{a,{b,c},d}` → `a b c d`, not
`a {b,c} d`); failing that, a range (`expand_range`) — numeric
(`{1..5}`, `{-3..3}`) or single-letter (`{a..z}`, stepping raw ASCII code
points even across a mixed-case pair like `{A..z}`), both with an
optional third `..step` field (its sign is ignored — direction is always
inferred from the endpoints — and an explicit step of `0` is treated as
`1`, matching bash exactly). Zero-padding: a leading `0` on either
endpoint (after an optional sign, and with more than one digit) triggers
padding of every generated term to that endpoint's own total literal
width, sign included — `{-01..05}` produces `-01 000 001 002 003 004
005`, each three characters, matching bash's own documented example
exactly; a leading `+` never counts (`{+1..+3}` is plain `1 2 3`,
unpadded).

Explicitly out of scope, each a documented, accepted gap: redirect
targets and case subjects/patterns aren't brace-expanded (see above);
assignment-statement values aren't either (matches bash, not a gap, but
noted since it's easy to expect otherwise); a generated range element
that happens to itself be a shell metacharacter — specifically a bare `\`
from a mixed-case ASCII range crossing code point 92, e.g. one term of
`{A..z}` — doesn't get real bash's own post-generation
backslash-consumption quirk (bash silently drops that one term; rush
prints the literal `\`), an extremely obscure corner no real script
depends on. Every other case — comma-lists, nesting, cross products,
quoting interactions, numeric/letter ranges with and without an explicit
step, zero-padding (including negative and all-zero cases), the
assignment-vs-argument-word distinction, and the `$`-expansion ordering —
was verified directly against real bash across more than 60 scenarios,
matching exactly.

### C25 — `case` fallthrough: `;&` / `;;&` ✅ done
Present in bash 4+/ksh93/zsh (not POSIX). Two new lexer tokens
(`Token::SemiAmp` for `;&`, `Token::DSemiAmp` for `;;&`, alongside the
existing `Token::DSemi` for `;;`) and a new `CaseTerm` enum
(`Break`/`FallThrough`/`Continue`) recording which terminator closed each
`Compound::Case` item — defaulting to `Break` when the last item before
`esac` omits one, same as today.

**Semantics** (`exec.rs`), verified directly against real bash: `;;`
(`Break`) stops — the case's result is whatever its own body's last
command returned. `;&` (`FallThrough`) unconditionally runs the *next*
item's body too, with no pattern test at all, and chains through that
item's own terminator in turn (`a) ..;& b) ..;& c) ..;;` on a subject
matching `a` runs all three bodies in order). `;;&` (`Continue`) resumes
*pattern* testing starting at the next item — not unconditional — running
the first one (if any) whose pattern matches, same as if the whole `case`
restarted from there (`a) ..;;& b) ..;; a) ..;;` on subject `a` runs the
first and third bodies, skipping `b`, since only the third item's pattern
matches on the resumed scan). `$?` after the whole `case` is always the
last body that actually ran, whichever terminator chain led there.

Every scenario — a single `;&`, a chain of several in a row, `;;&`
finding a later match vs. finding none, a trailing `;;&` on the last item
before `esac` (nothing left to resume into — stops, same as `;;`), exit
status propagation through a fallthrough chain, and the terminator-less
last-item default — was verified directly against real bash across 10
scenarios, matching exactly.

### C26 — `select` (numbered-menu prompt) ✅ done
Specified by POSIX and implemented by bash/ksh93/zsh — though dash,
otherwise a fairly complete POSIX subset, omits it too.

**Grammar** (`parser.rs`): `select NAME [in WORDS]; do BODY; done` — the
same grammar as `for` (including the `has_in`/`words` convention: an
omitted `in` iterates `"$@"`; an explicit `in` with no words is a real
empty list), just a new reserved word and a `Compound::Select` variant.

**Execution** (`exec.rs`), verified directly against real bash across
more than a dozen scenarios: an empty word list is a no-op (status 0, no
menu, no read at all — same as `for`). Otherwise, prints the numbered
menu to *stderr*, then loops: print `$PS3` (default `"#? "` when
genuinely unset; an explicit `PS3=` prints nothing — both `$`-expanded
via the same `expand_dollars` `$PS1` itself doesn't use, since `PS1` has
its own bespoke backslash-escape codes while `PS3` is ordinary
parameter/command-substitution expansion), read one line. `$REPLY` is
set to that line *raw* — no `$IFS` splitting or trimming at all, unlike
ordinary `read` (verified directly: three bare spaces as the whole line
come back in `$REPLY` as three literal spaces, where `read reply` on the
same input trims to empty) — though it does still get `read`'s own
backslash-continuation processing (`a\<newline>b` still joins into `ab`).
A genuinely empty line (zero length, not merely all-whitespace)
redisplays the menu and prompts again *without* running `BODY` at all.
Otherwise: the line, trimmed and parsed as a base-10 integer (leading
`+`/`-`/zero-padding all tolerated) in `1..=len(words)`, sets `NAME` to
that word; out of range or unparseable sets `NAME` to `""` — either way
`BODY` then runs once, with the same `break`/exit-status-propagation
machinery `for`/`while` already use. EOF while reading ends the whole
construct with status `1`, *overriding* whatever `BODY`'s last run
returned — a real, deliberate bash quirk verified directly (distinct
from `while read line; do …; done`, whose own status after its final
failing `read` keeps the loop body's last-iteration status instead) —
and prints a trailing newline first, so the shell doesn't leave the
cursor stuck at the end of an unanswered prompt.

Explicitly out of scope, a documented, accepted (cosmetic, not
functional) narrowing: real bash lays the menu out in columns sized to
`$COLUMNS`; rush always prints one entry per line instead. Every
functional behavior — numbering, `$REPLY`'s raw/untrimmed/unsplit
content, the blank-line redisplay, index parsing's tolerance for
whitespace/sign/leading zeros, out-of-range/unparseable → empty `NAME`,
`break`'s exit-status semantics, and the EOF-forces-status-1 quirk — was
verified directly against real bash and matches exactly.

### C27 — C-style `for (( i=0; i<n; i++ ))` ✅ done
Present in bash/ksh93/zsh (not POSIX sh/dash). A very common counted-loop
idiom in bash-family scripts. Done together with C28/C29 in one pass,
as this doc's own sequencing notes suggested — all three needed the same
lexer/`arith.rs` groundwork.

New `Compound::CFor { init, cond, update, body }`, each clause `None`
when its part of the header was left empty (`for ((;;))` is a real
infinite loop — `cond: None` means always-true; `init`/`update: None`
are no-ops — all verified directly). `parse_for` checks for
`Token::DblParen` (see C28) right after the `for` keyword, before
falling through to the ordinary `NAME [in WORDS]` grammar — a `NAME`
can never itself start with `(`, so this is unambiguous, and no space is
needed between `for` and `((` (`for((i=0;...` parses the same as
`for ((i=0;...`, verified directly). Execution (`exec.rs`) evaluates
`init` once, then loops: test `cond` (or `true` if absent), run `body`,
then `update` — except when `body` ended via `break` (here, or
propagating from an outer loop via `break N`/`continue N`), in which
case `update` does *not* run; a local `continue` (level 1) *does* still
run `update` before re-testing `cond` — real C `for` semantics, both
verified directly against bash.

### C28 — Standalone arithmetic command: `((expr))` ✅ done
Present in bash/ksh93/zsh. The idiomatic way to write `((i++))` or `((count
+= 1))` as a statement instead of wrapping it in `$(( ))` and discarding the
value.

The real complexity here was disambiguation, not evaluation: `((expr))`
in command position is *never* two nested subshells, full stop — bash
doesn't try one reading and fall back to the other, verified directly
(`((echo hi))` fails as invalid arithmetic rather than running `echo hi`
in a doubly-nested subshell; a nested-subshell reading needs an explicit
space, `( (echo hi) )`). Since a subshell's own `(`/`)` are ordinary
lexer-level tokens with no memory of surrounding whitespace by the time
the parser sees them, this has to be decided in the lexer, the same way
`$((...))` already disambiguates from `$(...)` (peeking whether the
character right after the first `(` is *also* `(`, with no gap). A new
`Token::DblParen(String)` — holding the raw, unsplit text between the
matching `((`/`))`, via a new `take_double_paren` mirroring
`expand::take_arith`'s identical depth-tracking algorithm — is emitted
wherever the lexer would otherwise tokenize a bare `(`, whenever a
second `(` immediately follows. The parser turns this into a new
`Compound::Arith(String)` at command position (see C27 for `for
((...))`'s own use of the same token). Execution `$`-expands the text
(same two-step pipeline `$((...))` itself uses) then evaluates it via
`arith.rs` (C29) for its side effects; exit status mirrors `test`'s
convention (`0` if the result is nonzero, `1` if zero). An empty `(( ))`
evaluates as `0`/status `1` rather than erroring — matching a real,
slightly odd bash asymmetry with `$(( ))` (which *does* error on empty),
verified directly.

### C29 — Richer arithmetic: `++`/`--`, `+=`, `**`, bitwise ops, ternary `?:` ✅ done
Present in bash/ksh93/zsh (POSIX arithmetic is more minimal, closer to
rush's previous scope). `arith.rs`'s own doc comment used to flag "no
assignment/increment inside the expression yet" — this rounds that out,
and was the prerequisite C27/C28 both needed (a C-style `for` header and
a standalone `((...))` are close to meaningless without `i++`/`i+=1`).

**The one real architectural change**: the previous `arith.rs` combined
parsing and evaluation into one recursive-descent pass — fine when
nothing has side effects, but assignment means `&&`/`||`/`?:` have to
*actually* short-circuit (`0 && (i=5)` must never run the assignment,
verified directly), which a combined pass can't do once it's already
recursed into evaluating the right-hand side. `arith.rs` now parses into
an `Expr` tree first, then a separate `eval_expr` walks it, evaluating
`LogAnd`/`LogOr`/`Ternary`'s untaken side not at all — not just
discarding its value.

**Grammar**, precedence lowest to highest (verified directly against
real bash at every boundary): assignment (`=`, and `+= -= *= /= %= <<=
>>= &= ^= |=` — deliberately no `**=`, since real bash itself doesn't
have one either, verified directly) is lowest and right-associative
(`a = b = 5` assigns `b` first); then ternary `?:` (right-associative in
its `else` position: `a?b:c?d:e` is `a?b:(c?d:e)`); `||`; `&&`; bitwise
`|`, `^`, `&` (in that order — binding *looser* than the comparison
operators below them, the classic C gotcha, e.g. `2+3 & 1` is `(2+3)&1`
= `1`, not `2+(3&1)`); `==`/`!=`; `<`/`<=`/`>`/`>=`; `<<`/`>>`; `+`/`-`;
`*`/`/`/`%`; `**` (right-associative, binds *tighter* than `*` but
*looser* than unary — `2*3**2` is `2*(3**2)` = 18, `-2**2` is `(-2)**2`
= 4); prefix `- + ! ~ ++ --`; postfix `++`/`--` (highest, and only valid
directly on a variable name — `(1+2)++` errors, matching real bash).
Assignment/`++`/`--`'s lvalue must be a plain variable name — indexing
an array element in arithmetic context (`arr[i]++`, `arr[i] = x`) isn't
supported, an accepted, documented gap.

Every behavior — operator precedence at each boundary above, `**`'s
right-associativity and its interaction with unary/`*`, bitwise
operators' looser-than-comparison binding, ternary's right-associative
nesting, chained right-associative assignment, compound-assignment's
own RHS being a full sub-expression (`a += 2*3`), pre- vs. post-`++`/`--`
returning the new vs. old value respectively, and `&&`/`||`/`?:` never
running the untaken side's side effects — was verified directly against
real bash and matches exactly.

### C30 — Here-strings: `<<<` ✅ done
Present in bash/ksh/zsh (not POSIX sh/dash). A small, extremely convenient
shorthand for `cmd <<< "$var"` instead of a full heredoc.

Turned out to be exactly the low-effort item predicted: a new
`RedirOp::HereString` (lexer) — checked for right after `<<`, before
falling into the ordinary heredoc-delimiter reading, so `<<<` never gets
misread as `<<` followed by a stray `<` — and a new
`RawRedirect::HereString(Word)` (parser), whose word the parser reads
exactly like any other redirect's filename. Expansion (`expand.rs`)
treats that word like a normal redirect target — single-word expansion
only, no splitting/globbing (verified directly: `x="a b"; cat <<< $x`
still feeds `a b` as one line, not two) — appends exactly one `\n`
(always, even if the expanded text already ends with one, matching real
bash's own behavior exactly), and drops the result straight into the
same `heredoc: Option<String>` slot a real here-document's body already
uses — meaning `exec.rs` itself needed zero changes; the entire feeding
path (`redirect_stdio`, `feed_heredoc`) was already generic over "some
string feeds stdin," regardless of which redirect produced it. A later
`<<`/`<<<` on the same command overwrites an earlier one, same "last
redirect for a given fd wins" rule as any other redirect, verified
directly. Every case — a literal string, an unquoted variable (no
splitting), a multi-line quoted string, the always-append-one-newline
rule, and the last-redirect-wins interaction with a real heredoc on the
same command — was verified directly against real bash and matches
exactly.

### C31 — Process substitution: `<(cmd)`, `>(cmd)` ✅ done
Present in bash/ksh/zsh (not POSIX sh/dash). Treats a command's output as a
file — `diff <(cmd1) <(cmd2)`. The biggest lift in this tier, needing real
fork/pipe plumbing rather than just parser/`arith.rs` work.

**Mechanism**, verified directly against real bash at every point below:
on Linux, `<(cmd)`/`>(cmd)` is a genuine pipe plus `/dev/fd`'s
magic-symlink-to-an-open-fd trick — not a named FIFO, which bash only
falls back to on platforms without `/dev/fd` at all (not a concern for a
Unix-only feature; gated `#[cfg(unix)]`, matching subshells/job control).
A new `exec::process_substitute(src, write_side)` forks `cmd` (parsed via
the ordinary `parser::parse`, run via `run_list`) hooked up to one end of
a `make_pipe()` pipe — its stdout for `<(cmd)`, its stdin for `>(cmd)` —
and returns a `/dev/fd/<n>` path for the *other* end, which the shell
process itself keeps open. Crucially, this never blocks waiting for
`cmd` (verified directly: `diff <(sleep 1; echo a) <(sleep 1; echo b)`
takes ~1s total, not ~2s serialized) — the kept-open fd survives,
unclosed, until the caller has finished spawning whatever the
substitution's path was expanded into (fork+exec inherits open,
non-`CLOEXEC` fds unchanged; `make_pipe`'s raw `libc::pipe` already
doesn't set `CLOEXEC`), then gets closed and its child
non-blocking-reaped via a new `close_pending_proc_subs`, called once at
each of the handful of places a whole pipeline actually gets spawned
(ordinary foreground, backgrounding, `$(...)` capture) — covering every
path a substitution's word could expand into (builtin, function call, or
a real spawned child) without needing to duplicate this at each of
*those* individually. `$!` reflects the substitution's own pid — real,
current bash behavior, verified directly (`: <(echo hi); echo $!` prints
a real, distinct pid each time) — deliberately not added to the job
table, though, matching real bash's own `jobs -l` not listing one either.

**Lexing/expansion**: unlike `WordPart::ArrayLiteral`, this needed no new
`WordPart` variant at all — `<(cmd)`/`>(cmd)` is captured as raw text
embedded directly in a `WordPart::Unquoted` string, exactly like
`$(cmd)` already is, via the existing `consume_balanced_paren`. Since
`(` immediately after `<`/`>` never starts a real redirect (verified
directly: real bash always reads it as process substitution, with no
attempt at the alternative reading first), the lexer checks for it both
at the top level (before falling into ordinary `<`/`>` redirect lexing)
and *inside* `lex_word`'s own per-character loop, so `pre<(cmd)post`
concatenates onto adjacent text the same way `$(...)` does (verified
directly). A new `expand::expand_unquoted` — `expand_dollars` plus this
same `<(`/`>(` recognition — is used specifically for genuinely
*unquoted* text (ordinary argv words, assignment values, redirect
targets, case subjects): quoting fully suppresses process substitution
in real bash (verified directly: `echo "<(echo hi)"`/`echo '<(echo
hi)'` both print the literal text), unlike `$(...)`, which *does* still
expand inside double quotes — this is why it's a separate function
rather than a flag threaded through `expand_dollars` itself, which stays
`$`-only for genuinely quoted text. Assignment RHS *does* get process
substitution — a real, deliberate asymmetry with brace expansion (which
doesn't), verified directly (`x=<(cmd)` assigns the literal path,
forking for real).

**A real, general bug found and fixed along the way (not specific to
process substitution)**: Rust's runtime sets `SIGPIPE` to `SIG_IGN` at
startup, so any builtin's `print!`/`println!` surfaces a write to an
already-closed pipe as an ordinary `Err` — which those macros then
*panic* on, dumping a scary backtrace, instead of the process just
quietly dying the way a normal Unix command does (`rush -c 'while true;
do echo x; done' | head` reproduced this with no process substitution
involved at all). Fixed by resetting `SIGPIPE` to its default
disposition once at startup (`main.rs`) — matching real bash's own
behavior exactly, verified directly: bash's own builtin `echo` hits the
identical race against a `>(...)` whose reader exits without reading,
and just dies silently there rather than printing anything.

Explicitly out of scope, each a documented, accepted gap: the exact
`/dev/fd` fd *numbers* real bash uses (a fixed high range starting at
63, counting down per additional substitution on one command line) —
rush just uses whatever fd the OS hands back, which is typically much
lower; scripts shouldn't (and don't, in practice) hardcode the number
either way. A substituted command combined with an explicit non-standard
redirect target fd (`cat 3< <(cmd)`, `exec 3< <(cmd)`) inherits the
pre-existing C38 limitation (redirects to fd 3+ collapse) — confirmed
directly that `exec 3< <(cmd)` hangs the exact same way `exec 3< anyfile`
already does today, with no process substitution involved. A write-side
substitution whose own reader exits without ever reading (`echo hi >
>(exit 7)`) races the main command's own write against the reader's
exit — an inherent property of the underlying pipe, confirmed to
reproduce identically (and just as unpredictably) in real bash itself
under concurrent load, not something to paper over with rush-only
synchronization real bash doesn't have either.

Verified directly against real bash across more than a dozen scenarios —
read side, write side, concatenation, quoting suppression, nesting,
piping inside a substitution, assignment RHS, redirect targets, `$!`,
non-blocking/concurrent timing, and status independence — all matching
exactly (aside from the documented fd-number cosmetic difference).

---

## Tier V — Interactive UX

Where zsh and especially fish differentiate from bash/dash/ksh — and where
rush, having already written its own `rustyline` completion `Helper`, has a
real head start.

### C32 — History expansion: `!!`, `!$`, `!n` ✅ done
Present in bash/zsh/ksh (csh-style recall). Rush already had persistent
history storage via `rustyline`; it now has bang-history recall syntax on
top of it too.

**Mechanism**: a new `history_expand::expand(line, history)` — a plain
textual preprocessing pass over the raw input line, run in `main.rs`'s
`interactive()` loop *before* the line reaches `parser::parse` or
`rl.add_history_entry`, exactly matching where real bash's own
readline/history layer does this (so it applies regardless of what the
line eventually parses as, and a failed reference blocks execution
entirely rather than surfacing as a shell syntax error — verified
directly). Interactive-only, matching real bash's own `histexpand`
default (on interactively, off in scripts) — a script run via `rush -c`/
`rush file` never sees this pass at all.

**Scope, verified directly against real bash at every point**: whole-event
recall — `!!` (last command), `!n`/`!-n` (absolute/relative event number,
matching `history`'s own 1-based numbering), `!string`/`!?string?`
(backward prefix/substring search) — and the previous command's own word
designators — `!$` (last word), `!^` (first argument), `!*` (all
arguments), `!:n` (word `n`, 0-based, `n=0` the command name itself).
Quoting/escaping mirrors real bash exactly: single quotes suppress
expansion, double quotes do *not*, and `\!` de-escapes to a literal `!`
with no echo (verified directly that bash's own history file stores the
still-backslashed raw line here, not a de-escaped one — so passing the
untouched raw line through unexpanded, letting rush's own lexer's
already-generic `\X` → literal `X` handling do the stripping, produces
the identical end result). A bare `!` followed by whitespace, end of
line, or `=` (so `test`'s `!=` is never misread as a history reference)
is left completely untouched, no error — also verified directly. An
event that can't be resolved reports "event not found"/"bad word
specifier" and runs nothing, matching real bash's own "a failed
reference blocks execution" behavior (not its exact wording).

Explicitly out of scope, each a documented, accepted gap given this
item's S–M effort budget: combining an explicit event specifier with a
word designator in one reference (`!2:1`, `!echo:$`) — real bash supports
this, but the two forms above (`!!`/`!n`/etc. alone, and the previous
command's own `$`/`^`/`*`/`:n` alone) cover the overwhelming majority of
real usage (`sudo !!`, reusing `!$`) on their own; and quote-aware word
splitting for the designators — real bash's own splitter treats a quoted
phrase as one word (`echo "a b" c` then `!:1` → `"a b"`), rush's uses a
plain `split_whitespace` (`!:1` → `"a`) instead.

Verified directly against real bash (via `bash -i`, isolated `HISTFILE`s)
across more than a dozen scenarios, and covered by integration tests
running the actual compiled binary in piped/interactive mode. **Effort:
S–M.**

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
- **C33 (autosuggestions)** is the standout cheap win in Tier V given
  `completion.rs` already has the `Hinter` trait wired up as a no-op.
