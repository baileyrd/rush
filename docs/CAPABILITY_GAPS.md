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

**Bottom line:** rush started out closest to **dash** — a solid, mostly-POSIX
execution core (real pipes, real job control, real forked subshells) with
almost none of the bash/ksh/zsh-family conveniences layered on top. Every
item tracked in this document (C1–C40, all five tiers) is now done, closing
that gap almost entirely — what follows is the history of how, tier by
tier, plus the narrower, individually-documented edge cases each item's own
write-up calls out as still out of scope (a fd-numbering cosmetic
difference here, a multi-stage-pipeline corner there — none of them the
kind of gap this document originally tracked). Tier I's original 6 items are done, and so now are C35 (`\$` inside
double quotes wasn't staying literal — fixed by giving the lexer a
separate, never-re-expanded `WordPart::Literal("$")` for it instead of
just stripping the backslash and leaving a bare `$` indistinguishable
from a real, unescaped one), C37 (an unknown command name used to
print a raw OS error and abort the whole script instead of reporting
status 127 and continuing — fixed for a standalone command, the headline
case; a not-found command as one stage of a multi-command pipeline is a
deliberately out-of-scope narrower remainder, needing real process-group
unwinding rush's `Command`-based spawn path can't cheaply do), and C38
(redirects to fd 3+ silently collapsed onto fd 1 — fixed via a real
per-fd `dup2` in both `redirect_stdio`, builtins, and a new `pre_exec`
`dup2` sequence in `build_stage` for real spawned children; also fixed a
genuine hazard the fix itself could have hit — a freshly opened file's
own fd is often exactly the fd being redirected to, and `dup2` on
identical fds is a no-op that would otherwise leave a stray `Drop` to
close the very fd just set up). Tier II is fully closed out too —
`local`, `getopts`, `command`/`type`/`hash`, `wait` (with its own
prerequisite, `$!`), `source`/`.`, `eval`, `exec`, and `umask` all landed
alongside `read`/`printf`/`shift`. C36 (a PATH-visibility bug in
`command`/`type`/`hash`, plus a deeper root cause — the shell never
seeded its own variable table from the inherited environment at startup,
so a *bare* `PATH=$PATH:dir` silently failed to reach a spawned child's
own environment even though internal lookups saw it) turned up while
closing out `source`; C37 while closing out `eval`; C38 while closing out
`exec`. Chasing C36 down turned up one further item — `unset` of an
inherited/exported variable didn't stop a spawned child from still
seeing it (C40), which turned out to need more than the environment fix
its own write-up anticipated: rush's own resolution of *which* program to
run consulted the real OS environment directly too, bypassing the fixed
child environment entirely, and half a dozen other "read this variable"
call sites across the codebase had the identical now-obsolete
fallback-to-`std::env` pattern once C36 started seeding `vars`
comprehensively at startup. Fixing all of it makes Tier I fully closed
out — 10 of 10, complete. `set -euo
pipefail` — the header nearly every production shell script opens with —
now works in full: `-e`, `-u` (C18), and `-o pipefail` (C19) all landed,
and `-x` (C20, xtrace) alongside them. `TERM`/`HUP` traps (C21) now fire
too — including interrupting a blocking wait immediately, the headline
case for a container's graceful-shutdown pattern. (One gap turned up
alongside `-x` — `set --`/`set args…` didn't reassign positional
parameters at all — fixed as C39, closing out Tier III completely, 5 of
5; fixing it also caught a real bug its own implementation could
otherwise have introduced, an unrecognized `set` flag that didn't stop
processing immediately.) Tier IV (bash/ksh/zsh
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
parser. History-based autosuggestions (C33) — a dimmed, greyed-out
completion of the current line from history, fish's own signature
feature — followed right behind it, turning out to be almost entirely
`rustyline`'s own ready-made `HistoryHinter` and key bindings once
`RushHelper` actually delegated to it instead of its previous no-op
`Hinter` impl. Argument- and context-aware completion (C34) — variable
names after `$`/`${`, directories-only for `cd`, variable names for
`export`/`unset`/`local`/`declare`, alias names for `alias`/`unalias`,
and `%n` job specs for `fg`/`bg`/`kill`/`wait` — closes out Tier V
completely, 3 of 3, bounded deliberately to this fixed case list rather
than a full fish/zsh-style completion-spec engine. Tiers I through III
closed out too, each while chasing down a tracked bug found mid-adjacent-
item (a `\$` quoting bug, C35; an unknown command aborting the whole
script, C37; redirects to fd 3+ collapsing onto fd 1, C38;
`set --` not reassigning positional parameters, C39) — the last of
which, C40, turned up while fixing C36 and needed more than its own
write-up anticipated: the shell's own resolution of *which* program to
run, and half a dozen other "read this variable" call sites across the
codebase, all still consulted the real OS environment directly rather
than `vars`'s own (correctly `unset`-aware) table. With that, every tier
in this document is fully closed.

**A fresh comparison pass** (this time verified live against all five
comparison shells — dash, bash, zsh, ksh93, and fish were all actually
installed and invoked directly, not just checked against documentation,
a strictly higher bar than this document's original methodology)
surfaced 33 new, previously-undocumented gaps, C41–C73, spanning all
five tiers. The headline finding was **C55: rush had no `[[ ]]` extended-test
construct at all** — no lexer tokens, no parser production, nothing;
`[[ foo = foo ]]` was "command not found," and `<`/`>` inside one were
silently misparsed as ordinary file redirections; now done (see its
write-up). Close behind it:
**C45, `readonly`/`declare -r` (read-only variables) was entirely
missing**, despite being POSIX-mandated and present in all five
comparison shells including dash — and worse than a mere missing
feature, `readonly x=1` treated `x=1` as an argument to the unrecognized
command, so the assignment itself was silently lost; now done. And **C41: `$`
(the shell's own PID), `$PPID`, and `$-` didn't expand at all**, despite
`$`/`$-` being POSIX-mandated and `$PPID` being near-universal — arguably
the highest-impact single item in the whole fresh pass, given how often
`$$` shows up in temp-file-naming idioms; C41 is now done (the first of
the fresh pass to land, and fixing it exposed and fixed a real adjacent
`set` bug — clustered flags like `set -euo pipefail` didn't parse at
all; see its write-up). The remaining items are documented tier-by-tier
below in the same style as C1–C40, each with a directly-verified repro
and an S/M/L effort estimate.

---

## Comparison matrix

A cross-section, not the full 73 below — enough to place rush relative to a
strict POSIX shell (dash), the bash family, and the interactive-first shells
(zsh, fish). ✅ full · 🟡 partial/simplified · ❌ not implemented · — not
applicable to that shell's own model.

| Capability | rush | dash | bash | ksh93 | zsh | fish |
|---|---|---|---|---|---|---|
| Real pipes / job control / forked subshells | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `#`/`##`/`%`/`%%` param. expansion | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| `read` / `printf` / `shift` / `getopts` | ✅† | ✅ | ✅ | ✅ | ✅ | 🟡 |
| `local` function-scoped vars | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `wait` / `disown` | ✅‡ | ✅ | ✅ | ✅ | ✅ | ✅ |
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
| Context-aware completion | ✅§§§ | — | 🟡 | 🟡 | ✅ | ✅ |
| History autosuggestion | ✅*** | — | ❌ | ❌ | 🟡 | ✅ |
| Native Windows job control | ❌ | — | — | — | — | 🟡 |
| `[[ ]]` extended test | ✅× | ❌ | ✅ | ✅ | ✅ | ❌ |
| `readonly` / read-only vars | ✅×× | ✅ | ✅ | ✅ | ✅ | ✅ |
| `$`/`$PPID`/`$-` special vars | ✅××× | ✅ | ✅ | ✅ | ✅ | 🟡 |

\* Done for the interactive/script job-control path; a compound as one stage
among several *inside* a `$(...)` substitution, or on non-Unix, still errors.

† All four are done, with narrower caveats: `read` (with `-r` and `$IFS`
splitting) and `printf` (sans `%e`/`%f`/`%g`) are otherwise complete;
`shift`/`getopts` are full.

‡ `wait` (`pid`/`%job`/`-n`, or none) is done, along with its `$!`
prerequisite; `disown` is done too (C64).

§ `-e`, `-u`, `-o pipefail`, and `-x` are all done; `-x`'s trace doesn't
cover a compound's own header line (`for i in 1 2`, `case a in`), only the
commands actually inside its body.

‖ `EXIT`/`INT`/`TERM`/`HUP` all fire now — including interrupting a
blocking wait immediately, not just once the foreground job finishes on
its own — and `ERR` fires on errexit's exact condition (C53; not
inherited by functions, bash's no-`errtrace` default). `DEBUG`/`RETURN` fire too (C65).

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
`k="b c"; arr[$k]=val` idiom works); `declare -p`/`-x`/`-r`/`-f`
(`-u`/`-l`/`-i` attribute transforms are done — C43);
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

*** A dimmed, greyed-out inline suggestion of the rest of the most recent
matching history entry, accepted with the right arrow at end of line —
built almost entirely on `rustyline`'s own ready-made `HistoryHinter` and
key bindings; rush adds the dimming and the wiring. A live-terminal
rendering feature (bypassed entirely when stdin isn't a real TTY, same as
every other rustyline editing feature), verified directly under a real
pseudo-terminal rather than the piped-stdin pattern used elsewhere in
this document.

§§§ Bounded to a fixed set of the highest-value cases rather than a full
completion-spec engine: variable names after a bare `$`/`${`, `cd`'s
directory-only argument, variable names for `export`/`unset`/`local`/
`declare`, existing alias names for `alias`/`unalias`, and (Unix only)
`%n` job specs for `fg`/`bg`/`kill`/`wait`. Not done: flag completion for
any builtin, per-external-command argument specs (`git <TAB>`
subcommands, `ssh <TAB>` known hosts) — the rest of what a real fish/zsh
completion *system* (as opposed to this fixed case list) provides.

× Done (C55): full lexer/parser/evaluator — split-safe/glob-safe
operands, `&&`/`||`/`!`/`( )` nesting, pattern-matching `==`/`!=` with
bash's per-part quoting rule, lexicographic `<`/`>`, arithmetic
`-eq…-ge`, `-nt`/`-ot`/`-ef`. `=~` (regex, with `$BASH_REMATCH` captures) is done too (C56). fish
has no `[[ ]]` syntax either (its own conditional model is built on
`test`/`[` plus `and`/`or`).

×× Done (C45): `readonly`/`declare -r`/`local -r` all mark the flag,
every mutation path rejects it (a bare assignment fatally, matching
bash's non-interactive abort; builtin-mediated attempts with status 1),
and `readonly`/`readonly -p` list in bash's own `declare -r` format.

××× All three expand now (C41, done): `$`/`${$}` from
`std::process::id()`, `$PPID` seeded once at startup from
`libc::getppid()` (Unix), and `$-`/`${-}` assembled from the tracked
option flags (`e`/`i`/`u`/`x`; pipefail is letterless, same as bash).
fish exposes the same information under its own differently-named
variables (`$fish_pid`, no direct `$-` equivalent), so its own model
only partially overlaps rather than matching the POSIX/bash-family
syntax directly.

---

## Summary counts

- **Tier I — correctness/POSIX risk:** 14 (14 done, 0 open — closed out again)
- **Tier II — missing standard builtins:** 17 (17 done, 0 open — closed out again)
- **Tier III — scripting-safety idioms:** 10 (10 done, 0 open — closed out again)
- **Tier IV — bash/ksh/zsh language parity:** 23 (23 done, 0 open — closed out again)
- **Tier V — interactive UX:** 9 (9 done, 0 open — closed out)

73 items tracked in total: the original C1–C40 (all done, see "Bottom
line" above) plus 33 newly-discovered items (C41–C73) from a fresh live
comparison pass against dash/bash/ksh93/zsh/fish — and all 73 are now
done. The last holdout, C71 (the right-side prompt), was
dependency-blocked on rustyline's architecture until rustyline itself
was replaced with a hand-rolled line editor (`src/editor.rs`) — see
C71's write-up.

**Update (2026-07-11): a third review pass found 57 new gaps, C74–C130,
see the "2026-07-11 review pass" section at the end of this
document. New-pass counts by tier: Tier I (correctness) 15, Tier II
(builtins) 15, Tier III (options/environment/job control) 8, Tier IV
(language parity) 10, Tier V (interactive UX) 9. Grand total tracked:
130 items, 73 done, 57 open.

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

### C35 — Backslash-escaped `$` inside double quotes isn't literal (tracked) ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh: inside `"..."`, `\$` shall
produce a literal `$` (suppressing expansion of whatever follows), same as
`\"`/`\\` are already handled. Rush used to drop the backslash but still
expand the parameter anyway — `echo "\$?"` printed the exit status instead
of the literal text `$?`, and `echo "\$FOO"` printed `$FOO`'s *value*
instead of the literal text `$FOO`. Silent wrongness, not an error, so it
fit this tier; found while verifying C13's `$!` against real bash (not
specific to `$!` — reproduced for `$?`, a plain `$FOO`, everything).

**Root cause and fix**: the lexer already special-cased `\"`/`\\`/`\$`
identically — strip the backslash, push the escaped char straight into the
double-quoted text — which is fine for `"`/`\`, but not for `$`: that text
becomes a `WordPart::Quoted` string, re-scanned by `expand.rs` for
`$`/`$(...)` *later*, so a bare `$` left behind by stripping `\$`'s
backslash is indistinguishable from a real, unescaped `$` by the time
expansion runs. The fix (`lexer.rs`) makes `\$` flush whatever quoted text
came before it, emit a separate `WordPart::Literal("$")` — never
re-expanded, by definition, exactly the same guarantee `'...'` already
gives — and then keep lexing the rest of the double-quoted span as
before. No new escape-recognition logic was needed in `expand.rs` itself;
the fix is entirely about which `WordPart` variant the lexer already had
available to represent "this stays literal."

Verified directly against real bash across the cases above plus:
composing an escaped `\$` with a real, still-expanding `$FOO` in the same
string (`"pre\$mid$FOO"` → `pre$midbar`); and the easy-to-conflate `\\$FOO`
case (a literal backslash from `\\`, followed by an ordinary, *still-
expanding* `$FOO` — not itself the `\$` escape) continuing to expand
correctly (`\bar`), confirming the fix didn't overreach into misreading a
literal backslash followed by an unrelated, unescaped `$` as the escape
case. **Effort: S.**

### C37 — An unknown command name aborts the whole script instead of failing with status 127 (tracked) ✅ done
POSIX-mandated in every comparison shell here: running a command that
doesn't resolve — a typo, something not on `$PATH` — prints an error to
stderr (bash: `command not found`) and continues the script with `$?` set
to 127. Rush used to print the raw OS spawn error (`No such file or
directory (os error 2)`) and **abort the entire script right there** — an
`echo` placed right after the bad command never even ran. Found while
diffing `eval "nonexistent_cmd"` against bash (C15), but reproduced for any
top-level mistyped command — not specific to `eval` at all, and arguably
the highest-impact item in this tier, since it fires on the single most
common shell-scripting mistake there is.

**Fix**: a new `exec::spawn_failure_status(name, &io_error)` prints the
usual message and returns the right POSIX status — 127 for
`io::ErrorKind::NotFound` ("no such command"), 126 for anything else
(permission denied, is a directory, …) — verified directly against real
bash (`126` for a non-executable file or a directory, `127` for a plain
typo; rush's own message *wording* differs, same as every other error in
this shell, but the functional status matches exactly). Wired in at both
of this shell's two `Command::spawn()` call sites (`job::spawn_pipeline`,
the Unix job-control path used for virtually everything on this shell's
primary platform, and `exec::run`, used for command substitution and the
non-Unix foreground fallback) — for a **standalone command** (not one
stage among several in a pipeline), a spawn failure now returns this
status as an ordinary `Ok(...)` result instead of propagating a hard
`Err`, so the rest of the script keeps running exactly like it does after
any other failing command — including still triggering `set -e`,
verified directly.

**Explicitly out of scope, a deliberate, documented scope-narrowing**: a
command that fails to spawn as a **non-first-or-only stage of a
multi-command pipeline** (`cmd1 | badcmd | cmd3`) still aborts the whole
script, as before. Real bash's own C-level fork-then-exec model gives
every pipeline stage a real, if short-lived, process no matter what (the
exec step is what can fail, *inside* an already-forked, already-
process-grouped child) — Rust's `std::process::Command::spawn` hides that
distinction entirely, reporting a failed exec atomically as a single
`Err` with no pid ever exposed to the caller at all. Synthesizing a fake
per-stage exit code and correctly unwinding an already-established
process group (`job::spawn_pipeline`'s `pgid`/`wait_pgid`, which expects
one real pid per stage) for this narrower case is real architectural
work this item's effort budget doesn't cover. The very same limitation
applies to **backgrounding** a standalone unknown command (`badcmd &`):
the script no longer aborts (the actual headline fix), but — for the same
reason — there's no real pid for `$!`/`jobs` to report, unlike real
bash's own short-lived one.

Verified directly against real bash across: a standalone typo (mid-script
and as the sole command), a found-but-not-executable file and a directory
(126 vs. a typo's 127), `set -e` still catching it, command substitution
(captures nothing, reports 127), and backgrounding (doesn't abort the
script; no synthetic `$!`, a documented gap). **Effort: S** for the
standalone-command fix that was this item's actual described scope.

### C38 — Redirects to any fd other than 0/1/2 silently collapse to fd 1 (tracked) ✅ done
POSIX-mandated: `cmd 3>file`, `cmd 4<&5`, `exec 3>file` (holding a
descriptor open for later) are all ordinary, if less common, shell idioms.
Rush's whole redirect machinery — both `redirect_stdio` (builtins) and
`build_stage` (real spawned children) — used to collapse any `fd` that
wasn't literally `0` or `2` into fd **1** (a `target_fd` closure's
`_ => 1` arm, duplicated in both places), so `cmd 3>file` silently
redirected the command's *stdout*, not a real fd 3. Silent wrongness, not
an error. Found while implementing `exec` (C16), which was the first
place this blocked a headline idiom (`exec 3>file`) rather than being an
edge case, but it was general — reproduced for any command, builtin or
external.

**Fix — `redirect_stdio` (builtins, in-process)**: the `target_fd`
collapse was simply deleted — `StdioGuard`'s own save/restore bookkeeping
(`Vec<(i32, i32)>`) was already keyed by plain `i32`, needing no
structural change to support fd 3+ once the artificial 0/2/else clamp was
gone. This also fixed two related bugs the same collapse was
responsible for, not called out in the original write-up: a `Dup`
redirect's *source* side collapsed exactly the same way as its
destination (`4>&3` botched fd 3, not just an unusual destination fd),
and a `Read`-mode redirect to fd 1 or 2 (`cmd 1<file`, unusual but valid)
was silently dropped entirely rather than merely collapsed.

**Fix — `build_stage` (real spawned children)**: `std::process::Command`
only exposes `.stdin()`/`.stdout()`/`.stderr()` — there's no generic
"set fd N" API for a child process. Fd 3+ redirects are now collected (in
their own source order, alongside the existing fd-0/1/2 handling) into a
new `FdAction` list — `Open(File, fd)` for a freshly opened target, `Dup
{ source, dest }` for an `fd>&target`/`fd<&target` pair where either side
is 3+ — and applied via one `pre_exec` closure (`CommandExt::pre_exec`,
the same Unix-only mechanism `job.rs` already uses for process-group
setup) that runs a plain `dup2` per entry in the child, after `fork` but
before `exec`. This composes cleanly with `job.rs`'s own separate
`pre_exec` call (multiple closures registered on one `Command` all run,
in order) and needs no new persistent shell-side bookkeeping — the
child's own inherited (then `dup2`'d) fd table entries are exactly the
kind of state `exec`'s permanent redirect form already relies on the OS
to hold, generalized from fd 0/1/2 to any fd.

**A real, general bug found and fixed along the way, not specific to fd
3+ at all**: a freshly opened file's own fd is often *exactly* the target
fd being redirected to (its "lowest available fd" allocation landing on
the very number requested) — overwhelmingly likely for fd 3+
specifically, since 0/1/2 are essentially always already open in a real
process but 3+ usually isn't. `dup2` on identical fds is a defined
no-op, so in that case the freshly opened file *is* the live redirect
already — but the original code still let it drop normally at the end of
its match arm, which closed the very fd the redirect had just set up.
Fixed in `redirect_stdio` by detecting this case and `mem::forget`-ing
the file instead (ownership has effectively passed to the fd table entry
itself); `build_stage`'s `pre_exec`-closure design was naturally immune
(the files it captures are never dropped in the parent before `exec`
replaces the child's whole process image).

**Also fixed, found during the same pass and not called out in the
original write-up**: the lexer's `<&n` (read-side fd duplication, e.g.
`4<&5`) didn't parse *at all* — only `>&n` did, since `lex_gt_op` checked
for a following `&` but there was no equivalent check on the `<` side, at
either of the two places `<` is lexed (the bare `'<'` top-level dispatch,
and `lex_redirect`'s explicit-fd-prefixed path, e.g. `4<...`). A new,
shared `lex_lt_op` (mirroring `lex_gt_op`) fixes both. `RedirOp::Dup`
doesn't need a direction flag — `<&n`/`>&n` both just mean "`dup2` this
fd onto that one," and which arrow spelled it doesn't change that.

Verified directly against real bash across: a standalone `cmd 3>file`
and `4<&5`-style chains (including multi-hop, `3<file 4<&3 <&4`) for
both a builtin (`read`/`echo`) and a real external command (`cat`),
`exec 3>file`'s permanent form, and the exact self-dup coincidence bug
above (which real bash, forking a real process per redirect rather than
opening files in-process the way rush's builtins do, never hits at all —
purely a rush-internal hazard this fix specifically addresses). Adds 2
new lexer unit tests plus 3 new integration tests exercising both the
builtin and external-command code paths; full suite and clippy stay
clean (Windows cross-compile checked too, since `build_stage` itself
runs on every platform even though the new `pre_exec` logic is
`#[cfg(unix)]`). **Effort: M**, as estimated.

### C40 — `unset` of an inherited/exported variable doesn't stop a spawned child from still seeing it (tracked) ✅ done
Found while fixing C36: `exec::build_stage`/`run_foreground`'s
`command.envs(vars::exported())` only *added/overrode* entries on top of
whatever `std::process::Command` already inherits from rush's own real OS
environment by default (no `env_clear()` anywhere) — it never *removed*
a key. So `unset`-ing a variable that came from the inherited process
environment (`PATH`, or anything else genuinely exported) only deleted
rush's own internal record of it; the child process still inherited the
original OS-level value regardless. `unset PATH; some_command_only_on_the_
now-supposedly-unset-PATH` still found and ran it — real bash instead
fails with "command not found" (status 127), since its child truly no
longer has `PATH` at all.

**Fix, child environment**: `command.env_clear()` before
`command.envs(vars::exported())`, at both spawn sites
(`build_stage`, real spawned children, and `exec_cmd`'s process-
replacement form). Since `main.rs` already seeds every inherited
environment variable into `vars` at startup (C36), `vars::exported()` is
now a complete, accurate picture of what a child's environment should
be — rebuilding it from scratch, rather than layering onto
`Command`'s default full-environment inheritance, is what makes `unset`
actually take effect.

**A deeper piece the environment fix alone didn't cover, found while
verifying the headline `unset PATH; some_command` reproduction still
didn't work after the environment fix**: `std::process::Command::new(name)`
resolves a bare (no `/`) program name to an executable using the *real*
process environment's own `PATH` at spawn time — a lookup that happens in
the parent, before `fork`, entirely independent of whatever's configured
for the child via `.envs()`/`env_clear()`. So even with the child's own
environment now correctly excluding `PATH`, rush's *own* attempt to
*locate* the command still silently succeeded via the untouched real OS
environment. Fixed with a new `exec::resolve_program`: a bare name is
resolved via `builtins::resolve_in_path` (rush's own, `vars`-aware `$PATH`
search, already used for `command -v`/`type`/`hash`/`source`) to its
absolute path, so `Command`'s own built-in search never runs at all; a
name that doesn't resolve there gets a trailing `/` appended
(guaranteed, verified directly, to fail with `NotFound` — and, having a
`/`, also skips `Command`'s own search), routing it through the exact
same not-found handling C37 already gives a missing command, rather than
a second error path. A direct path (already containing `/`) is left
completely alone, preserving C37's existing 126-vs-127 distinction for
that case unchanged.

**A related, broader architectural issue surfaced by this same
investigation, fixed alongside it**: several other "read this variable"
call sites (`expand.rs`'s central `var_raw`, `arith.rs`'s arithmetic
variable lookup, `PS1`/`PS3`/`PS4`'s own prompt lookups, `IFS`'s
field-splitting lookup, and a bare `cd`'s `$HOME` fallback, which
previously didn't consult `vars` *at all*) all had the identical
`vars::get(name).or_else(|| std::env::var(name).ok())` pattern — a
fallback that made sense *before* C36 (when `vars` might genuinely not
know about an inherited variable yet), but became actively wrong once
C36 started seeding `vars` from the full environment at startup: it
silently resurrected an inherited variable's original value after
`unset`, for every one of these reads, not just spawning. All of them
now use `vars::get` alone. `~` (tilde expansion) is a deliberate,
verified-directly exception: real bash's own `~` *does* follow a plain,
unexported `HOME=/custom` reassignment, but *keeps resolving* even after
`unset HOME` (falling back to its own OS-level notion of the user's home)
rather than breaking — `expand.rs`'s `home_dir` keeps a `std::env`
fallback specifically for that reason, checking `vars::get("HOME")`
first so an assignment is honored (previously it wasn't, either).

Explicitly out of scope, a narrow, deliberate scope-narrowing found along
the way: `unset PS4` (a shell-internal special variable bash itself
pre-populates with `"+ "` at startup, not one inherited from the OS
environment) still traces with rush's own hardcoded `+ ` default,
whereas real bash's own trace prefix genuinely goes empty — verified
directly. This is a different root cause than the rest of this item
(a *shell-internal* default that bash treats as real, mutable state
rather than a read-time fallback) and doesn't affect anything
environment/`PATH`-related; fixing it would mean seeding `PS1`/`PS4`'s
own bash-matching defaults into `vars` at startup the same way C36 seeds
the real environment, a small enough change to be its own follow-up
rather than folded into this one.

Verified directly against real bash across: `unset PATH` breaking a bare
command name while a direct path keeps working, `PATH` reassignment
still resolving new commands correctly, `unset IFS` reverting to default
whitespace splitting, a bare `cd` following a plain `HOME=` reassignment
and erroring after `unset HOME` (matching bash's own message
functionally, wording aside), and `~` continuing to resolve after
`unset HOME` while still following an assignment. **Effort: M**, as
estimated — ended up needing the parent-side resolution fix and the
broader fallback cleanup alongside the originally-scoped environment fix
to fully close the gap the reproduction actually described.

### C41 — `$$`, `$PPID`, `$-` don't expand ✅ done
POSIX-mandated (`$$`, `$-`); `$PPID` a near-universal extension. Present
in bash/dash/ksh/zsh (`$PPID` not in dash). Rush's `$`-scanner
(`expand.rs`) has no arm for a second `$`, an unnamed-variable `PPID`
read, or `$-`, so all three fall through to being treated as a bare `$`
followed by ordinary text — `echo $$` prints the literal two-character
string `$$`, not the shell's pid; `$PPID` and `$-` silently expand to
empty. Silent wrongness, not an error, and `$$` in particular is one of
the most common idioms in real scripts (`tmpfile=/tmp/x.$$`, `kill $$`),
making this arguably the highest-impact single item in this whole
document — surprising precisely because it's this basic. **Effort: S** —
`$$` is `std::process::id()`; `$PPID` is `libc::getppid()` (Unix), seeded
once; `$-` is a one-line assembly from the flags `vars.rs` already tracks
for `errexit`/`nounset`/`xtrace`/`pipefail` (plus `i` for interactive
mode).

Implemented, exactly per the sketch above. `$$` and `$-` got their own
arms in `expand.rs`'s `$`-scanner (before, both fell through to the
literal-`$` default), plus the braced spellings `${$}`/`${-}` in
`expand_braced`'s special-parameter table — all four verified directly
against real bash. `$PPID` needed no scanner change at all: `main.rs`
seeds it once at startup via `libc::getppid()` as an ordinary
non-exported shell variable (bash doesn't export it either), placed
*after* the environment-seeding loop so a stale `PPID` exported by some
parent process can't shadow the real value — bash wins that same race the
same way (verified: `PPID=12345 bash -c 'echo $PPID'` prints the real
ppid). `$-` assembles from `vars::option_flags()`: `e`/`u`/`x` from the
existing errexit/nounset/xtrace thread-locals plus `i` from a new
interactive flag set on REPL entry; `set -o pipefail` contributes no
letter, matching real bash (verified: no new letter appears in `$-`
there either).

Verifying `$-` exposed a real, separate pre-existing bug in `set`
itself: **clustered short flags didn't parse at all** — `set -eu`, and
even `set -euo pipefail`, the near-universal script header this
document's own Tier III narrative celebrates landing, errored with
`set: -euo: not supported`; only one-flag-per-word spellings (`set -e;
set -u`) ever worked, and no test had ever combined them. Fixed in
`set_cmd` (`builtins.rs`): a `-`/`+` word's letters now apply in
sequence, with `o` consuming the next word as its option name even
mid-cluster. Probing real bash for the error path surfaced a second
subtlety: bash applies *nothing* when any flag in the invocation is
invalid (`set -eu -z` leaves errexit and nounset both off — verified
directly), so `set_cmd` now collects flag changes and applies them only
once the whole invocation validates. That rollback matters more than it
looks: partial application would have turned errexit on before `set`'s
own failure returned nonzero, errexit-killing the shell on the spot
(reproduced against the naive left-to-right implementation before the
fix).

Verified against bash (and dash/ksh where applicable, all installed and
invoked directly): `$$`/`${$}` print the shell's real pid (child of the
invoking process — checked exactly in the integration test, where the
invoker is the test process itself), `tmpfile=/tmp/x.$$` composes,
`$PPID` matches the invoker's pid, `$-` is empty by default and
gains/loses letters through `set -eu`/`set +e`, `set -euo pipefail`
works, and `set -e a b` both applies the flag and reassigns `$1`.
Regression tests: a unit test for `option_flags`' assembly order and
pipefail's letterlessness (`vars.rs`), plus five integration tests
(`tests/exec_behavior.rs`) covering `$$`/`${$}`/quoted `$$`, `$PPID`
(including the stale-inherited-`PPID` shadowing case), `$-` lifecycle,
clustered `set` flags, and the invalid-flag full-rollback semantics.

### C42 — POSIX bracket character classes (`[[:alpha:]]`, `[[:digit:]]`, …) in globs/`case` ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh (this one genuinely works in
dash, unlike most of Tier IV's bash-family extensions — it's a POSIX
baseline glob feature, not a convenience). `glob.rs`'s bracket-expression
parser (`parse_class`) only understands single characters and `c-c`
ranges; `[:alpha:]`-style named classes inside a bracket are misparsed as
their own literal characters, so `case 5 in [[:digit:]]) …` silently
never matches and `ls [[:alpha:]]*` silently matches nothing (rather
than erroring) — silent wrongness affecting filename globbing, `case`,
and the `${v#pat}`-family pattern-removal operators alike, since they all
share the same matcher. **Effort: S–M** — localized to `glob.rs`'s
bracket parser: recognize `[:name:]` and map the standard POSIX class
names to predicates.

Implemented, exactly per the sketch: `parse_class` now recognizes
`[:name:]` members, with the bracket's member list generalized from
plain `(char, char)` ranges to a `ClassItem` enum (`Range` |
`Named(predicate)`), so named classes mix freely with ordinary
members/ranges (`[[:alpha:]5]`) and negate correctly (`[![:digit:]]`).
All twelve standard names map to predicates: `digit`/`xdigit` are
ASCII-only even in a Unicode locale (matching bash), the letter-ish
classes use Rust's Unicode-aware predicates (agreeing with bash under
the usual UTF-8 locales). Because `case`, filename globbing, and the
pattern-removal operators all share this one matcher, one fix covered
all three surfaces — each verified separately.

Two edge cases were probed char-by-char against real bash rather than
assumed: a *properly-delimited unknown name* (`[[:bogus:]]`) is a member
that matches nothing, not a parse error; and an *unclosed* `[:`
(`a[[:digit]`) triggers a genuine bash quirk — bash drops the `[` itself
and keeps `:digit` as ordinary members (matches `ad`/`a:`, not `a[`),
where dash keeps the `[` as a member too. Rush follows bash, this
document's reference shell, on both.

Verified against real bash (and dash for the POSIX-baseline cases; both
invoked directly on identical fixture files): `a[[:digit:]]`,
`a[[:alpha:]]`, `a[[:upper:]]`, `a[[:lower:]]`, `a[[:punct:]]`,
`[[:alpha:]]*`, the mixed/negated forms, both edge cases, `case 5 in
[[:digit:]])`, and `${v%%[[:digit:]]*}` — byte-identical output on every
pattern. Regression tests: two unit tests in `glob.rs` (the full class
table plus the edge cases) and two integration tests in
`tests/exec_behavior.rs` (`case`/pattern-removal, and filename globbing
against real fixture files).

### C43 — `declare -u` / `-l` / `-i` attributes are silently ignored ✅ done
Present in bash/zsh (as `typeset`), and ksh93 (`typeset -u/-l/-i`); no
POSIX/dash equivalent. The `declare`/`local` flag parser (`expand.rs`)
recognizes only `-a`/`-A`; any other flag (`-u`, `-l`, `-i`, `-r`, `-n`,
`-x`, `-p`) is misparsed as a bare variable name to declare (a no-op),
and the real assignment proceeds as an ordinary scalar with no
transform. `declare -u u=hello; echo $u` prints `hello`, not `HELLO`;
`declare -i n; n=2+3; echo $n` prints the literal `2+3`, not `5`. Silent
wrongness — no error, no diagnostic, just a wrong value — found alongside
the array/`declare` gap survey below (C60/C62), which share the same
root cause (no attribute field on `Var` beyond `exported: bool`).
**Effort: M** — needs an attribute flag on `Var` (`vars.rs`), applied at
every assignment (`-u`/`-l` transform the value; `-i` routes the RHS
through the arithmetic evaluator `arith.rs` already provides).

Implemented, with one deliberate deviation from the sketch: attributes
live in their own `ATTRS: HashMap<String, Attrs>` map (`vars.rs`) rather
than as a field on `Var`, because an attribute can be declared on a name
with no value yet (`declare -i n; n=2+3` — the item's own headline
repro) and bash keeps the variable *genuinely unset* in that state
(`${n+set}` stays empty); `VARS` has no unset-but-existing
representation, and inventing one would have rippled through every
exhaustive `VarValue` match in the codebase. The transforms hook the
central assignment paths (`set`, `set_exported`, `append_scalar`,
`set_array` per-element, `array_set`, `assoc_set`), so every assignment
form — plain, `+=`, array literal, one-element — transforms.

Semantics were probed against real bash case-by-case rather than
assumed, and several turned out non-obvious: attributes are **not
retroactive** (`x=abc; declare -u x` leaves `abc`; the next assignment
maps); `-u` and `-l` **displace each other across separate
declarations** but **cancel when clustered together** (`declare -lu
w=Abc` leaves `Abc` untouched — a real bash quirk, matched); under `-i`,
`+=` becomes **arithmetic addition** (`declare -i n=5; n+=3` → 8), an
unresolvable name evaluates to 0, and a syntax error keeps the old value
(bash also returns status 1 there — the diagnostic is matched, the
status is an accepted simplification, documented in the code); `unset`
drops attributes along with the value; and a `local -u` binding starts
from its own declared attributes (not the shadowed outer variable's)
with the outer attribute state restored on return — local frames now
capture prior attributes alongside prior values.

The flag parser in `expand.rs` (shared by `local`/`declare`) now accepts
clustered flag words over `-a/-A/-u/-l/-i` (`declare -ui n`), threading
the attributes through a new `Command::decl_attrs` field to
`local_from_decls`/`declare_from_decls`. A word with any *other* letter
still ends flag parsing exactly as before, keeping `-r`/`-n`/`-x`/`-p`
(C45/C62/C48) no worse than they were. Verified against real bash on
all of the above plus ksh93/zsh's `typeset -u` equivalents for the
headline cases; regression tests: three unit tests (`vars.rs`) and three
integration tests (`tests/exec_behavior.rs`).

### C44 — `trap` with a numeric or `SIG`-prefixed signal spec registers but never fires ✅ done
POSIX explicitly permits the numeric form; present in bash/dash. Multiple
signal *names* in one `trap` call already work correctly (`trap 'cmd'
INT TERM`) — this is specifically the numeric (`trap 'cmd' 15`) and
`SIG`-prefixed (`trap 'cmd' SIGTERM`) spellings. `trap_cmd`
(`builtins.rs`) stores the signal spec verbatim as a lookup key with no
normalization, but `trap.rs`'s delivery-side check only ever looks up the
canonical bare name (`"TERM"`, `"HUP"`) — so a trap registered under
`"15"` or `"SIGTERM"` is silently orphaned: the signal arrives, the
process takes the default disposition, and the registered handler never
runs, with no error at registration time either. **Effort: S** — a
normalization step in `trap_cmd` (numeric → canonical name via the same
table `kill`'s signal parsing already needs, strip a leading `SIG`)
before the name is used as `trap.rs`'s lookup key.

Implemented: new `trap::normalize_signal_spec` collapses numeric (`15` →
`TERM`, `0` → `EXIT`, per POSIX), `SIG`-prefixed (`SIGTERM`), and
lowercase (`sigterm`, `term` — bash accepts these too, verified)
spellings to the canonical bare name delivery keys on, backed by a
22-entry name↔number table (the x86-64 Linux numbers, the same ones
bash's own `trap -l` shows there). `trap_cmd` (`builtins.rs`) normalizes
both registration *and* removal (`trap - 15` now removes a trap
registered as `TERM`), and an invalid spec is finally an error at
registration time — `trap: BOGUS: invalid signal specification`, status
1 — rather than a silently-orphaned entry. Two adjacent bash behaviors
were probed and matched while here: an invalid spec does *not* block the
other specs in the same call from registering (`trap 'cmd' BOGUS TERM`
errors and still registers `TERM`, verified directly), and the `trap`
listing prints real signals `SIG`-prefixed with `EXIT` bare
(`trap -- 'echo T' SIGTERM`), which rush's listing used to get wrong by
printing the raw stored key.

Verified against real bash across: `trap 'cmd' 15` / `SIGTERM` /
`sigterm` all firing on a real delivered `SIGTERM`, `trap 'cmd' 0`
firing at exit, `trap - 15` restoring the default disposition (shell
dies 143, matching bash), both invalid-spec cases (status, diagnostic,
valid-spec-still-registers), and the listing format. Regression tests:
one unit test (`trap.rs`, the normalization table) and three integration
tests (`tests/exec_behavior.rs`) covering firing via every spelling,
invalid-spec handling, and listing output.

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

### C36 — `command -v`/`type`/`hash` don't see in-shell `PATH` changes (tracked) ✅ done
Found while fixing C14's own PATH search (see above): `builtins::resolve_in_path`
(backing `command -v`/`command -V`/`type`/`hash`) and `completion.rs`'s
`$PATH`-scanner all called `std::env::var_os("PATH")` directly — the *real*
OS process environment — rather than the shell's own `PATH` variable. A
script that did a plain `PATH=$PATH:dir` assignment and then ran
`command -v tool`/`type tool`/`hash tool` for something in `dir` got a
false "not found". Fixed with the same one-line change as C14's
(`vars::get("PATH").or_else(|| std::env::var("PATH").ok())`), applied at
each of the two remaining call sites.

**A deeper root cause turned up while verifying "actually running `tool`
works fine" (this doc's own original claim, made when C36 was first
tracked) — it doesn't, for the case that matters most**: a *bare*
`PATH=$PATH:dir` (no `export` keyword) is exactly the same reassignment a
production script would actually write, and rush had never seeded its own
variable table from the inherited process environment at startup. So the
first assignment to `PATH` (or any other already-exported, OS-inherited
name) created a *brand-new* internal entry marked `exported: false` —
`vars::set`'s existing-entry path already correctly preserves whatever
`exported` flag is there (verified by an existing test,
`set_get_unset_and_export`), but there was nothing to preserve, since
`PATH` had never been recorded as exported in the first place. The result:
internal lookups (this fix, `command -v`/`type`/`hash`, and `expand.rs`'s
own `$PATH` reads) saw the update correctly, but `exec::build_stage`'s
`command.envs(vars::exported())` — which only *adds/overrides* entries on
top of `Command`'s default full-environment inheritance, never removes —
silently fed the spawned child the *original*, unextended `PATH` instead.
Fixed in `main.rs`: at startup, before any rc file or script runs, every
inherited environment variable is registered via `vars::set_exported`,
matching real bash's own rule that an environment-inherited variable
stays exported through a later plain reassignment. Verified directly
against real bash: `PATH=$PATH:dir; command -v tool; type tool; hash
tool; tool` now matches bash's output and exit status exactly, including
the actual spawn (previously `tool` alone still failed with the raw OS
spawn error even after the `command -v`/`type`/`hash` half of this fix).

Chasing this down turned up one further, narrower bug — deliberately left
for its own item rather than folded into this one's effort budget: `unset`
of an inherited/exported variable doesn't stop it from reaching a spawned
child either (only rush's own record is deleted; nothing calls
`std::env::remove_var` or blocks `Command`'s default inheritance) — see
C40. **Effort: S** for the two-call-site PATH fix; the startup-seeding fix
alongside it turned out to be a similarly small, contained change.

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
used to print a raw OS error and *abort the entire script* instead of
reporting exit status 127 and continuing, the way every POSIX shell does.
Discovered while diffing `eval "nonexistent_cmd"` against bash, but
reproduced with a plain top-level typo too. Fixed separately as C37 (see
Tier I).

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

### C45 — `readonly` / `declare -r` (read-only variables) entirely missing ✅ done
POSIX-mandated special builtin; present in every comparison shell,
**including dash** — this is a POSIX baseline feature, not a bash-family
extension, the same class as `read`/`printf`/`shift` this tier already
closed out. `readonly` isn't registered as a builtin at all: `readonly
x=1` prints `command not found` and — worse — since it's then resolved
as an ordinary external-command invocation, `x=1` is treated as an
argument to that missing command rather than an assignment, so `x` isn't
even set. There is no notion of a read-only variable anywhere in
`vars.rs` (no flag beyond `exported: bool`), so `declare -r` has the
exact same "flag silently swallowed" problem as C43. **Effort: M** — a
`readonly: bool` flag on `Var`, checked and rejected (with a nonzero
status, matching every comparison shell) by `vars::set`/`assign`/
`unset`/array-mutation; a new `readonly` builtin mirroring `export`'s own
structure (plus `readonly -p` listing, `readonly -a`/`-A`), and wiring
`declare -r`/`local -r` to the same flag.

Implemented on C43's attribute machinery: `readonly` is a new field on
`Attrs` (so it can mark a still-unset name — `readonly z; z=1` errors
while `${z+set}` stays empty, verified against bash), enforced by a
shared `readonly_rejected` guard on every mutation path in `vars.rs`
(`set`, `set_exported`, `append_scalar`, whole-array/assoc replacement,
element writes, `+=` merges) plus a refusal in `unset`. The `readonly`
builtin routes through the same decl path as `local`/`declare` (so
`readonly arr=(a b)` array literals survive, and `-a`/`-A` compose);
`declare -r`/`local -r` reach the same flag via the C43 flag cluster —
with `-r` deliberately installing *after* the initializer applies
(unlike `-u`/`-l`/`-i`, which install before), so `readonly x=1` /
`declare -r x=5` / `local -r v=5` can each still set their own value.
`readonly`/`readonly -p` list every read-only name in bash's own
`declare -r x="1"` format (`-ar`/`-Ar` for arrays, bare for unset).

The fatality split was probed against real bash case-by-case, and it's
sharper than expected: a *bare assignment* to a readonly name (`x=2`,
`x+=2`, `arr[0]=c`, a readonly `for` variable) **aborts the whole
non-interactive script** (status 1) — while *builtin-mediated* attempts
(`unset x`, `export x=2`, `local x`, `readonly x=9`) fail with status 1
and the script continues. Rush matches both halves: the fatal path
rides the same error channel as an expansion failure; the builtin paths
pre-check and report. Two more probed subtleties: a bare `export x` on
a readonly name succeeds (it only adds the export flag), and a *prefix*
assignment (`x=2 cmd`) errors but still runs the command with the
refused assignment dropped from the child environment — bash does
exactly this (verified with a child echoing `$x`).

Verified against real bash across all fourteen probe scenarios (and
dash for the POSIX-baseline abort). Regression tests: two unit tests
(`vars.rs` — mutation rejection, listing format) and four integration
tests (`tests/exec_behavior.rs` — assign-and-lock plus fatality,
builtin-mediated non-fatality, listing/`declare -r`/`local -r`, prefix
assignment).

### C46 — `ulimit` entirely missing ✅ done
Present in every comparison shell **including dash** — like C45, this is
a POSIX-family baseline (XSI-mandated), not a bash-only convenience.
Container/daemon/CI scripts routinely open with `ulimit -n`/`ulimit -c
0`/`ulimit -s`; its total absence (`command not found`) blocks that whole
class of operational scripts outright. **Effort: M** — a new builtin
over `libc::getrlimit`/`setrlimit`, with the resource-letter table
(`-n`/`-c`/`-s`/`-f`/`-u`/`-v`/…), soft-vs-hard (`-S`/`-H`), `-a` (dump
all), and the `unlimited` keyword — broad surface but mechanically
straightforward, no interaction with the rest of the shell.

Implemented per the sketch: a 15-resource table (letter, `RLIMIT_*` id,
bash's own `-a` label, unit scale — 512-byte blocks for `-c`/`-f`,
kbytes for the memory sizes, matching bash's reporting units), over real
`getrlimit`/`setrlimit`. Reading reports the soft limit unless `-H`;
setting applies to both limits unless `-S`/`-H` narrows it (verified:
`ulimit -S -n 512` lowers only the soft limit, `-H -n` still shows the
original hard one); `unlimited` maps to `RLIM_INFINITY`; bare `ulimit`
is `-f`, same as bash; a set limit is inherited by spawned children
(verified via a real child `/bin/sh` reporting its own `-n`). Error
paths match bash: unknown flag → usage, status 2; non-numeric limit →
status 1. The Linux-only resources (`-e`/`-i`/`-q`/`-r`/`-x`) are
cfg-gated so non-Linux Unix builds keep the portable ten. Accepted,
documented narrowings: bash's read-only `-p` (pipe size) and
`-b`/`-k`/`-P`/`-R`/`-T`, and the `hard`/`soft` keywords as limit
operands.

**A real, broader pre-existing gap found while verifying**: a sole
*builtin* (or shell function!) inside `$(...)` was spawned as an
external command — `$(umask)`, `$(type x)`, `$(myfunc)`, and C46's own
`$(ulimit -n)` all failed with "command not found" unless an external
twin happened to exist on PATH (which is why `$(pwd)`/`$(echo …)` had
always *seemed* fine — `/bin/pwd` was doing the work). Fixed in
`capture_pipeline_expanded` (`exec.rs`): a sole builtin/function stage
now captures in-process via the same fork-with-fd1-on-a-pipe scheme
`capture_compound` already uses — a real subshell, which is also bash's
own `$(...)` semantics (side effects like `$(cd /tmp)` don't escape,
verified). Remaining narrower limitation, deliberately documented
rather than silently wrong: *multi-command* substitutions still run
each pipeline separately in the parent's context, so `$(cd /tmp; pwd)`
prints the parent's cwd rather than `/tmp` — the "whole substitution is
one subshell" architecture is its own future item.

Verified against real bash: `-n`/`-c`/default-`-f` values byte-identical,
the full `-a` dump line-identical over the implemented set, both error
paths, `-S`/`-H` split, child inheritance. Regression tests: one
integration test covering read/set/inherit/`-S`-vs-`-H`/`-a`/both error
paths, plus the substitution fix is exercised by `$(ulimit -n)` inside
it.

### C47 — `command -p` (default-`$PATH` form) not supported ✅ done
POSIX-mandated; present in bash/dash. `command -v`/`command -V`/the
function-bypass form (`command name`) are all done (C12) — this is
specifically the `-p` flag, which searches a default, hardcoded PATH
instead of the shell's own (possibly compromised/customized) one, for
security-conscious or portable scripts that don't trust an inherited
`$PATH`. Rush's `command` builtin treats `-p` as the command name itself
rather than a flag, so `command -p echo hi` reports `-p: command not
found`. **Effort: S** — parse the `-p` flag and route the lookup through
a hardcoded default path (e.g. `confstr(_CS_PATH)`, or a fixed
`/usr/bin:/bin`) instead of `vars::get("PATH")`.

Implemented in both halves of `command`'s split brain: the *lookup*
forms (`command -pv`/`-pV`, clustered or separate — bash accepts both,
verified) route file resolution through a new
`resolve_in_default_path` (`/bin:/usr/bin`, the same value bash's own
`confstr(_CS_PATH)` yields on Linux, checked via `getconf PATH`), while
aliases/functions/builtins keep their usual precedence; and the
*execution* form (`command -p name …`, handled by `exec.rs`'s
`command_bypass`) pins argv[0] to its default-path resolution as an
absolute path before the spawn, so the shell's own `$PATH` can't sway
it — verified with `PATH=/nowhere; command -p ls` running fine. A
builtin still wins over a default-path file (`command -p echo` runs the
builtin), same as bash. A name found nowhere on the default path takes
the ordinary 127 path — and fixing that surfaced a small cosmetic bug
in the shared diagnostic: the synthetic trailing `/` that
`resolve_program` (and now `command_bypass`) appends to force a clean
NotFound was leaking into the "command not found" message; it's now
stripped there.

Verified against real bash for every form above (including the
`PATH=/nowhere` isolation both ways and the 127 status). Regression
test: one integration test covering execution, both lookup spellings,
builtin precedence, and the clean not-found diagnostic.

### C48 — `type -a` (list every match, not just the first) not supported ✅ done
Present in bash/ksh93/zsh (not dash, which has no `type -a`). `type`/
`type -t` (C12) already report the single highest-precedence match
(function, then builtin, then `$PATH`); `-a` additionally lists every
*other* match too — the standard way to check whether a name is shadowed
(e.g. both a builtin and a same-named `$PATH` executable). Rush parses
`-a` as a name to look up rather than a flag, so `type -a echo` reports
`-a: not found` alongside `echo`'s own single match, silently never
showing the shadowed alternatives. **Effort: M** — `type`'s classifier
needs to keep going past the first hit and scan every `$PATH` directory
for additional matches, not stop at the first.

Implemented: a new `classify_all` alongside the existing single-hit
classifier — alias, keyword, function, builtin in precedence order,
then *every* `$PATH` directory's match in order (duplicate directories
deliberately not deduped: real bash lists `ls` twice for
`PATH=/bin:/usr/bin:/bin`, verified directly, and rush now matches
byte-for-byte). `type`'s flag parsing generalizes to clustered
`-a`/`-t` words (`type -at echo` → `builtin`/`file`/`file`, same as
bash). One accepted narrowing, documented: for a function, bash's
`type -a` prints the full function *body* after the header line; rush
keeps its existing one-line `f is a function` form. Verified against
real bash byte-identically for the builtin+files, `-at`, and
duplicate-directory cases; not-found stays status 1. One integration
test covers all of the above.

### C49 — `typeset` (ksh/zsh's own spelling of `declare`) isn't registered at all ✅ done
ksh93 has *only* `typeset` (no `declare` at all); zsh and bash accept
both as synonyms. Portable ksh/zsh-targeting scripts use `typeset`
exclusively, and get a flat `command not found` from rush today — even
for the `-a`/`-A` array forms `declare` itself already supports, since
`typeset` doesn't route anywhere. **Effort: S** — add `"typeset"`
alongside `"declare"` at the one dispatch point that already recognizes
`declare`/`local` (`expand.rs`) and the builtin name table
(`builtins.rs`); everything `declare` already supports (and everything
future work adds to it, including C43/C45's attribute flags) becomes
available under `typeset` for free.

Implemented exactly per the sketch — `"typeset"` added at the decl-word
dispatch (`expand.rs`), the builtin dispatch (`exec.rs`, routing to the
same `declare_from_decls`), and the builtin name table. Verified the
prediction held: the C43 attribute transforms (`typeset -u u=hello` →
`HELLO`, `typeset -i n; n=2+3` → `5`), both array forms (`-a`/`-A`),
and C45's `-r` (readonly, with the same fatal-assignment semantics) all
work under `typeset` with zero additional code, matching ksh93/zsh's
own `typeset -u` output directly. `type typeset` reports a shell
builtin. One integration test covers all of it.

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

### C39 — `set -- args…` / `set args…` doesn't reassign positional parameters (tracked) ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh. The standard way to
reassign `$1`/`$2`/…/`$#` mid-script — the textbook idiom right after
`getopts` finishes (`shift $((OPTIND - 1)); set -- "$@"` to drop the
parsed flags) — or to split a string into positional fields (`set -- $line`).
Rush's `set` builtin used to only recognize its flag-toggling forms
(`-e`/`-u`/`-x`/`-o pipefail`); any other argument, including a bare `--`,
was rejected outright (`set: --: not supported`, status 1) rather than
becoming the new `$1`/`$2`/…. Found while verifying C26 (`select`
without an `in` clause iterating `"$@"`) — needed a way to *set* `"$@"`
for the test and discovered there wasn't one; general, not specific to
`select` at all.

**Fix**: a new `vars::set_positional(args)` reassigns just `$1`…/`$#`,
leaving `$0` untouched (unlike the existing `set_args`, used only for a
script's own initial argv, which does set `$0` too). `set_cmd`
(`builtins.rs`) now recognizes two triggers for it, matching real bash
exactly: an explicit `--` (everything after becomes positional, even
text that looks like a flag — `set -- -x` makes `$1` the literal `-x`,
not the xtrace flag), and a bare first word that isn't `-`/`+`-prefixed
(`set a b c` works with no `--` at all) — both consume the rest of the
argument list as the new positional parameters. A flag preceding either
trigger still applies first (`set -e -- a b c` both turns on `errexit`
*and* reassigns).

**A real bug fixed along the way, in behavior this item's own
implementation could otherwise have newly introduced**: an unrecognized
flag (`set -z a b`) or an invalid `-o`/`+o` option name must be a hard
stop — verified directly that real bash leaves `$1`/`$2` completely
untouched in both cases, rather than treating whatever follows the bad
flag as the new positional parameter list. The pre-existing code for
both error cases only set a status flag and *kept looping* rather than
returning immediately; that was harmless before (there was nothing past
it to accidentally trigger), but would have let a genuinely invalid
`set` invocation silently corrupt `$1`/`$2`/`$#` once positional
reassignment existed. Both error paths now return immediately.

Verified directly against real bash across: `set -- args`, the
no-`--`-needed bare form, `set --` alone (clears positional parameters),
`$0` staying untouched, `--`-then-flag-looking-text staying literal, a
preceding flag still applying, the textbook post-`getopts` idiom, and
both hard-error cases leaving `$1`/`$2` alone. Not matched, an accepted
cosmetic difference: rush's own exit status for these `set` error cases
(1) versus real bash's (2) — an existing convention throughout this
shell (own phrasing/status for its own error conditions rather than
mirroring bash's internal ones), not something this item changed.
**Effort: S**, as estimated.

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

### C50 — `set -C` (`noclobber`) and the `>|` override ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh. A real scripting-safety
idiom in the same family as `set -euo pipefail` — refuses to let an
ordinary `>` redirect silently truncate/overwrite an existing file,
`>|` the explicit escape hatch when overwriting is actually intended.
Rush's `set_cmd` rejects `-C` outright (`not supported`, status 1) — the
option doesn't exist — and `>|` isn't recognized by the lexer at all
(`RedirOp` has no clobber-override variant), erroring with `expected
filename after ">"`. **Effort: M** — a `noclobber` flag in `vars.rs`
(mirroring `errexit`/`nounset`), an existence check before an ordinary
`Write`-mode redirect opens its target, a new `RedirOp::Clobber`
variant, and lexer support for the `>|` token.

Implemented per the sketch, all four pieces: a `NOCLOBBER` thread-local
(mirroring `errexit`'s, toggled by `set -C`/`set +C`, surfacing as `C`
in `$-`); a new `RedirOp::Clobber`/`RedirMode::Clobber` pair with `>|`
lexed in `lex_gt_op` (so the explicit-fd form `2>| file` rides along
free); and the enforcement centralized in `exec::open_write`, which now
takes the mode — a plain `>` under noclobber refuses an existing
*regular* file, while writing to an existing device (`> /dev/null`)
stays fine, per POSIX and verified against bash. `>>` and `>|` are
exempt; `&>` honors noclobber too (probed — bash refuses there as
well). One inherited (not new) divergence, documented: rush treats any
failed redirect open as fatal to the script where bash fails the one
command with status 1 and continues — noclobber refusals inherit that
pre-existing behavior; the original file's content survives either way.

Verified against real bash: fresh-file create, refuse-and-preserve,
`>|` override, `/dev/null`, `>>`, `&>`, `set +C`, and `$-` gaining and
losing `C`. One integration test covers all of it.

### C51 — `set -n` (noexec / syntax-check only) not supported ✅ done
POSIX-mandated; present in dash/bash/ksh/zsh — the standard `sh -n
script.sh` linting idiom (parse the whole script, report syntax errors,
run nothing). Rush's `set_cmd` rejects `-n` outright, and there's no
parse-only mode at any entry point (`-c`, a script file, or interactive)
— rush always executes what it parses. **Effort: M** — a flag that makes
the top-level exec loop parse and skip instead of run, touching all
three invocation modes; rush's existing clean parse/exec separation
makes this a natural, if not entirely trivial, fit.

Implemented, and the clean parse/exec separation did make it small: a
`NOEXEC` thread-local (surfacing as `n` in `$-`), checked at
`exec::run_andor` — the one choke point every top-level and
compound-body command funnels through — so everything still parses and
nothing runs. `rush -n` (before `-c` or a script file) pre-sets the same
flag, giving the standard `sh -n script.sh` lint: status 0 on clean
syntax with zero execution, status 2 on a syntax error (matching bash's
own 2). Two bash subtleties matched: mid-script `set -n` is one-way (the
`set +n` that would undo it never executes — verified against bash), and
an *interactive* shell ignores `set -n` entirely (bash does the same, to
avoid locking the session out). Verified against bash for all of:
mid-script skip, one-way behavior, `-n -c`, `-n file`, and both exit
statuses. Two integration tests cover it.

### C52 — `set -o` long option names, and bare `set -o`/`set +o` listing ✅ done
POSIX-mandated (the long names themselves); present in dash/bash/ksh/zsh.
`-e`/`-u`/`-x`/`-o pipefail` (C18–C20) are all done via their short forms
and `-o pipefail` specifically — but `set -o errexit`/`set -o nounset`/
`set -o xtrace` (the long spellings many scripts prefer for readability)
all fail with `invalid option name`, since `set_cmd` (`builtins.rs`) only
recognizes `pipefail` after `-o`/`+o`. Bare `set -o` (list every option's
current state) and `set +o` (list in a directly re-runnable form) — used
for introspection/debugging — fail with "option requires an argument"
instead. **Effort: S** — map the long names to the existing flag setters
already backing the short forms, and add a listing path over that same
known-option table.

Implemented per the sketch: the `-o`/`+o` arm in `set_cmd` now maps all
six tracked long names — `errexit`, `nounset`, `xtrace`, `noclobber`,
`noexec`, `pipefail` — onto the same pending-flag letters the short
forms queue (so they also get C41's validate-then-apply rollback for
free), and a bare `set -o`/`set +o` lists instead of erroring: `-o` in
bash's own `name<padding>on|off` table format (verified byte-identical
over the tracked options), `+o` as directly re-runnable `set -o name`/
`set +o name` lines — round-tripping `saved=$(set +o); … ; eval
"$saved"` restores the options, the idiom the listing form exists for,
verified working. Scope note: bash lists dozens of shopt-adjacent
options; rush lists exactly the six it tracks. An unknown `-o` name
stays a hard error (status 1). One integration test covers the long
spellings, both listing formats, the eval round-trip, and the error
path.

### C53 — `trap ERR` never fires ✅ done
Present in bash/ksh/zsh (not POSIX/dash) — a common error-handling/
cleanup-framework idiom, paired with `set -e` in the same spirit this
tier's other items already cover. `trap 'cmd' ERR` registers
successfully (`trap.rs` stores any name) but is simply never fired
anywhere — confirmed directly (`trap '...' ERR; false` runs nothing).
**Effort: S–M** — `exec::run_andor`'s existing `last_ran`/status tracking
(built for `errexit`'s exact "final reached command in the and-or chain
failed" rule) already identifies precisely the condition `ERR` needs to
fire on; wiring a registered `ERR` trap into that same check is a
comparatively small addition on top of already-shipped machinery.

Implemented on exactly that machinery: `exec_list_impl`'s errexit check
now fires a registered `ERR` trap on the same condition — a reached,
non-negated final command failing outside an `if`/`while` condition —
whether or not `set -e` is on, and *before* the errexit exit when it is
(order verified against bash). The handler sees the failing status as
`$?` on entry, and `$?` is restored to that status afterward regardless
of what the handler ran (both bash-verified). Not fired inside a
function call, matching bash's default — the `ERR` trap isn't inherited
by functions unless `set -o errtrace`, which rush doesn't implement
(documented narrowing; a function *returning* nonzero still fires at
the call site). C44's spec normalizer needed an `ERR` arm too — it's a
pseudo-signal like `EXIT` with no number and no `SIG` spelling, matched
case-insensitively like bash.

**A real, previously-untracked gap found while landing this**: `! cmd`
— POSIX pipeline negation — didn't parse at all (`!: command not
found`), and it interacts directly with ERR/errexit (a negated pipeline
is exempt from both even when its status is 1 — verified: `set -e; !
true` survives in bash, and `true && ! true` fires no ERR). Implemented
in the same change: a leading `!` (repeatable — `! ! cmd` toggles, like
bash) on `RawPipeline`, status negated in both the run and capture
paths (`$(! true; echo $?)` prints 1, matching bash), with the
exemption threaded through `run_andor`'s existing `last_ran` signal.

Verified against real bash across fourteen scenarios. Two integration
tests cover the ERR matrix and the negation semantics.

### C54 — `${PIPESTATUS[@]}` (per-stage pipeline exit statuses) not implemented ✅ done
Present in bash (zsh has the same idea under `$pipestatus`, lowercase;
not in ksh93/dash) — lets a script tell *which* stage of a pipeline
failed, not just the last/pipefail-adjusted status `$?` gives. `set -o
pipefail` (C19) is done, but the underlying per-stage status array isn't
exposed as a variable at all — `${PIPESTATUS[@]}` always expands empty.
**Effort: M** — the per-stage `Vec<i32>` already exists internally
(`exec::pipeline_status`/`job::wait_pgid`, built to implement pipefail
itself); the work is capturing that same vector into a real indexed-array
variable after every pipeline runs (indexed arrays are fully supported,
C22) rather than discarding it.

Implemented per the sketch: `vars::set_pipestatus` replaces the
`PIPESTATUS` indexed array with the just-finished pipeline's per-stage
statuses, recorded at two points — the multi-stage vector exactly where
the stages are reaped (`job::wait_pgid`, the same `codes` pipefail
already consumes), and a one-element array for every single-stage
command in `run_pipeline_node` (builtins, functions, compounds,
assignment statements, and `cmd &` — bash updates it for *every*
command, verified). Semantics probed against bash and matched: reading
it twice shows the first `echo`'s own `(0)` the second time; `! false`
records the *un*-negated `(1)` (recorded before C53's negation);
`set -o pipefail` doesn't distort the per-stage values; and
`${PIPESTATUS[1]}`/`${#PIPESTATUS[@]}` compose with all the existing
array read forms for free. Deliberately *not* set by pipelines run
inside `$(...)` capture — a real bash substitution is a subshell whose
`PIPESTATUS` never escapes, so the parent's copy staying untouched is
the matching behavior. zsh's lowercase `$pipestatus` spelling is out of
scope (bash is this codebase's reference). One integration test covers
the matrix.

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

### C55 — `[[ ]]` extended test construct entirely missing ✅ done
**The single largest gap in this document.** Present in bash/ksh93/zsh
(not dash — a genuine POSIX-shell/bash-family split, matching rush's own
model in that specific narrow sense, but rush's own README calls itself
"bash-compatible," and `[[` is one of the most defining bash-family
constructs). It isn't merely a stricter-parsing `[ ]`: rush has *no*
`[[`/`]]` tokens, no parser production, nothing — `[[ foo = foo ]]`
resolves `[[` as an ordinary external command and fails with `command
not found` (status 127). Worse, since `[[` is never recognized
syntactically, `<`/`>` inside it are parsed as ordinary redirections:
`[[ abc < abd ]]` tries to open a file named `abd` for reading instead of
comparing the two strings.

This actively breaks common bash-family idioms, not just a missing
convenience: `[[`'s whole reason to exist is suppressing word-splitting
and globbing on its own operands and tolerating an empty/multi-word
variable without quoting. Since rush's `[ ]`/`test` (C6) receives
already-split, already-glob-expanded argv exactly like real `[ ]` does,
every idiom `[[` exists to make safe fails the same way real `[ ]` would:
`x=; [ $x = foo ]` → `too many arguments` (empty `$x` vanishes,
leaving `[ = foo ]`); `x="a b"; [ $x = "a b" ]` → `too many arguments`
(unquoted splits into two words); `x=foo.txt; [ $x = *.txt ]` → `too many
arguments` (unquoted RHS glob-expands). A script author who reaches for
the idiomatic `[[ $x = foo ]]` form gets a hard, unconditional 127
instead of bash's own empty-safe, split-safe, glob-safe behavior.

Roughly half of the other new items below are gated on this one existing
first: `=~` regex matching (C56) and glob-pattern matching on `==`'s RHS
only exist inside `[[`; POSIX bracket-class support inside `[[`
specifically (independent of C42's glob/`case` fix) also needs this.
Extended globbing (C57), `shopt`/glob options (C58), and `test`'s own
missing operators are all independent of `[[` and separately shippable.

**Effort: L.** A new construct needs: lexer recognition of `[[`/`]]` with
word-splitting and glob-suppression off for its interior (and `<`/`>`
must *not* become redirections there); a new, genuinely recursive parser
production, since `&&`/`||`/`!`/`( )` nest directly inside `[[` (unlike
`[ ]`'s flat `-a`/`-o` combinators, C6); and a new evaluator. Real head
start available: `[ ]`'s own `test_unary`/`test_binary`/the `-a`/`-o`
recursive-descent structure (`builtins.rs`) already covers most of the
primaries (`-f`, `-eq`, etc.) and closely mirrors the `&&`/`||`/`!`
grammar `[[` itself needs.

Implemented — all three layers:

- **Lexer**: a standalone `[[` word switches into a dedicated
  interior-lexing mode (`lex_cond`) until the matching `]]`: `<`/`>`
  become ordinary comparison-operator words (never redirections — the
  item's own headline misparse), `&&`/`||`/`(`/`)` are the construct's
  operators, a lone `;`/`|`/`&` is a syntax error (as in bash), newlines
  are mere whitespace (a multi-line `[[` works), and running out of
  input yields `Incomplete` so the REPL reads continuation lines. Words
  keep their full `WordPart` quoting structure — evaluation needs it. A
  `case` pattern like `[[:digit:]]` lexes as one longer word and never
  trips the mode (regression-tested against C42's own tests).
- **Parser**: a genuinely recursive `CondAst` production —
  `or := and ('||' and)*`, `and := not ('&&' not)*`,
  `not := '!' not | '(' or ')' | primary` — with primaries recognizing
  `test`'s unary set plus `-a` (exists)/`-h`/`-L` (symlink, added to
  `[ ]`'s shared helper too) and the binary set
  `= == != =~ < > -eq…-ge -nt -ot -ef`. Operator words must be plain and
  unquoted (`"-f"` is an operand), same as bash. A malformed expression
  is a parse-time syntax error that aborts with status 2, matching bash
  exactly (`[[ a -eq ]]` verified).
- **Evaluator** (`exec::eval_cond`): operands expand with full
  `$`/`$(...)`/quote handling but **no word-splitting and no globbing**
  — the whole point of `[[`. All three of the item's broken idioms now
  behave: `x=; [[ $x = foo ]]` (empty-safe), `x="a b"; [[ $x = "a b" ]]`
  (split-safe), `x=foo.txt; [[ $x = *.txt ]]` (the *RHS* is a glob
  pattern). Pattern semantics follow bash's quoting rule per word part:
  unquoted parts keep metacharacters active (including ones arriving via
  `$var` — `p="*.txt"; [[ foo.txt = $p ]]` is true), quoted/literal
  parts are backslash-escaped (`[[ abc = "a"* ]]` needs a literal `a`) —
  the shared `glob.rs` matcher does the matching, so C42's POSIX classes
  work inside `[[` for free (`[[ 5 = [[:digit:]] ]]`). `<`/`>` compare
  lexicographically; `-eq…-ge` evaluate both sides as full arithmetic
  (`x=5; [[ x -eq 5 ]]` is true, unlike `[ ]`'s integer-literal rule —
  bash-verified); `-nt`/`-ot`/`-ef` compare file mtimes/identity.
  True/false/error → 0/1/2, without aborting on an evaluation error.
  `=~` is recognized by the grammar but returns a status-2 "not
  supported yet" pointing at C56, the item sequenced next.

Verified against real bash across 32 scenarios, each byte-identical —
including the three headline idioms, grouping/negation/nesting, `if`/
`while` conditions, `set -e` interplay, multi-line input, and the
malformed-expression abort. Regression tests: one parser unit test
(recursion + `<`-is-not-a-redirect) and one comprehensive integration
test.

### C56 — `[[ $s =~ $regex ]]` (ERE matching) + `$BASH_REMATCH` ✅ done
Present in bash (full, including capture groups in `$BASH_REMATCH`);
ksh93 and zsh both support `=~` matching but populate the captures under
a *different* name (`.sh.match`, `$MATCH`/`$match` respectively) — a real
semantic divergence between the "big three," not just a syntax one. Not
in dash/POSIX. **Depends entirely on C55** — `=~` is `[[`-only in every
shell that has it. **Effort: M–L** (once `[[` exists) — needs a real ERE
engine (the `regex` crate is the practical option, though its own syntax
isn't a byte-for-byte match for POSIX ERE); `$BASH_REMATCH` itself is
cheap on top, since indexed arrays already exist (C22).

Implemented on C55's day-old foundation, using the `regex` crate as the
sketch suggested (the codebase's second dependency ever; its syntax
isn't byte-for-byte POSIX ERE but agrees on everything tested). The
piece that turned out to need real work was *lexing*, not matching:
bash reads the RHS of `=~` in its own mode — unquoted parens belong to
the pattern (balance-tracked; whitespace inside a group is part of it,
`[[ "a b" =~ (a b) ]]` matches), `{n}` quantifiers pass through, and
`\.` stays a literal dot. A new `lex_regex_word` handles exactly that,
entered when the `[[`-interior lexer sees `=~`. Semantics, each verified
against bash: the match is an unanchored *search*; quoted/literal RHS
parts match literally (`[[ abc =~ "a.c" ]]` is false) while unquoted
parts — including via `$var`, the common `p="^…$"` idiom — are live
regex; `BASH_REMATCH[0]` is the whole match with capture groups in
`[1..]` (an unmatched optional group present as an empty string); a
failed match *unsets* the array (bash 5 behavior); an invalid live
pattern is a status-2 evaluation error that doesn't abort. ksh/zsh's
different capture-variable names (`.sh.match`, `$MATCH`) are out of
scope — bash is the reference. One integration test covers the matrix.

### C57 — Extended globbing: `?(pat)` `*(pat)` `+(pat)` `@(pat)` `!(pat)` ✅ done
Native in ksh93 and zsh (`setopt kshglob`/`extendedglob`); bash requires
`shopt -s extglob` first (itself missing — see C58) — not in dash/POSIX.
Rush's glob matcher (`glob.rs`) has no alternation/grouping at all, and —
worse — its lexer treats a `(` following one of these prefix characters
as an ordinary subshell open, so `echo @(a|b)file` doesn't just fail to
match, it mis-tokenizes into `@`, `(a|b)` (a subshell), and `file`.
`case afile in @(a|b)file) …` is a hard parse error (`expected ')' in
case`), not a silent no-match. **Effort: L** — needs real alternation/
backtracking in the glob matcher, lexer changes so `(` after
`?`/`*`/`+`/`@`/`!` in a word context isn't read as a subshell, and `case`
pattern-parsing support for the new pattern shape. Independent of C55.

Implemented, all three pieces:

- **Matcher** (`glob.rs`): `parse_extglob` splits a group into top-level
  `|` alternatives (nesting-aware), and `match_extglob` does real
  backtracking — every split point tried, alternatives being full glob
  patterns themselves so nesting (`@(f@(o|x)o)`) and mixed wildcards
  (`@(*.txt|*.rs)`) recurse naturally. Semantics probed against bash
  (`shopt -s extglob`) first: `?(…)` is exactly 0-or-1 (`aax` does NOT
  match `?(a)x`), `*`/`+` are 0+/1+ repetitions of alternative-matched
  chunks, `@` exactly one, and `!(…)` matches any prefix *not* matched
  in full by an alternative (`abfile` matches `!(a|b)file`; `afile`
  doesn't) — each of which is easy to get wrong from the docs alone.
  An unterminated group falls back to literal characters, matching the
  matcher's existing `[`-fallback convention.
- **Lexer**: a `(` directly after `?`/`*`/`+`/`@`/`!` *within* a word
  swallows the balanced group (alternation `|` included) into the word
  instead of ending it at a subshell boundary — `@(a|b)file` is one
  word, `case … in @(a|b)file)` parses, and a bare `(...)` is still a
  subshell. Pipeline negation is unaffected (`!(…)` is a word; `! cmd`
  needs the space, same as bash).
- **Expansion**: the field-splitter's globbable detection now recognizes
  extglob openers (`@(`/`+(`/`!(` — `?(`/`*(` were already covered by
  their first character), so filename expansion actually fires.

Always-on, like ksh93 — bash gates these behind `shopt -s extglob`
(C58), and *without* extglob bash makes them hard syntax errors anyway,
so always-on is strictly more compatible than the old mis-tokenization.
Because `case`, `[[ ]]`, filename expansion, and the `${v%pat}` family
all share the one matcher, all four surfaces landed at once — filename
expansion verified byte-identical to bash on shared fixtures. One unit
test (the semantics matrix) and one integration test (all four
surfaces) cover it.

### C58 — `shopt` builtin, and the glob options it gates: `nullglob`, `failglob`, `dotglob`, `globstar` ✅ done
`shopt` itself is bash-specific; the *behaviors* it toggles have rough
equivalents in zsh (`setopt`) and, for `globstar`, native `**` in
ksh93/zsh. Not in dash. Rush has no `shopt` builtin at all (`command not
found`), and the glob engine's own behavior is hardcoded rather than
switchable: an unmatched glob always stays literal (bash/rush's shared
default — correct as a *default*, but there's no way to opt into
`nullglob`'s empty-list-instead behavior, or `failglob`'s hard error);
dotfiles are always skipped by `*` (no `dotglob` toggle to opt in);
`**` is not recursive at all — `glob.rs` collapses adjacent `*`
components, so `**/*.txt` behaves like `*/*.txt` (one directory level,
not arbitrary depth), with no `globstar` toggle to fix it either way,
unlike bash where it's a real opt-in. **Effort: M** for the `shopt`
framework itself plus `nullglob`/`failglob`/`dotglob` (each a flag check
at the existing glob-fallback decision point in `expand.rs`, mirroring
the thread-local flag pattern `errexit`/`nounset`/`xtrace`/`pipefail`
already use in `vars.rs`); **globstar specifically is its own M** — real
recursive-descent directory walking in `glob.rs`'s `walk`, not just a
flag check.

Implemented, both halves. The `shopt` builtin covers list/`-p`
(re-runnable)/query/`-q`/`-s`/`-u`, with bash's exact formats and
statuses (query returns 0 only when *all* named options are set;
unknown names are "invalid shell option name", status 1; a bare
`shopt -s` lists only the set options — all verified). Recognized set:
the four glob options plus `extglob`, which defaults **on** (C57's
ksh93-style choice, now genuinely toggleable — `shopt -u extglob` makes
the matcher treat `@(…)` as literal text; bash-off-mode's hard syntax
error is the one deliberate divergence, since rush's lexer change is
unconditional). Options live in a `SHOPTS` map over a defaults table in
`vars.rs`, only storing explicit toggles.

The glob behaviors: `nullglob` (no-match drops the word) and `failglob`
(no-match is a hard error — bash aborts the whole `-c` script there,
verified, and rush's expansion-error channel does the same) hook the
single existing fallback decision point in `expand.rs`; `dotglob` is a
flag check in `glob.rs`'s `walk`; and `globstar` got the real recursive
descent the estimate promised — `**` matches zero or more directory
levels, with output shapes verified **byte-identical to bash** on a
shared fixture tree for `echo **`, `echo **/*.txt`, and `echo a/**`
(including bash's quirk of listing the zero-level directory itself as
`a/` with a trailing slash). Not implemented, documented: the
`**/`-only-directories trailing-slash form (rush's path splitter drops
the empty final component). Without the option, `**` keeps collapsing
to `*`, exactly as before. One integration test covers the whole
matrix.

### C59 — String transformation operators: search/replace `${v/pat/repl}`, substring `${v:offset:length}`, case conversion `${v^^}` ✅ done
The single most commonly used family of missing `${...}` operators.
Search/replace (`${v/pat/repl}` first match, `${v//pat/repl}` all,
`${v/#pat/repl}`/`${v/%pat/repl}` anchored) and substring extraction
(`${v:offset}`, `${v:offset:length}`, including negative offset/length)
are both present in bash/ksh93/zsh (not dash/POSIX); case conversion
(`${v^}`/`${v^^}`/`${v,}`/`${v,,}`, optionally with a pattern argument)
is bash-only (zsh spells the same idea `${(U)v}`/`${(L)v}`; no ksh93
equivalent). All three hit the same `expand_braced` dispatch point in
`expand.rs` and fall through to its final `bad substitution` error —
a hard, clean parse-time-adjacent rejection, not silent wrongness, but a
real gap: this exact operator family is close to ubiquitous in real bash
scripts (stripping a path segment, normalizing case, trimming a known
prefix length). Also missing: applying any of these across a whole array
(`${arr[@]/pat/repl}`, `${arr[@]^^}`) — the operators don't exist for
scalars to begin with, so the array-wide form is a strict superset gap.

Implemented — all three families plus the array-wide forms:

- **Search/replace**: `/` (longest match at the earliest position — the
  greediness verified against bash: `${v/X*/Z}` on `aXbXc` yields `aZ`),
  `//` (every non-overlapping match), `/#`/`/%` (anchored), a missing
  `/repl` deletes, and the pattern/replacement split is the first
  *unescaped* slash (`${v/\//_}` replaces a literal `/`). Patterns are
  the shared glob matcher's — classes, extglobs and all.
- **Substrings**: `${v:offset[:length]}`, both sides full arithmetic
  expressions; negative offset counts from the end (with bash's own
  space disambiguation against `:-`, and out-of-range → empty), negative
  length means "up to that many before the end" and errors with bash's
  exact "substring expression < 0" when it lands before the offset. The
  `:-`/`:=`/`:+`/`:?` family is untouched — a `:` only reads as a
  substring when the next character can't start that family.
- **Case conversion**: `^`/`^^`/`,`/`,,`, with the optional
  single-character pattern restriction (`${v^^[a-f]}` → `hEllo`).
- **Array-wide forms**: `${arr[@]/pat/repl}`, `${arr[@]^^}`, and the
  `#`/`%` strips apply per element; `${arr[@]:off:len}` turned out to be
  a different operation entirely — array *slicing* (a range of elements,
  `${arr[@]:1:2}` is elements 1–2, `${arr[@]: -1}` the last), caught by
  direct comparison and implemented as such.

Verified against bash byte-for-byte across ~40 cases. Two integration
tests cover the scalar matrix and the array-wide/slicing split.
**Effort: M** — search/replace needs a leftmost-glob-match-at-each-
position primitive (the existing pattern matcher only supports whole-
string/anchored matching, per C1's prefix/suffix removal); substring
needs new parser disambiguation from the existing `:-`/`:=`/`:+`/`:?`
family (a leading `:` is currently always read as one of those); case
conversion is the cheapest of the three, a straightforward char-map.
Array-wide application is a small M on top of the scalar case existing.

### C60 — Indirect expansion `${!var}`, name-listing `${!prefix@}`, transformation operators `${v@Q}` ✅ done
Rarer, more bash-specific siblings of C59, sharing the same
`expand_braced` dispatch point and the same `bad substitution` failure
mode. `${!var}` (expand to the value of the variable *named by* `$var`'s
own value) is bash-specific syntax — ksh93's own `${!var}` means
something different (the *name*, a nameref-flavored read, not the
bash-style double dereference), and zsh has no equivalent spelling at
all (`${(P)var}` instead) — a real semantic fork across shells, not just
a portability gap. `${!prefix@}`/`${!prefix*}` (list every variable name
starting with `prefix`) is shared by bash and ksh93. `${v@Q}` (shell-
requote), `${v@E}` (ANSI-C unescape), `${v@A}` (reconstruct as a
`declare` statement), `${v@a}` (attribute flags) are bash 4.4+ only.
**Effort: S** each for indirect expansion and name-listing — the latter
can reuse `vars::names()` (already built for tab completion, C34)
directly, just prefix-filtered. **Effort: M** for the `@`-transforms,
lower priority given how rarely they appear outside debugging/
serialization helpers, and `@A`/`@a` specifically depend on attribute
introspection that's cheaper once C43/C45's attribute-flag work exists.

Implemented, all three groups. `${!var}` dereferences to a variable
name *or* a positional-parameter number, and trailing operators compose
by re-dispatching through `expand_braced` itself — so `${!v:-def}`,
`${!v/pat/repl}`, and every other operator apply to the *referent* with
zero extra code (verified against bash). An empty referent is a hard
"invalid variable name" error, matching bash exactly (not an empty
expansion). `${!prefix@}`/`${!prefix*}` reuse `vars::names()` as the
sketch predicted, sorted and joined like `$@`/`$*`. The `@`-transforms
landed too, and the prediction about C43/C45 held: `@a` reads the
attribute flags (`declare -ir n=5` → `ir`) plus the array kinds, and
`@A` reconstructs `name='value'` or a `declare -flags` form on top of
`@Q` — whose output format matches bash's exactly (single quotes with
the `'\''` dance, or `$'...'` when control characters are present).
`@E` interprets the `$'...'` escape set. Documented narrowings: `@A`'s
array form uses the modern element-list format (this container's older
bash prints a strange scalarized form), and the `@`-transforms apply to
scalars (not `${arr[@]@Q}` element-wise). One adjacent observation
recorded here at the time: `$'...'` ANSI-C quoting itself was not
implemented and not tracked by any item — subsequently implemented
while landing C63, whose `%q` output (like `@Q`'s) uses that form for
control characters and rush couldn't re-read its own quoting. Two integration tests cover the
matrix.

### C61 — `mapfile` / `readarray` ✅ done
Bash-only (no equivalent spelling in zsh/ksh93, which use `arr=("${(@f)$(...)}")`/`read -A`
idioms instead; not in dash) — but since rush targets bash compatibility
specifically, and `mapfile -t lines < file` is the modern, correct
replacement for a `while read` loop over a whole file, its total absence
(`command not found`) is a real gap despite only one shell having it.
**Effort: M** — rush's existing `read` builtin (C7) already reads one
line at a time off fd 0 without over-consuming; `mapfile` is a loop over
that same primitive, appending into an indexed array (C22) via
`array_append`. `-t` (strip trailing newline) is the one flag that
actually matters for real usage; `-d`/`-n`/`-s`/`-O`/callback flags
(`-c`/`-C`) can reasonably wait.

Implemented exactly per the sketch: `mapfile [-t] [array]` (and its
`readarray` synonym) loops `read`'s existing byte-at-a-time
logical-line primitive in raw mode — so it never over-consumes fd 0 and
`mapfile -t lines < file` works with redirects like any builtin — and
assigns the collected lines as one indexed array (`MAPFILE` when no
name is given). Verified against bash: `-t` strips the newline; without
it each element keeps its own trailing `\n`; an unterminated final
line still becomes an element (with no newline to keep either way);
empty input yields an empty array (`n=0`); an invalid identifier is
"not a valid identifier", status 1. `-d`/`-n`/`-s`/`-O`/`-c`/`-C`
remain documented waits per the item's own scoping, and error clearly
rather than misparse. One integration test covers the matrix.

### C62 — Nameref variables: `declare -n` / `local -n` / ksh `nameref` ✅ done
Present in bash 4.3+ (`declare -n`/`local -n`) and ksh93 (its own
`nameref` keyword, plus `typeset -n`); no equivalent in zsh (`-n` errors
there too) or dash. Silent wrongness, not an error: `declare -n ref=x`
assigns the *literal string* `"x"` to `ref` (the `-n` flag is swallowed
as a bogus bare name — same root cause as C43/C45) rather than making
`$ref` an alias for `$x`, so `ref=changed` leaves `x` completely
untouched instead of updating it. This is the standard mechanism bash
library functions use to "return" a value or array by writing through a
caller-named variable — a real, if less common than C59's operators,
gap in reusable-function style bash code. **Effort: L** — needs a new
`Nameref(String)` attribute on `Var` plus indirection at *every* read and
write site (`get`/`set`/`assign`/array ops/`unset` all need to check for
and follow it) — genuinely cross-cutting, unlike the other attribute
flags in this document, which only affect assignment.

Implemented, and the cross-cutting prediction was accurate: a
`NAMEREFS: ref → target` map (following the same separate-map pattern
C43 established) with a `resolve_name` chain-follower (depth-capped —
a circular `declare -n a=b; declare -n b=a` stops following instead of
hanging; bash warns there) hooked at the top of **twenty-six** read and
write functions in `vars.rs` — `get`, `set`, every array/assoc
read/write/append/unset-element path, `export`, and `unset` (which
unsets the *target*, the ref keeping its referent — verified). Two
probed subtleties matched exactly: a bare `declare -n ref` records a
target-less nameref whose next plain assignment *names* the target
rather than writing through, and `local -n out=$1` — the headline
"return through a caller-named variable" mechanism — is frame-scoped,
with local frames now capturing and restoring the prior nameref mapping
alongside prior values and attributes. Works for scalars and whole
arrays in both directions (`ref[0]=Z` writes through; `out=(a b c)`
returns an array). One integration test covers the matrix.

### C63 — `printf %q` ✅ done
Present in bash/zsh/ksh93 (not dash/POSIX, not fish) — quotes a string so
it's safe to reuse as shell input, common in codegen and "print a
copy-pasteable command" tooling. Rush's `printf` (C8) accepts only
`diouxXcsb`; `%q` is rejected outright (`invalid conversion
specification`). **Effort: S–M** — self-contained: extend the accepted
conversion set and add a shell-quoting helper (backslash-escape style,
matching bash/zsh; ksh93 prefers single-quote style, a cosmetic
difference not worth chasing). No interaction with the rest of the
expander.

Implemented exactly per the sketch: `q` joins the accepted conversion
set, backed by a quoting helper in bash/zsh's backslash style —
shell-special characters escaped, `''` for an empty argument, and the
`$'...'` form when control characters are present. Output verified
byte-identical to bash across the probe set (quotes, spaces, `$`, `;`,
empty, control characters), and the control-character form round-trips
through `eval` — which required actually implementing **`$'...'`
ANSI-C quoting** in the lexer (the untracked gap C60's write-up had
just recorded): rush's own `%q`/`@Q` output uses that form and rush
couldn't re-read it. `$'...'` now lexes as a literal with the `@E`
escape set interpreted at lex time, verified against bash. One
integration test.

### C64 — Job-control niceties: `jobs -l`/`-p`, `kill -l` + a fuller signal table, `wait -n`, `disown` ✅ done
A cluster of small, independently shippable job-control completeness
gaps, all present in bash and mostly in zsh/ksh93 (`wait -n` is
bash-only; the doc's own comparison matrix already flags `disown` as the
one known-missing piece of the `wait`/`disown` row — confirmed still
accurate, bundled here alongside its close relatives rather than
re-reported standalone). `jobs_cmd` (`job.rs`) takes no arguments at all
today, so `-l` (include pids) and `-p` (pids only) are silently ignored
— the job table already tracks every pid, the flags just aren't parsed.
`kill -l` (list signal names) fails outright, and the signal-name table
`kill`/`trap` share (`job.rs`) only knows seven names (TERM/KILL/INT/
HUP/QUIT/STOP/CONT) — so even `kill -USR1 %1` fails today, not just the
`-l` listing form. `wait -n` (wait for whichever tracked job finishes
next, not a specific one — common in bounded-parallelism worker-pool
loops) isn't recognized as a valid job spec. `disown` (remove a job from
the table so it survives the shell exiting) isn't registered as a
builtin at all. **Effort: S** each — `jobs -l`/`-p` and `kill -l` are
pure formatting over data the job table already has; a fuller signal
table is a lookup-table expansion; `disown` removes an entry from the
existing job table using the same `%job`/current-job selection logic
`fg`/`bg` already have; `wait -n` needs a small extension to the existing
blocking-wait loop to stop at whichever tracked pid exits first instead
of a specific one (**S–M**).

Implemented, the whole cluster. `jobs -l` adds the pgid to each line
and `-p` prints pgids alone (pure formatting over the existing table,
as predicted). The `kill`/`trap` signal table grew from seven names to
twenty-one (each mapped to its real `libc` constant), so `kill -USR1
%1` works; `kill -l` lists the numbered table, and `kill -l TERM` /
`kill -l 15` convert in both directions. `wait -n` blocks on
`waitpid(-1)` until whichever child exits next, records it through the
existing bookkeeping, and returns its status (127 with no children
left) — verified with the worker-pool shape it exists for. `disown`
drops the selected (or most recent) job from the table via the same
`%job` selection `fg`/`bg` use.

One genuinely behavioral piece rode along: **registering a trap for
the newly-nameable signals now actually installs a handler** —
`trap 'cmd' USR1` used to register and then die on delivery (only
`TERM`/`HUP` had handlers installed at startup). `trap::set`/`unset`
now install/restore the disposition dynamically for the catchable,
non-job-control set (`QUIT`/`ABRT`/`ALRM`/`USR1`/`USR2`/`PIPE`), with
delivery-side naming switched to the shared `libc`-constant table;
`INT` stays with the interactive machinery and the stop signals are
deliberately untouched. One integration test covers the cluster,
including a delivered `USR1` firing its trap.

### C65 — `trap DEBUG` / `trap RETURN`, and `trap -l`/`trap -p` ✅ done
Present in bash/ksh93/zsh (not POSIX/dash) — distinct from C44 (a real
bug: a *registered* numeric/`SIG`-prefixed trap silently never fires) and
C53 (`trap ERR`, tier III, a scripting-safety idiom): these are two
rarer pseudo-signals (`DEBUG` fires before every simple command; `RETURN`
fires when a function or sourced script returns — both niche, mostly
profiling/step-debugging tools) plus two introspection flags. `trap -l`
(list signal names — shares C64's signal-table gap) and `trap -p` (print
every registered trap in a directly re-runnable form, for saving/
restoring trap state) are both currently misparsed: `trap_cmd`
(`builtins.rs`) treats any argument except a literal `-` as the trap's
command string, so `trap -l`/`trap -p` register a bogus, harmless
no-op trap named `-l`/`-p` instead of listing anything (bare `trap` with
no arguments does correctly list — only the flagged forms are broken).
**Effort:** `DEBUG` **M** (a new hook before every simple command in
`exec::exec_list_impl`'s per-job loop); `RETURN` **S–M** (fire once, in
`exec::call_function`'s and `source_file`'s own return paths); `-l`/`-p`
**S** each (`-l` shares C64's table; `-p` reuses the listing logic bare
`trap` already has).

Implemented, all four pieces. `DEBUG` fires before each pipeline in
`exec::run_andor` — bash fires per *simple command*, so one `a | b`
pair is a single firing here where bash may fire per stage, a
documented approximation — with `$?` preserved across the handler
(bash-verified: `trap 'echo D' DEBUG; false; echo $?` still prints 1),
via a new shared `fire_preserving`. `RETURN` fires from
`call_function`'s return path and when a sourced script finishes, the
function's own status preserved for the caller. `trap -l` prints
bash's numbered five-per-line tab-separated table off C44's existing
name↔number table, and `trap -p [name...]` reuses the bare-`trap`
listing format with optional filtering (specs normalized, so
`trap -p 15` works too). Both pseudo-signals joined the C44 normalizer
(case-insensitive, no number, no `SIG` spelling — like `ERR`). One
integration test covers all four.

### C66 — `coproc` (named bidirectional coprocess) ✅ done
Present in bash (`coproc`) and zsh; ksh93 uses different (`|&`) syntax;
not in dash/fish. A specialized, powerful tool — a long-lived helper
process with a shell-visible bidirectional pipe (`${NAME[0]}`/`${NAME[1]}`
as its read/write fds) — but rare in ordinary scripts compared to
everything else in this document. Entirely unimplemented: no `coproc`
keyword, and a realistic hand-rolled equivalent trips the pre-existing
C38-adjacent limitation that `${arr[N]}`-sourced fd redirects
(`>&"${mycop[1]}"`) don't resolve either, meaning the fd-array plumbing
this would need is itself a prerequisite, not just the keyword/parsing.
**Effort: L** — new parser grammar, a fork with two real pipes, and
populating a shell array with the resulting two fd numbers, plus
background-job bookkeeping to track the coprocess itself.

Implemented, prerequisite first: **`fd>&$word` / `fd<&"${arr[N]}"`
redirects** — the fd number arriving via expansion, which the doc
correctly flagged as the real blocker — got a new `RedirOp::DupWord`
lexed when `>&`/`<&` isn't followed by digits, resolved to a numeric fd
at expansion time (a non-numeric expansion is "bad file descriptor").
Then `coproc [NAME] command` itself: a new parser production (NAME
accepted before a `{ ... }` group, bash's own rule; unnamed uses
`COPROC`), and an executor that forks with two real pipes — the child
gets them on stdin/stdout and runs the command; the parent publishes
`NAME=(read_fd write_fd)` and `NAME_PID`, marks both fds close-on-exec
(matching bash — ordinary children don't inherit them; an explicit
`>&$fd` still works since `dup2` clears the flag on the copy), and
records the pid as `$!`. One bug caught by testing rather than review:
the forked child inherits the parent's TERM/HUP record-and-defer
handlers and would swallow a plain `kill $COPROC_PID` while blocked —
and the first fix (the child resetting them after fork) still lost an
intermittent race when the kill landed before the reset, caught as a
hanging test. The default dispositions are now set in the parent
*before* the fork (race-free), reinstalled immediately after; a killed
coprocess `wait`s as 143 exactly like bash, verified under an 8-run
stress loop. Documented narrowings: the coprocess is a
forked shell wrapping the command (bash execs a simple command
directly), and it isn't listed in the interactive `jobs` table, though
`wait $COPROC_PID` works through the ordinary pid path. Verified
against bash for the full echo→read round-trip, the named-group form,
`$!`, and the kill/wait status. One integration test.

### C67 — Rarer special variables: `$LINENO`, `$RANDOM`, `$SECONDS`, `$FUNCNAME`, `$BASH_SOURCE`, `$EPOCHSECONDS`/`$EPOCHREALTIME` ✅ done
A grab-bag of bash-specific special variables (ksh93/zsh have some under
different names; none in dash/POSIX, which is the reason none of these
made it into C41's POSIX-mandated set). All currently expand to empty
via the same `$`-scanner fallthrough C41 describes. Real-world value
varies widely: `$RANDOM` and `$SECONDS` are genuinely common (ad hoc
temp-name suffixes, crude timing); `$FUNCNAME`/`${FUNCNAME[@]}` (the call
stack of function names) and `$BASH_SOURCE`/`${BASH_SOURCE[@]}` (the
"script's own directory" idiom, `"${BASH_SOURCE[0]}"`) are moderately
common in reusable library scripts; `$EPOCHSECONDS`/`$EPOCHREALTIME`
(bash 5+) are genuinely rare. `$LINENO` is the structurally hardest of
the set — rush's AST carries no source-line information at all today, so
supporting it means threading line numbers through the lexer, parser,
and executor, not just adding a scanner arm. **Effort: S** each for
`$RANDOM` (a seeded PRNG) and `$SECONDS`/`$EPOCHSECONDS`/`$EPOCHREALTIME`
(all a stored start `Instant`/`SystemTime::now()` away); **M** for
`$FUNCNAME`/`$BASH_SOURCE` (need a call-stack/source-stack maintained
through `exec::call_function`/`source_file`); **M–L** for `$LINENO`
specifically, given the AST-wide plumbing.

Implemented, the whole bag — closing Tier IV completely. The clock/PRNG
set are *dynamic* variables computed at read time in `vars::get`:
`$RANDOM` is a seedable LCG in bash's 0..=32767 range (`RANDOM=42`
makes the stream reproducible, matching bash — assignment re-seeds
rather than stores); `$SECONDS` counts from shell start with
`SECONDS=100` re-basing it; `$EPOCHSECONDS`/`$EPOCHREALTIME` read the
system clock (the microsecond form agreeing with the second form,
verified). `${FUNCNAME[@]}` is a real array mirrored from a call stack
pushed/popped by `call_function` (innermost first; unset outside any
function, like bash), and `${BASH_SOURCE[@]}` likewise from a
source-file stack (the script at the bottom, `source`d files pushed;
empty under `-c`, same as bash). `$LINENO` turned out cheaper than the
M–L estimate: rather than threading line numbers through the whole AST,
`RawPipeline` carries one `line` computed from newline-token counts at
parse time, and `run_andor` publishes it before each pipeline — values
byte-identical to bash for the same script file. Documented
approximations: a here-doc body's own newlines don't advance `$LINENO`
(the lexer swallows them), and `BASH_SOURCE` inside a function reflects
the current source stack rather than the function's definition site.
One integration test covers the bag.

### C68 — No syntax highlighting or live validation of the command line ✅ done
Native in fish (real-time), available in zsh (`zsh-syntax-highlighting`),
absent from bash/dash/ksh93. Rush's `RushHelper` (`src/completion.rs`)
implements `Completer`/`Hinter` but its `Highlighter` impl is limited to
C33's dimmed history-suggestion text — it does nothing to the line the
user is actually typing (no color for strings/keywords/unknown commands,
no red flag for unmatched quotes or an unresolvable command name before
Enter is pressed). Confirmed not blocked on `rustyline` 18's architecture:
`Highlighter::highlight()`/`highlight_char()` are exactly the hooks fish
and zsh-syntax-highlighting are built on elsewhere, and rush already
constructs and installs a custom `Highlighter` for C33, so the wiring
point already exists — this is additional logic in that same impl, not a
new mechanism. **Effort: M** — needs a lightweight, error-tolerant
re-lex of the in-progress line (reusing `lexer.rs`) to classify spans,
plus a first-word-only PATH/builtin/function/alias lookup for the
command-not-found case; live validation (unmatched quotes, etc.) rides
the same re-lex pass, which is why the two are budgeted together rather
than separately.

Implemented in `RushHelper`'s existing `Highlighter` impl, as the
write-up predicted — same wiring point as C33, new logic only. One
deviation from the sketch, for cause: the classifier is a small
dedicated span scanner rather than a re-lex through `lexer.rs`, because
the real lexer returns tokens *without byte spans* and hard-errors on
exactly the incomplete input an in-progress line is made of. The
fish-style scheme: command-position words (including after `|`/`;`/`&`,
with assignment prefixes skipped) resolve through the full
keyword/builtin/function/alias/`$PATH` chain and render green or —
the pre-Enter command-not-found flag — red; quoted strings yellow with
an *unmatched* quote's whole tail red (the live-validation half);
comments dimmed; `$var`/`${...}` cyan; operators magenta.
`highlight_char` requests repaint on every change. C33's dimmed-hint
rendering is untouched beside it. Unit tests cover the classification
matrix (resolvable/unresolvable command words, matched/unmatched
quotes, comments, vars, pipe-resets-command-position, assignment
prefixes).

### C69 — Tab completion shows one candidate at a time, no columned list ✅ done
Native list/menu display in fish, zsh, and bash (bash-completion or even
stock bash's default double-Tab listing). Rush's `Editor` is currently
constructed with `rustyline`'s default `CompletionType::Circular` — Tab
blindly cycles through candidates one at a time in place with nothing
shown on screen listing what the other options are, confirmed directly
against the compiled binary under a pty. `rustyline` 18 already fully
implements the columned, paged alternative (`CompletionType::List`,
backed by its own `page_completions` — the exact mechanism bash's
default Tab-Tab listing and fish's dropdown are built on); switching is
a one-line `Config::builder().completion_type(CompletionType::List)`
change at `Editor` construction, not new completion logic. **Effort: S.**

Implemented as exactly that one-line change: the interactive `Editor`
is now built with `CompletionType::List`, so Tab shows `rustyline`'s
columned, paged candidate list (its own `page_completions`, the same
display bash's Tab-Tab and fish's dropdown are built on) instead of
silently cycling candidates in place. No completion logic changed —
every candidate source from C34 feeds the new display unchanged.

### C70 — No abbreviations / global-alias live expansion ✅ done
Native in fish (`abbr`) and zsh (`alias -g`) — a name that expands
in-place as you type (typically on space or Enter), visible and editable
before the command runs, distinct from a regular alias which only
expands at execution time and only in command position. Rush has C34's
completion-time alias-name awareness and the pre-existing execution-time
`alias`/`unalias` builtins, but nothing live-expands text on the line
itself while typing — confirmed directly (typing a defined alias name
followed by space does not rewrite the line). Feasible without a
dependency change: `rustyline` exposes `ConditionalEventHandler`/
`bind_sequence`, which is exactly the mechanism needed to intercept a
space/Enter keypress, check the just-typed word against a new
abbreviation table, and rewrite the buffer in place before the key's
default behavior runs. **Effort: M** — new abbreviation table
(name → expansion, likely its own builtin `abbr`/`unabbr` rather than
overloading `alias`, since fish/zsh keep the two concepts separate) plus
the key-event wiring.

Implemented per the sketch: a separate abbreviation table with its own
`abbr name=value` / `unabbr` builtins (listing re-runnably with bare
`abbr`, matching the concept split fish and zsh both keep), and the
key-event wiring the write-up identified — a `ConditionalEventHandler`
bound to the space key that, when the word just typed is a defined
abbreviation *in command position* (segment-aware: after `|`/`;`/`&`
counts, argument position doesn't), rewrites it in the buffer via
`Cmd::Replace` before the space lands — visible and editable before the
command runs, fish's `abbr` behavior exactly. The expansion decision is
a pure function (`abbr_expansion`), unit-tested apart from the
key-event plumbing; the builtins have an integration test. Expansion
triggers on space (fish also expands on Enter — a documented
narrowing).

### C71 — No right-side prompt (`$RPS1` / fish right prompt) (tracked) ✅ done
Native in zsh (`$RPS1`) and fish (`fish_right_prompt`) — text
right-justified on the current input line (commonly exit status, git
branch, or a clock), distinct from and independent of the left prompt
(`$PS1`). **Confirmed blocked on `rustyline` 18's own architecture**,
unlike every other item in this tier: the crate has no right-prompt
concept anywhere — no hook renders anything at a fixed right-hand
column, and its line-wrapping/cursor-math is written assuming a single
left-hand prompt only. Supporting this would mean either patching
`rustyline` itself or replacing it with a different line-editing crate
(e.g. `reedline`, which does support a right prompt) — a materially
larger undertaking than the rush-side-only work every other Tier V item
needs. **Effort: L / dependency-blocked.**

Implemented — by taking the write-up's "materially larger undertaking"
head-on: `rustyline` is gone entirely, replaced by a hand-rolled line
editor (`src/editor.rs`, ~900 lines) that owns the terminal directly —
raw mode via termios (with an RAII restore guard), its own key decoder
(CSI parsing with a 30 ms poll-based lone-ESC disambiguation), and its
own repaint engine with ANSI-aware display-width math (the
`unicode-width` crate) and soft-wrap row accounting. Owning the render
loop is exactly what makes this item trivial where it was impossible
before: each repaint, if `$RPS1` (expanded fresh every prompt, so
`$?`-style content stays live) fits after the input text with a gap
(`wtotal + wrps1 + 1 < cols`), the editor emits `ESC[{col}G` to park it
flush right, then repositions the cursor; when the input grows into it,
the right prompt simply isn't drawn — zsh's own behavior. Everything
rustyline provided was reimplemented rather than dropped: emacs
keybindings, a vi keymap (`set -o vi`, now switchable live per-read with
no editor rebuild and no history loss — retroactively simplifying C73),
history with consecutive-dedup plus Ctrl-R incremental search, tab
completion (longest-common-prefix insertion, then a columned candidate
list) including a new self-contained filename completer, the C69
history-hint ghost text (Right/End accepts), the C68 syntax
highlighting, and the C70 abbr-on-space expansion — `completion.rs` is
now pure candidate/hint/highlight logic with no editor-crate types in
its API. Verified end-to-end with a real terminal via a Python
`pty.fork()` harness (`tests/pty/editor_pty_test.py`, 14 scenarios:
editing, history, completion, hint, abbr, vi mode, Ctrl-R, Ctrl-C,
wrap, and the right prompt itself), which caught a genuine bug unit
tests couldn't: reading input through `io::stdin()`'s userspace buffer
made `poll(2)` on fd 0 lie (bytes already buffered aren't "ready"), so
arrow keys decoded as lone ESC + literal `[A` — fixed by reading fd 0
raw. Documented narrowings: the vi keymap is a practical subset (motions
h/l/0/$/b/w/e, x/X/D/dd/dw/db/d$/d0, i/I/a/A, j/k history), non-tty
stdin and non-Unix fall back to a plain buffered reader (so piped
"interactive" input still works, as rustyline's did), and long
completion lists print unpaged.

### C72 — `cd` niceties: spelling correction, `pushd`/`popd`/`dirs`, `cd -N`, `$CDPATH` ✅ done
A bundle of `cd`-adjacent conveniences, none present in rush today
(confirmed directly: `pushd`/`popd`/`dirs` are all "command not found";
`cd -N` and a misspelled directory name both just fail). Cross-shell
picture varies per item: fish natively corrects near-miss spelling
(`cd /tmp/foo` when only `/tmp/Foo` exists, an interactive-only
convenience with no POSIX/dash equivalent); `pushd`/`popd`/`dirs` (a
directory stack) are in bash/ksh93/zsh but not dash/fish (fish uses
`dirh`/its own history-based equivalent instead); `cd -N` (jump N
entries back in the stack) rides on the same stack; `$CDPATH` (a
colon-separated search path for `cd`'s bare-name argument, like `$PATH`
for directories) is POSIX-mandated and present in all five, notably
including dash. Rush's `cd` builtin (`src/builtins.rs`) today only
handles a bare path, `~`, and `-` (previous directory via `$OLDPWD`) —
no stack, no `$CDPATH` lookup, no fuzzy match. **Effort: S** for
spelling-correction alone (a simple edit-distance check against sibling
directory names when the literal path doesn't exist); **M** for the
`pushd`/`popd`/`dirs`/`cd -N`/`$CDPATH` cluster (a new directory-stack
data structure plus `$CDPATH` lookup logic, both fairly mechanical
additions to the existing `cd` builtin).

Implemented, the whole bundle. The directory stack: `pushd dir` (cd +
push, printing the stack in bash's exact current-dir-first,
`~`-abbreviated format — verified byte-identical), bare `pushd` (swap
top two), `popd`, `dirs`/`dirs -c`, and `cd -N` (1-based jump into the
stack, zsh's spelling — bash has no `cd -N`). `$CDPATH`: a bare
relative name that doesn't resolve locally is searched through the
colon-separated path, and the resulting directory is printed, as POSIX
requires (verified against bash). Spelling correction is
**interactive-only**, like fish's: when the literal path doesn't exist
(and `$CDPATH` didn't resolve it either), a *unique* sibling within
edit distance 2 (or a case-insensitive match) is taken, with a
`cd: corrected to …` notice on stderr — a script's typo still fails
exactly as before, verified. Unit tests cover the distance helper and
both interactive/non-interactive correction paths; an integration test
covers the stack (bash-identical output), `$CDPATH`, `cd -N`, and the
empty-stack errors.

### C73 — No runtime `set -o vi`/`set -o emacs` line-editing mode switch ✅ done
POSIX-mandated (`set -o vi`), present in bash/ksh93/zsh (`emacs` is the
default in all of them; dash and fish don't support the vi keybinding
switch at all). Confirmed directly: rush's `set_cmd` (C52) has no `vi`/
`emacs` case, so the option is silently ignored rather than switching
keybindings. `rustyline` itself already supports vi-style editing via
`Config::edit_mode(EditMode::Vi)`, so the underlying capability exists —
but rush's `Editor` is constructed once at startup from a fixed `Config`
with no plumbing to reconfigure it mid-session in response to a builtin
command run after the fact. **Effort: M–L** — needs either rebuilding
the `Editor` in place (losing in-memory history unless explicitly
carried over) or a `rustyline` version/API that supports swapping
`edit_mode` on a live `Editor`, whichever proves cleaner in practice.

Implemented via the first of the write-up's two routes — rebuilding the
`Editor` — with the history-loss caveat handled explicitly: `set -o
vi`/`set -o emacs` (and their `+o` inverses) flip a tracked option, and
the interactive loop, on noticing the change before the next prompt,
constructs a fresh editor with the new `EditMode`, re-applies the
helper and C70's abbreviation binding, and *carries the in-memory
history across* entry by entry. The option rides C52's `-o` machinery
(so it appears in `set -o`/`set +o` listings and gets C41's
validate-then-apply rollback), and — matching bash — contributes no
letter to `$-`. The toggle/listing half is integration-tested; the
editor-rebuild half is interactive-only by nature.

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

### C33 — History-based autosuggestions ✅ done
Native in fish; common via plugin in zsh. Shows a greyed-out completion of
the current line based on history as you type.

**Turned out much smaller than the M-effort estimate**: `rustyline` itself
already ships a ready-made `hint::HistoryHinter` — searches history
backward from the current entry for the most recent one that starts with
what's typed so far, offering the remainder as the hint, with no
suggestion when the line is empty or already an exact match. `RushHelper`
(`src/completion.rs`) now holds one and delegates its own `Hinter::hint`
straight to it, rather than the previous no-op impl. The only genuinely
new code is `Highlighter::highlight_hint`, dimming the suggestion (ANSI
`\x1b[2m...\x1b[0m`) so it visually reads as a suggestion rather than
text already on the line — the same visual language fish and
zsh-autosuggestions use. Accepting it (right arrow at end of line) and
the underlying search are both rustyline's own key-binding/history
machinery, unmodified.

Verified end-to-end against the actual compiled binary under a real
pseudo-terminal (`pty.fork()`): typing `echo he` after `echo hello world`
is in history renders the typed prefix followed by the dimmed suggestion
`llo world`; accepting it with the right arrow and pressing Enter runs
the full `echo hello world` and prints `hello world`. This is inherently
a live-terminal rendering feature — `Editor::readline` itself falls back
to plain file-style line reading (no raw mode, no hint rendering at all)
whenever stdin isn't a real TTY, which is also why it can't be covered by
the piped-stdin integration-test pattern used for C32; covered instead by
unit tests exercising `RushHelper`'s `Hinter`/`Highlighter` impls directly
against a `rustyline::history::DefaultHistory` and a `Context::new`
(rustyline's own testing constructor for exactly this).

### C34 — Argument- and context-aware completion ✅ done
Native and rich in fish; rich in zsh via compsys; bash gets it only via the
separate bash-completion project. Rush's completion used to be file/PATH/
builtin-name only — no notion that a command's second word should complete
differently than its first.

**Scope, deliberately bounded** rather than a full fish/zsh completion-spec
engine (a per-command argument-spec system, `complete -F`-style
programmable completion, etc. — out of scope, a documented gap): a fixed
set of the highest-value cases where plain filename completion is rarely
what's actually wanted.

- **Variable names** — a bare, still-open `$name`/`${name}` (unquoted, at
  the start of the word being completed) completes every shell variable
  name plus every environment variable name, reconstructing `$name` or
  `${name}` (auto-closing the brace) in the replacement. A new
  `vars::names()` enumerates the shell-variable side (`vars.rs`'s own
  `VARS` map had no existing "list all names" function — only
  `exported()`, scoped to exported scalars for env-seeding).
- **`cd`'s own argument** completes directories only, not files — reuses
  rustyline's own `FilenameCompleter` for the actual path-matching/
  escaping logic, then filters its candidates down to directories via a
  plain filesystem check. (Confirmed directly against real bash's own
  bare-readline defaults, no bash-completion loaded, that this isn't bash
  behavior either — `cd` there lists plain files alongside directories
  too; genuinely a fish/zsh-parity addition, not something plain bash
  already does.)
- **`export`/`unset`/`local`/`declare`'s arguments** complete variable
  names (the same enumeration as `$`/`${` above) — except a word starting
  with `-` (a flag), which is deliberately left uncompleted rather than
  nonsensically offering variable names for it.
- **`alias`/`unalias`'s arguments** complete existing alias names (`alias.rs`
  already had a `pub fn all()` — no new code needed there) — but only
  while still typing the bare name, before an `=` (which starts a new
  alias's *value*, arbitrary text that isn't an alias name to complete
  against).
- **(Unix only) `fg`/`bg`/`kill`/`wait`'s arguments** complete `%n` job
  specs from the live job table, in exactly the plain `%N` format those
  builtins themselves already parse (confirmed by reading `wait_one`/
  `kill_cmd`/`select_index`, all of which just `strip_prefix('%')` then
  parse an integer — no `%+`/`%-`/`%string` forms are supported today, so
  completion doesn't offer those either). A new `job::ids()` enumerates
  the job table (previously nothing outside `job.rs` could list job ids).

Explicitly out of scope, each an accepted, documented gap given this
item's effort budget: flag completion for any builtin (`export -`, `set
-`, `declare -`); variable completion when `$`/`${` isn't the *start* of
the word being completed (`foo$bar`, concatenated text plus a reference);
variable completion unwrapped out of an open double quote (`"$HO` is
treated as a literal word starting with `"`, not specially unquoted first
— the same not-lexer-accurate approach this module already had for
command-position detection); any actual per-command argument specs for
external commands (`git <TAB>` completing subcommands, `ssh <TAB>`
completing known hosts, etc.) — real fish/zsh completion is ultimately a
whole ecosystem of per-command completion scripts, not something a single
shell's own core reasonably reimplements.

Verified end-to-end against the compiled binary under a real
pseudo-terminal (`pty.fork()`) across all five cases above — including
that `cd`'s directory filtering actually excludes a real file sitting
alongside a real directory, that `${VAR<TAB>` auto-closes the brace, and
that a job spawned with `sleep 100 &` really does complete `fg %<TAB>` to
`fg %1`. Also covered by 8 new unit tests exercising the pure completion
functions directly (`current_command`, `complete_variable`,
`complete_variable_name_arg`, `complete_alias_name`,
`job_spec_candidates`, `is_directory`).

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
- **C41 (`$`/`$PPID`/`$-`) and C45 (`readonly`) were the two
  highest-leverage items in the fresh C41–C73 pass** — both POSIX-mandated,
  present in all five comparison shells including dash, and failing
  silently or wrongly rather than just being unsupported. Both are done.
- **C55 (`[[ ]]`) is the single largest undertaking in the whole document**
  — new lexer tokens, a new recursive parser production, and a new
  evaluator — but it's also a prerequisite for C56 (`=~` regex), so it's
  worth sequencing before rather than after that one.
- **C42 (POSIX character classes) and C49 (`typeset`)** were both
  small, self-contained wins with no dependencies on anything else in the
  new batch — both done.

---

# 2026-07-11 review pass — 57 new gaps, C74–C130

A third full comparison pass, run after the rusty_regx/rusty_libc
integration landed (C1–C73 all done). Method: five parallel review
sweeps — expansions/quoting/arrays, builtins, grammar/control flow,
shell options/environment/job control, and interactive UX — each
differentially testing the built rush binary against real bash 5.2
(`bash -c` vs `rush -c` on identical snippets) and reading the relevant
`src/` modules. Every scripting-tier item below has a live, reproduced
output difference; interactive-tier items are verified by source reading
(absence in `src/main.rs`/`completion.rs`/the rusty_lines `Hooks` wiring)
plus bash/zsh/fish documented behavior. Nothing here duplicates C1–C73.

Effort scale as before: S (hours), M (a day or two), L (multi-day).

## Tier I — Correctness: silent wrong behavior, aborts, crashes (C74–C88)

These are the highest-priority items: not "feature missing" but "rush
runs the script and produces wrong results or dies".

### C74 — IFS splitting is applied to literal command-line words
`IFS=x; echo axb` → rush `a b`, bash `axb`. Splitting must apply only to
the *results of expansions*, never to literal source text. Any script
that sets a custom IFS silently corrupts every later command line
containing an IFS character. **Effort: M**

### C75 — Temporary env-assignment prefix before a builtin isn't scoped
`IFS=: read -r x y <<< "1:2:3"; echo "$x|$y"` → rush `1:2:3|`, bash
`1|2:3`. The prefix assignment neither applies to the builtin nor gets
restored afterward (`IFS` is left clobbered to empty). Breaks the
canonical `IFS=x read` idiom twice over. **Effort: M**

### C76 — Quotes inside `${v:-word}` defaults are literal; inner whitespace mangled
`unset v; echo "${v:-"a b"}"` → rush `"a b"` (quote characters in
output), bash `a b`. Unquoted form also collapses runs of spaces.
Extremely common default-value idiom produces wrong strings. **Effort: M**

### C77 — Adjacent text around `"x${arr[@]}y"` collapses the array
`a=(1 2); printf "[%s]" "x${a[@]}y"` → rush `[x1 2y]` (one word), bash
`[x1][2y]` (prefix glues to first element, suffix to last). Same for
`"$@"`. **Effort: M**

### C78 — Backslash-newline line continuation broken
`echo one \` + newline + `two` → rush passes a literal newline into the
argument (`-c` mode) or runs `two` as a second command (stdin mode);
bash deletes the `\<newline>` pair during tokenization. One of the most
common script formatting conventions. **Effort: M**

### C79 — Backslash handling inside double quotes: `` \` `` kept, `\<newline>` not a continuation
`printf "%s" "\\\`"` → rush `` \` ``, bash `` ` ``. A backslash-newline
inside `"..."` also produces a stray backslash + newline instead of
joining the line. **Effort: S**

### C80 — EXIT trap not reset in subshells — cleanup runs twice
`trap "echo bye" EXIT; (echo sub)` → rush prints `bye` twice, bash once.
Every `( … )` or `$( … )` under an EXIT trap double-runs tempfile
deletion / unlock / kill-children cleanup. **Effort: S–M**

### C81 — `set -e` not suppressed inside functions called under `||`/`&&`/`if`
`set -e; f(){ false; echo in; }; f || echo caught` → rush exits at the
inner `false` (no output, rc 1); bash prints `in`, rc 0. The single most
common errexit interop pattern aborts the whole script. (Plain
`if false` already works.) **Effort: M**

### C82 — Builtins as pipeline stages are exec'd as external commands
`echo hi | read x`, `readonly -p | grep RV`, `alias | cat`, `jobs -r |
grep sleep` → all fail with `No such file or directory (os error 2)`;
each works in rush *without* the pipe. The pipeline stage builder in
`exec.rs` doesn't route builtins/declaration-words through builtin
dispatch. **Effort: M**

### C83 — Deep function recursion aborts the whole process; `FUNCNEST` ignored
Runaway recursion hits a native Rust stack overflow (`SIGABRT`, rc 134)
at ~2700 frames; bash honors `FUNCNEST=n` with a recoverable error and
degrades more gracefully. Needs a call-depth counter in `func.rs`, a
`FUNCNEST` check, and a hard internal cap below the native stack limit.
**Effort: M**

### C84 — Assoc-array assignment with a quoted key isn't parsed as an assignment
`declare -A a; a["x y"]=1` → rush: `a[x y]=1: command not found`; bash
assigns. Any assoc key containing spaces is unusable. **Effort: M**

### C85 — Negative array subscripts silently wrong (read and write) ✅ done
`a=(x y z); echo "${a[-1]}"` → rush empty, bash `z`; `a[-1]=Q` is
silently dropped. Returns empty / no-ops instead of last-element access —
silent logic errors. **Effort: S**

### C86 — Slicing/`#` on positional parameters is a hard error ✅ done
`set -- a b c d; echo "${@:2:2}"` → rush `bad substitution` (exit 1),
bash `b c`. `${#*}`/`${#@}` also error. (Named-array forms already
work — only `@`/`*` fail.) **Effort: S**

### C87 — A redirection with no command (`> file`) is an error instead of truncating
`> f` → rush `empty command` (rc 1), file untouched; bash truncates/creates.
The canonical truncate idiom fails and the error aborts the line.
**Effort: S**

### C88 — `return` at top level silently exits the whole script
`return 5` outside any function/source → rush exits the script with
rc 5; bash warns `can only return from a function or sourced script` and
continues (rc 0 line status). Sourced-file behavior already matches.
**Effort: S**

## Tier II — Missing builtins and builtin flags (C89–C103)

### C89 — `read`/`mapfile` flag coverage
`read` supports *no* option flags: `-t` (timeout), `-n`/`-N` (chars),
`-d` (delimiter), `-a` (array), `-u` (fd), `-p` (prompt), `-s` (silent),
`-e` each print `read: -X: invalid option` and leave the variable empty.
`mapfile -d` is likewise rejected (`only -t is supported`). Timed,
per-char, prompted, and array reads all silently produce empty
variables. **Effort: L** (flag pass is easy; `-t`/`-n` need
termios/timeout plumbing)

### C90 — `echo -e` / `-E` / combined flags printed as literal text
`echo -e "a\tb"` → rush prints `-e a\tb`; only a lone `-n` is
recognized. Scripts emit garbage flag text into their output.
**Effort: S**

### C91 — `let` builtin missing
`let x=3+4; echo $x` → `let: command not found`, `$x` empty; scripts
proceed with empty variables. Thin wrapper over the existing `(( ))`
arithmetic (C28/C29). **Effort: S**

### C92 — `builtin` builtin missing
`cd(){ builtin cd "$@" && ls; }` — the standard shadow-a-builtin wrapper
pattern — recurses or fails (`builtin: command not found`). **Effort: S**

### C93 — Programmable completion builtins: `complete` / `compgen` / `compopt`
All three are `command not found`, so every bash-completion script fails
to load. rush has a real completion engine (`completion.rs`, C34) but no
programmable interface (`COMP_WORDS`/`COMP_CWORD`/`COMPREPLY` protocol).
**Effort: L**

### C94 — `times`, `help`, `caller`, `enable`, `suspend` all missing
Each returns 127 where bash returns a proper result/status — and 127
breaks feature-detection (`if help foo >/dev/null …`). **Effort: S each
(help M)**

### C95 — `test`/`[` missing operators: `-v`, `-o`, string `<`/`>`, `-R`
`x=1; test -v x` → rush `too many arguments` rc 2; bash rc 0. Exit code
2-instead-of-0/1 actively flips conditionals. `test -v` is the standard
"is variable set" check. **Effort: S–M**

### C96 — `declare -p` / `-f` / `-F` silently print nothing (rc 0)
`declare -p x` → no output, rc 0; bash prints `declare -- x="5"`.
`declare -F nosuch` returns 0 instead of 1. Round-tripping
(`eval "$(declare -p a)"`) and function-existence tests give wrong
results — worse than an error because nothing fails visibly.
**Effort: M**

### C97 — `unset -f` doesn't remove functions
`f(){ :; }; unset -f f; type f` → still `f is a function`. Functions
cannot be undefined at all. **Effort: S**

### C98 — `export -f` (functions) and `export -n` (un-export) unsupported
`export -f f; bash -c f` → child gets `command not found` (needs the
`BASH_FUNC_name%%=` env encoding); `export -n FOO` leaves FOO exported.
Exported functions are load-bearing for xargs/parallel/make recipes.
**Effort: M (`-n` is S)**

### C99 — `printf` gaps: `-v var`, `%(fmt)T`, leading-quote char codes
`printf -v x "%03d" 7` treats `-v` as the format (pollutes stdout, var
unset); `printf "%(%Y)T" 0` → invalid conversion (bash: `1970`);
`printf "%d" '"A'` → invalid number (bash: `65`). **Effort: M**

### C100 — `type -p`/`-P` and `hash -t`/`-d`/`-p` flags unrecognized
`type -p ls` errors + prints the wrong format (breaks `$(type -p x)`
captures); `hash -p /path name` can't seed the table. **Effort: S**

### C101 — Assorted verified flag gaps (batch)
Each breaks a real pattern; all reproduced:
- `kill -s TERM pid` → `invalid signal specification` (signal-by-name).
- `kill -l 143` → error; bash prints `TERM` (decode `$?` of a
  signal-killed child).
- `trap -- "cmd" EXIT` → `--` misparsed (matters because `trap -p`
  output itself uses `--`).
- `exec -a name` / `-l` / `-c` → treated as filenames (argv[0] spoofing,
  clean-env exec).
- `cd -P` / `-L` → `No such file or directory`; `pwd -L` prints the
  physical path after cd'ing through a symlink (no logical-path
  tracking — M).
- `dirs -v` prints no index column; `popd +1` pops the top instead of
  entry 1.
**Effort: S each (cd/pwd logical paths M)**

### C102 — `fc` builtin missing entirely (POSIX-mandated)
No `fc -l` (numbered listing), `fc -s old=new` (quick re-run), or
`fc [-e editor] range` (edit-and-execute past commands). The editor's
Ctrl-X Ctrl-E only covers the *current* line. **Effort: M**

### C103 — `history` builtin missing
`history | grep foo` — among the most common interactive idioms — fails
with 127. No `-c` clear, `-d N` delete, `-a`/`-r`/`-w`/`-n` file sync,
`-s`, `-p`. rusty_lines already exposes `history()`,
`add_history_entry`, `save_history`, `append_history`; rush never
surfaces them as a builtin. **Effort: M** (builtin S; threading the
editor's history store into the builtin layer is the M part)

## Tier III — Options, environment, invocation, job control (C104–C111)

### C104 — Invocation flags missing: `-s`, `-i`, `-l`, `-r`, `--posix`, `--norc`/`--rcfile`, `-O`/`+O`, `-D`
`main.rs` understands only `-c`, a script path, and a special-cased
`-n`; every other flag is treated as a script filename
(`rush: -s: No such file or directory`). Blocks login-shell use,
restricted shells, and `curl | rush -s -- args` pipelines. **Effort: L**

### C105 — `$BASH_ENV` startup file not honored in non-interactive mode
`BASH_ENV=/tmp/e rush -c …` never sources the file; CI/wrapper-injected
setup silently vanishes. **Effort: S**

### C106 — Standard shell variables not seeded; `UID` writable; `SHLVL` not incremented
`UID`, `EUID`, `HOSTNAME`, `OSTYPE`, `HOSTTYPE`, `MACHTYPE`,
`BASH_VERSION`/`BASH_VERSINFO` analogs, `SHELLOPTS`/`BASHOPTS` are all
unset; `UID=5` succeeds silently (bash: readonly error); inherited
`SHLVL` isn't incremented. Root checks (`$UID -eq 0`), OS branching, and
nesting detection all take wrong branches. **Effort: M**
(`SHELLOPTS`/`BASHOPTS` need live option reflection)

### C107 — `set` short options largely unsupported
`-a` (allexport), `-b`, `-f` (noglob), `-h`, `-k`, `-v` (verbose), `-B`,
`-E` (errtrace), `-P`, `-T` (functrace), and `set -o
posix/errtrace/functrace` all print `set: -X: not supported`. `set -a`
(env-file sourcing) and `set -f` (safe filename handling) are common;
`set -euEo pipefail` preambles fail outright. **Effort: M–L** (some can
be accepted as no-ops initially)

### C108 — `shopt` table is glob-only (5 options)
Missing everything else: `lastpipe`, `inherit_errexit`, `xpg_echo`,
`patsub_replacement`, `login_shell`, `huponexit`, `execfail`, `cmdhist`,
`histappend`, `checkwinsize`, `sourcepath`, `extdebug`, `autocd`,
`nocaseglob`, `nocasematch`, `direxpand`/`dirspell`, `hostcomplete`, ….
`shopt -s <any>` in a ported script is a hard `invalid shell option
name` error, and the gated behaviors are unreachable. `nocasematch`
also affects `[[ == ]]`/`case`; `autocd` is the type-a-directory habit
for zsh/fish converts (one check in the command-not-found path).
`GLOBIGNORE` is likewise ignored, and bash 5.2's `patsub_replacement`
(`&` in `${v/pat/repl}`) doesn't expand. **Effort: M** (accept + wire
the high-value ones first: `lastpipe`, `inherit_errexit`, `nocasematch`,
`autocd`, `histappend`, `checkwinsize`, `xpg_echo`)

### C109 — `$PS4` not expanded for xtrace
`PS4='+${LINENO}: '; set -x` prints the literal `$LINENO` text. The
standard debugging idiom produces useless traces. (First-char repetition
already works.) **Effort: S**

### C110 — `wait -f` / `wait -p var` and `jobs -n`/`-r`/`-s` unsupported
bash-5.x `wait -n -p which` (identify the finished job) errors and
leaves the var unset; `jobs -r` ("any jobs still running?") is an
invalid option. Both are small option-parsing additions over the
existing C13/C64 machinery. **Effort: S**

### C111 — `exec` persistent redirections: fd > 3 fails; dup, close, and move all fail
`exec 7>/tmp/x`, `exec 4>&1`, `exec 3>&-`, `exec 1>&3-` → all `Bad file
descriptor` (rc 1) — and the error aborts the rest of the script. Blocks
`exec 5>>log` logging setups, the ubiquitous `exec 3>&1 … 3>&-`
fd-juggling, and any future `BASH_XTRACEFD`. (C38 fixed *per-command*
high fds; this is `exec`'s persistent form.) **Effort: M**

## Tier IV — Language/syntax parity (C112–C121)

### C112 — `time` reserved word missing (with `TIMEFORMAT`, `time -p`)
`time cmd` → 127. There is no external fallback for timing pipelines or
builtins. Needs a reserved word wrapping a full pipeline, rusage
collection, and `TIMEFORMAT`/`-p` formatting. **Effort: M**

### C113 — `function name { …; }` definition syntax unsupported
The ksh/bash form is a parse error — and `function name() { …; }`
silently *misparses* (runs the body eagerly, then 127) rather than
erroring cleanly. Add `function` to the parser's reserved words and
accept both header shapes. **Effort: S**

### C114 — `|&` pipe shorthand unsupported
`cmd |& cat` → `expected a command`. Desugar to `2>&1 |` at parse time.
**Effort: S**

### C115 — `{varname}>file` variable-fd redirection unsupported
`exec {x}>/dev/null; echo $x` should allocate fd ≥10 and set `$x`;
rush treats `{x}` as a filename (or a command name in prefix position).
Standard idiom for flock/lock-file and saved-stream management.
**Effort: M**

### C116 — Arithmetic evaluator gaps: `base#n` literals, empty expression, string re-evaluation
- `$((2#101))`/`$((16#ff))` → `unexpected character '#'` (bash: 5/255).
- `$(( ))` (e.g. from `$(($empty))`) → error instead of 0.
- `x=1+2; echo $((x))` → `not an integer` (bash evaluates a variable's
  string value as a sub-expression, recursively; needs a depth cap).
**Effort: S + S + M**

### C117 — Tilde expansion: `~user`, `~+`, `~-` unimplemented ✅ done
All three pass through literally (bash: passwd lookup, `$PWD`,
`$OLDPWD`). **Effort: S**

### C118 — Remaining expansion operators: `@U @u @L @K @k @P` transforms; `$"…"` ✅ done
Case transforms (`${v@U}` etc., bash 5.1+), assoc round-tripping
(`${a[@]@K}`), prompt expansion (`${PS1@P}`) are `bad substitution`
hard errors (only Q/E/a/A exist). `$"hello"` prints a stray `$`.
**Effort: S**

### C119 — `$'…'` escape coverage: `\xHH`, octal `\nnn`, `\uXXXX`, `\cX` left literal ✅ done
`echo $'\x41'` → `\x41` (bash: `A`). Byte-level string construction
produces literal backslash text. (`\n`/`\t`/`\e` already work.)
**Effort: S**

### C120 — `nocaseglob` / `nocasematch` behaviors (see also C108)
`shopt -s nocasematch; [[ ABC == abc ]]` unreachable — tracked with the
shopt table in C108 but called out separately because it changes
matching *semantics* in `[[ ]]`/`case`/globs, not just an option table
entry. **Effort: M** (glob and pattern matchers need a case-fold mode)

### C121 — `/dev/tcp/host/port` and `/dev/udp` pseudo-devices not intercepted
Redirections to them hit the kernel (ENOENT) instead of opening a
socket. Niche but distinctive bash capability (port probes, minimal
clients). **Effort: M**

## Tier V — Interactive UX (C122–C130)

### C122 — History configuration variables all ignored
`HISTFILE`, `HISTSIZE`, `HISTFILESIZE`, `HISTCONTROL`, `HISTIGNORE`,
`HISTTIMEFORMAT`: zero hits in `src/`. Path is hardcoded
`~/.rush_history`, history is unbounded, and — a real privacy
regression — `HISTCONTROL=ignorespace` is unhonored, so a
leading-space ` secret-cmd` is recorded. The editor already has
`set_max_history_len` and `set_history_dedup` knobs, unwired.
**Effort: S–M** (HISTTIMEFORMAT needs a file-format decision)

### C123 — History persists only on clean exit; concurrent sessions clobber each other
`save_history` (a whole-file overwrite) runs once at REPL exit — a
killed session/SSH drop loses everything, and the last of two concurrent
sessions to exit wins. rusty_lines *already ships* `append_history`
(documented as bash's `histappend` semantics); call it per accepted
line. **Effort: S**

### C124 — Multi-line compound commands recorded as history fragments (no `cmdhist`)
Each physical line of a `for`/`if` typed interactively becomes its own
(syntactically invalid) history entry. rusty_lines'
`add_history_entry` already joins embedded newlines with `; ` — rush
just never hands it the whole command. Buffer the lines and add one
joined entry after `parser::parse` succeeds. **Effort: S**

### C125 — PS1 escape coverage tiny; `\[`/`\]`/`\e` render as literal garbage
Only `\w \W \u \h \$ \? \n \\` exist; unknown escapes are kept literal —
so the single most common bash prompt,
`PS1='\[\e[32m\]\u@\h\[\e[0m\] \w\$ '`, renders visibly corrupted.
Missing: `\[ \] \e \a \nnn`, time/date (`\t \T \@ \A \d \D{fmt}`),
`\j \! \# \v \V \s \l`. Also `\u`/`\h` read env vars instead of
getpwuid/gethostname, so `\h` is usually empty on Linux. **Effort: S**

### C126 — No `$`-expansion of PS1 (promptvars); no `PROMPT_COMMAND`; no PS0
`prompt()` runs only the escape pass — `PS1='$(git_branch) \$ '` (the #1
prompt customization in the wild) never expands, while RPS1 *does* get
`expand_dollars` (internal inconsistency). No `PROMPT_COMMAND` hook
(zsh `precmd`) runs before each prompt, so exit-status coloring,
terminal-title updates, and `history -a`-style hooks are impossible.
**Effort: S**

### C127 — PS2 hardcoded to `"> "`; `PROMPT_DIRTRIM` missing
`$PS2` is never read; continuation prompts can't be customized, and deep
paths flood `\w`. **Effort: S**

### C128 — No `bind` builtin, no inputrc-style keybinding configuration
Keymaps are compiled into rusty_lines with no rebinding surface: no
`bind -x` (fzf integrations), no readline variables
(`completion-ignore-case`, `show-all-if-ambiguous`, `menu-complete`),
no `~/.inputrc` analog. Hard blocker for users with muscle-memory
bindings. **Effort: L** (needs a rusty_lines rebinding API; a
S-sized subset: expose completion case-folding etc. as shopt options)

### C129 — `COLUMNS`/`LINES` never set or updated (no `checkwinsize`)
The editor queries winsize internally but the shell vars are never
written — `exec.rs`'s own `select` implementation reads `$COLUMNS`,
which is always empty. Set at startup + after each foreground job
(bash 5 default). **Effort: S**

### C130 — `IGNOREEOF` and `TMOUT` unsupported
One stray Ctrl-D unconditionally kills the shell (`ReadResult::Eof =>
break`) — no "Use `exit` to leave" guard; `TMOUT` idle auto-logout
(a hardening requirement in some environments) is absent. **Effort: S
(IGNOREEOF) / M (TMOUT — needs a read timeout in rusty_lines)**

## Suggested sequencing for the C74+ batch

- **Tier I first, and within it C74/C75/C76 (IFS + prefix assignments +
  default-quoting)** — all three silently corrupt data in bread-and-butter
  scripts, and C74/C75 share the same expansion-pipeline territory.
- **C82 (builtins in pipelines)** unblocks a whole family of everyday
  one-liners (`… | read`, `alias | grep`, `declare -p | grep` — the
  latter also needs C96) and was independently rediscovered by two
  separate review sweeps — a sign of how often real usage hits it.
- **C80 + C81 (EXIT-trap double-fire, errexit-in-functions)** are the
  two most dangerous trap/errexit semantics bugs; both are contained
  changes in exec/trap state handling.
- **The history cluster C103 + C122 + C123 + C124** is the standout
  cheap win: rusty_lines already ships every needed API
  (`append_history`, `set_max_history_len`, newline-joining
  `add_history_entry`); rush just doesn't call them.
- **The prompt cluster C125 + C126 + C127** is the first thing any
  migrating bash user sees (their PS1 renders as garbage today); all S.
- **C89 (`read` flags) and C93 (programmable completion) are the two L
  items** with the broadest external-script compatibility payoff —
  bash-completion files and `read -p/-s/-t` idioms are everywhere.
- **C104 (invocation flags) is the login-shell gate** — until `-l`/`-i`
  parse, rush can't be anyone's chsh target.
