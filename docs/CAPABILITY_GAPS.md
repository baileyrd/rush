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
on top, and a few POSIX-mandated pieces (`read`, `${var%pattern}`, `for name;
do`) still missing entirely.

---

## Comparison matrix

A cross-section, not the full 32 below — enough to place rush relative to a
strict POSIX shell (dash), the bash family, and the interactive-first shells
(zsh, fish). ✅ full · 🟡 partial/simplified · ❌ not implemented · — not
applicable to that shell's own model.

| Capability | rush | dash | bash | ksh93 | zsh | fish |
|---|---|---|---|---|---|---|
| Real pipes / job control / forked subshells | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `#`/`##`/`%`/`%%` param. expansion | ❌ | ✅ | ✅ | ✅ | ✅ | — |
| `read` / `printf` / `shift` / `getopts` | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡 |
| `local` function-scoped vars | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `wait` / `disown` | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `set -e` / `-u` / `-o pipefail` | 🟡 | 🟡 | ✅ | ✅ | ✅ | — |
| Indexed / associative arrays | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Brace expansion `{a,b,c}` | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Compound as one pipeline stage | 🟡 | ✅ | ✅ | ✅ | ✅ | ✅ |
| Traps beyond EXIT/INT firing | 🟡 | ✅ | ✅ | ✅ | ✅ | — |
| Context-aware completion | ❌ | — | 🟡 | 🟡 | ✅ | ✅ |
| History autosuggestion | ❌ | — | ❌ | ❌ | 🟡 | ✅ |
| Native Windows job control | ❌ | — | — | — | — | 🟡 |

---

## Summary counts

- **Tier I — correctness/POSIX risk:** 6
- **Tier II — missing standard builtins:** 11
- **Tier III — scripting-safety idioms:** 4
- **Tier IV — bash/ksh/zsh language parity:** 10
- **Tier V — interactive UX:** 3

---

## Tier I — Correctness & POSIX risk

These don't just lack a feature — a script that assumes them can silently do
the wrong thing under rush instead of erroring, which is the worse failure
mode.

### C1 — Suffix/prefix parameter expansion: `${v%pat}` `${v%%pat}` `${v#pat}` `${v##pat}`
POSIX-mandated; present in dash, bash, ksh93, zsh. `expand_braced` currently
only recognizes `-`/`=`/`+`/`?` after an optional `:`, plus `${#name}` for
length — the whole pattern-stripping family is absent. This is the standard,
portable way to strip an extension or a path (`${file%.txt}`,
`${path##*/}`) without spawning `basename`/`sed`, and it's everywhere in real
scripts. **Effort: M.**

### C2 — `for name; do …; done` should iterate `"$@"`
POSIX-mandated shorthand, present in dash/bash/ksh/zsh. Today, omitting the
`in` clause leaves rush's word list empty, so the loop body silently never
runs — not an error, just quietly wrong. **Effort: S.**

### C3 — Compound command as one stage of a larger pipeline: `(cmd) | grep x` (tracked)
Present in every comparison shell. Rush can capture a *lone* compound via
`$(...)` and run one as an entire pipeline by itself, but a compound as one
stage among several in a real pipe still hard-errors. Needs File-based pipe
plumbing for a forked compound stage, not the current `Stdio`-based
approach. **Effort: L.**

### C4 — `set -e` doesn't match bash/POSIX's exact rule (tracked)
Correct in dash, bash, ksh, zsh: a failing command is exempt from errexit
unless it's positionally last in an `&&`/`||` list. Rush's simplified rule
fires on any job's *final* nonzero status instead — `set -e; false && true`
exits under rush but not under real bash. A script tested against bash's
actual semantics can abort earlier than its author intended. **Effort: M.**

### C5 — Real `$IFS`-driven word-splitting
POSIX-mandated; present in dash/bash/ksh/zsh. Rush hardcodes ASCII whitespace
as the split set. `IFS=','`-style field splitting — a standard, portable
parsing technique — silently does the wrong thing rather than honoring the
variable. **Effort: M.**

### C6 — `test`/`[` logical combinators `-a` / `-o` (tracked)
POSIX-mandated, present in dash/bash/ksh/zsh (bash discourages but still
ships them). Lower risk than the rest of this tier — absence is a hard usage
error, not silent wrongness — but still a real portability gap for scripts
targeting strict POSIX sh. **Effort: S.**

---

## Tier II — Missing standard builtins

POSIX-mandated in every comparison shell here. Each one blocks a whole
category of otherwise-ordinary scripts outright, rather than just being an
inconvenience.

### C7 — `read`
Arguably the single highest-value missing builtin. Without it: no `while
read line; do …; done < file`, no prompting for input, no parsing
`IFS`-delimited fields from a line. Blocks an entire class of everyday
scripts on its own. **Effort: M.**

### C8 — `printf`
The portable, correct way to emit formatted output — real scripts avoid
`echo` for exactly this reason, and rush's own `echo` has no `-e` at all,
making this more urgent than usual. **Effort: M.**

### C9 — `shift [n]`
The missing piece connecting positional parameters and `case` (both already
supported) into the ubiquitous `while [ $# -gt 0 ]; do case $1 in …; esac;
shift; done` argument-parsing loop. **Effort: S.**

### C10 — `local` (function-scoped variables)
Near-universal extension (dash, bash, ksh, zsh); fish scopes by default.
Right now every rush function shares the caller's entire variable
namespace — a function's own `i=0` silently clobbers the caller's `i`.
Functions already work; using them safely for anything nontrivial doesn't.
**Effort: M.**

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
