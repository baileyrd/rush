# rush

[![CI](https://github.com/baileyrd/rush/actions/workflows/ci.yml/badge.svg)](https://github.com/baileyrd/rush/actions/workflows/ci.yml)

A small, bash-compatible shell written in Rust — built to grow into a daily-use tool.

**Status: experimental.** rush covers most of the core POSIX shell language and
has a growing test suite, but it hasn't been hardened as a daily-driver shell
yet — treat it as a project to explore and contribute to, not (yet) a drop-in
`chsh` replacement. Job control (`&`, Ctrl-Z, `fg`/`bg`/`jobs`) is **Unix
only**; on Windows, rush runs foreground commands only.

`rush` reads a command line, lexes and parses it, expands it, then executes the
result. It covers most of the core POSIX shell language: pipelines and
redirection (including `2>`/`2>&1` and here-documents), the full set of
expansions (variables, `${…}` operators, `$?`, positional parameters, command
substitution, arithmetic `$((…))`, globbing, and tilde), control flow
(`if`/`while`/`for`/`case`, `break`/`continue`), shell functions with recursion,
subshells, and — on Unix — background jobs with real job control. It runs
interactively (with multi-line continuation and history) or as a script
(`rush script.sh args`).

```
/home/baileyrd/projects/rust_bash $ ls | grep rs | wc -l
5
/home/baileyrd/projects/rust_bash $ echo "hello world" > out.txt
/home/baileyrd/projects/rust_bash $ sort < out.txt
hello world
/home/baileyrd/projects/rust_bash $ echo "home is $HOME, here is $(pwd)"
home is /home/baileyrd, here is /home/baileyrd/projects/rust_bash
```

## Features

| Feature | Status | Notes |
|---|---|---|
| REPL with line editing | ✅ | via [`rustyline`](https://crates.io/crates/rustyline) |
| Tab completion | ✅ | builtins and `$PATH` executables in command position; `$`/`${` completes variable names, `cd` completes directories only, `export`/`unset`/`local`/`declare` complete variable names, `alias`/`unalias` complete alias names, `fg`/`bg`/`kill`/`wait` complete `%n` job specs (Unix); files otherwise |
| Persistent history | ✅ | stored in `~/.rush_history` |
| History expansion (`!!`, `!n`, `!$`, `!*`, `!:n`) | ✅ | bash-style bang-history recall; interactive only, quoting/escaping matches bash |
| History-based autosuggestions | ✅ | dimmed inline completion from history as you type (fish-style); accept with the right arrow |
| Startup file | ✅ | sources `~/.rushrc` (if present) at interactive startup |
| Prompt customization | ✅ | `PS1` (var or env), with `\w`/`\W`/`\u`/`\h`/`\$`/`\?`/`\n`/`\\`; falls back to `cwd $ ` |
| Quoting | ✅ | single quotes, double quotes, backslash escapes |
| Comments (`#`) | ✅ | `#` at a word boundary starts a comment to end of line |
| Pipelines (`\|`) | ✅ | N stages, stdout→stdin wiring; `!` negation; a compound (`if`/`while`/`(...)`/…) can be one stage among several on Unix (forks) |
| Redirection (`>`, `>>`, `<`, `2>`, `2>&1`, `&>`) | ✅ | per-fd to files, any fd (`3>file`, `4<&5`); fd duplication (`> f 2>&1`); `&>` both streams |
| Here-documents (`<<`) / here-strings (`<<<`) | ✅ | `<<EOF`, `<<-EOF` (tab-strip), `<<'EOF'` (no expansion); `cmd <<< "$var"` |
| Process substitution | ✅ | `<(cmd)`/`>(cmd)` via a real pipe + `/dev/fd/N`, non-blocking and concurrent (Unix only) |
| Builtins | ✅ | `cd`, `pwd`, `echo`, `export`, `unset`, `test`/`[ ]`, `true`, `false`, `:`, `break`/`continue`/`return`, `exit`, `alias`/`unalias`, `set`, `trap`, `read`, `printf`, `shift`, `local`, `declare`/`typeset`, `readonly`, `getopts`, `command`, `type`, `hash`, `.`/`source`, `eval` (+ `jobs`/`fg`/`bg`/`kill`/`wait`/`exec`/`umask`/`ulimit` on Unix) |
| `[[ ]]` extended test | ✅ | split/glob-safe operands, `&&`/`||`/`!`/`( )` nesting, pattern `==`/`!=` (quoting-aware), `<`/`>` string compare, arithmetic `-eq`…`-ge`, `-nt`/`-ot`/`-ef`; `=~` ERE matching with `$BASH_REMATCH` captures |
| Aliases | ✅ | `alias name=value`; a single, non-recursive substitution at command-word position |
| `set -e` (errexit) | ✅ | a failing command exits the shell; exempts `if`/`while`/`until` conditions; clustered flags (`set -euo pipefail`) parse as in bash |
| `set -u` (nounset) | ✅ | referencing an unset variable is an error; `:-`/`:=`/`:+`/`:?` and `$@`/`$*`/`$#`/`$?`/`$$` are exempt |
| `set -o pipefail` | ✅ | a pipeline's status is the rightmost non-zero stage, not just its last; applies inside `$(...)` too |
| `set -C` (noclobber) | ✅ | `>` refuses to overwrite an existing regular file; `>|` overrides; `>>`/devices exempt |
| `set -x` (xtrace) | ✅ | echoes each command (`$PS4`-prefixed) before running it; nesting in `$(...)` repeats `$PS4`'s first character |
| `trap` | ✅ | `EXIT` (every exit path), `INT` (Ctrl-C at an idle prompt), and (Unix) `TERM`/`HUP` — real signals, interrupting a blocking wait immediately; numeric/`SIG`-prefixed/lowercase specs all accepted (`trap 'cmd' 15`); `ERR` fires on errexit's condition |
| Variables & assignment | ✅ | `FOO=bar`, prefix `FOO=bar cmd`, `export`; shell vars shadow the environment; `declare`/`local` `-u`/`-l`/`-i` attribute transforms; `readonly`/`declare -r` read-only variables |
| Positional parameters | ✅ | `$0`, `$1`…, `${10}`, `$#`, `$*`, `$@` (incl. `"$@"` forwarding); `set -- args…`/`set args…` reassigns them; `${PIPESTATUS[@]}` per-stage pipeline statuses |
| Indexed arrays | ✅ | `arr=(a b c)`, `${arr[N]}`/`${arr[@]}`/`${arr[*]}`, `${#arr[@]}`, `${!arr[@]}`, sparse arrays, `arr[i]=`/`arr[i]+=`, `unset 'arr[i]'`, `local arr=(...)` |
| Associative arrays | ✅ | `declare -A arr`, `arr[key]=val`, `${arr[key]}`/`${arr[@]}`/`${arr[*]}`, `${!arr[@]}` (keys), `${#arr[@]}`, `arr+=([k]=v ...)` merge-by-key, `unset 'arr[key]'`, `local`/`declare -A arr=(...)` |
| Brace expansion | ✅ | `{a,b,c}` (comma-lists, nesting, cross products), `{1..5}`/`{a..z..2}` (numeric/letter ranges, zero-padding); command arguments, `for` word lists, array literals, `local`/`declare` |
| Scripts | ✅ | `rush script.sh args…` runs a file; `rush -c "cmds"` runs a string |
| Ctrl-C / Ctrl-D handling | ✅ | abort line / exit shell |
| Variable expansion (`$VAR`, `~`, `$(...)`) | ✅ | `$VAR`, `${VAR}`, `$?`, `$$`/`$PPID`/`$-`, `${V:-def}`/`:=`/`:+`/`:?`, `${#V}`, `${V#pat}`/`##`/`%`/`%%` (prefix/suffix pattern removal), tilde, command substitution; unquoted results field-split on `$IFS` |
| Arithmetic (`$((...))`, `((expr))`) | ✅ | `+ - * / % **`, bitwise `& \| ^ ~ << >>`, comparisons, `&& \|\| !`, ternary `?:`, assignment (`= += -= *= /= %= <<= >>= &= ^= \|=`), `++`/`--` (pre/post), parentheses, variables; standalone `((expr))` command, `for ((init;cond;update))` |
| Globbing (`*`, `?`, `[…]`) | ✅ | hand-rolled matcher; ranges, `[!…]`, POSIX named classes (`[[:alpha:]]`, `[[:digit:]]`, …), extended globs (`@(a|b)`, `!(pat)`, `+(pat)`, … — on by default, `shopt`-toggleable); `shopt` with `nullglob`/`failglob`/`dotglob`/`globstar` (recursive `**`), multi-component (`src/*.rs`); dotfiles skipped unless pattern starts with `.` |
| Operators (`&&`, `\|\|`, `;`) | ✅ | left-to-right, exit-status short-circuiting |
| Control flow | ✅ | `if`/`while`/`until`/`for`/`select` (`for`/`select x; do` with no `in` iterates `"$@"`), C-style `for ((init;cond;update))`, `case … esac` (incl. `;&`/`;;&` fallthrough), `break`/`continue [n]`; single- or multi-line |
| Functions | ✅ | `name() { … }`, recursion, own `$1`…, `return [n]`, `local [name[=value]]…` for function-scoped variables; brace groups `{ …; }` |
| Subshells | ✅ | `( … )` forks a real child on Unix (genuine isolation, incl. `exit`); state save/restore fallback elsewhere |
| Background & job control (`&`, Ctrl-Z, `fg`/`bg`/`jobs`/`kill %n`/`wait`, `$!`) | ✅ | **Unix only** — process groups, terminal hand-off, signals (`libc`) |

## Build & Run

```sh
cargo build --release
cargo run                       # start the interactive shell
cargo run -- script.sh a b c    # run a script with positional args
cargo run -- -c 'echo $1' x y   # run a command string
cargo test                      # run the unit tests
```

Requires a Rust toolchain with **edition 2024** support.

## Usage

Type commands as you would in any POSIX shell:

```sh
cd /tmp                       # builtin: changes the shell's own cwd
pwd                           # builtin
echo 'single $quoted'         # single quotes are literal
echo "double quoted"          # double quotes group words, allow \" and \\
cat file.txt | grep foo > matches.txt   # pipeline + redirection
echo ~ has $(ls *.rs | wc -l) files      # tilde, command sub, glob
mkdir build && cd build       # && runs only if mkdir succeeds
test -f x || echo "missing"   # || runs only if test fails
a ; b ; c                     # ; runs each in turn
for f in *.rs; do echo $f; done                 # for loop (one line or many)
if cmd; then echo ok; else echo failed; fi      # if/then/else by exit status
sleep 30 &                    # run in the background (Unix); prints [1] <pid>
jobs                          # list background/stopped jobs (Unix)
fg %1                         # bring job 1 to the foreground (Unix)
exit 0                        # leave the shell
```

- **Ctrl-C** abandons the current line and keeps the shell running.
- **Ctrl-D** on an empty line exits.
- **Ctrl-Z** (Unix) stops the foreground job; resume it with `fg` or `bg`.

Job control (`&`, Ctrl-Z, `fg`/`bg`, `jobs`) is **Unix only** — it relies on
POSIX process groups and signals. On other platforms the shell runs foreground
commands only and `&` is rejected.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — full architecture, data-flow
  diagrams, module reference, and roadmap.
- **[docs/CAPABILITY_GAPS.md](docs/CAPABILITY_GAPS.md)** — capability
  assessment against dash/bash/ksh93/zsh/fish: 73 ranked gaps, by
  consequence — the original 40 are closed; a fresh comparison pass
  found 33 more (headlined by a then-missing `[[ ]]`
  extended-test construct and a missing `readonly` builtin).
- **[CHANGELOG.md](CHANGELOG.md)** — what's been built, by area.

## Project Layout

```
src/
  main.rs       entry point: argv dispatch (script / -c / REPL), read→parse→run loop
  completion.rs tab completion: builtins/$PATH in command position, files elsewhere
  lexer.rs      tokenizer: input string → Vec<Token> (words keep their quoting)
  parser.rs     recursive-descent grammar → CommandList (pipelines, &&/||/;, if/while/for/case/functions)
  expand.rs     expansion: $VAR, ${…}, ~, $(...), $((...)), word-split, globs → concrete Pipeline
  arith.rs      integer arithmetic evaluator for $((...))
  func.rs       shell function registry (name() { ... })
  alias.rs      alias table: name -> value, substituted at command-word position
  trap.rs       signal-name traps (EXIT, INT): name -> command
  glob.rs       hand-rolled filename matcher (*, ?, [..]) + directory walk
  vars.rs       shell state outliving a command: $?, variables, positional params, flow control
  exec.rs       runtime: sequence the list, run compounds, spawn processes, wire fds
  job.rs        Unix job control: process groups, terminal, signals, fg/bg/jobs/kill
  builtins.rs   in-process commands: cd, pwd, echo, export, test, … (+ jobs/fg/bg/kill on Unix)

tests/
  exec_behavior.rs   black-box coverage of exec.rs's runtime, against the compiled binary
```
