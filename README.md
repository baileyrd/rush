# rush

A small, bash-compatible shell written in Rust — built to grow into a daily-use tool.

`rush` reads a command line, lexes and parses it, expands it, then executes the
resulting pipeline. The current version (`v0`) supports interactive editing with
persistent history, pipelines, file redirection, the handful of builtins that
must run inside the shell process, expansion of variables, `~`, command
substitution and filename globs, the control operators `&&`/`||`/`;`, and — on
Unix — background jobs with real job control.

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
| Persistent history | ✅ | stored in `~/.rush_history` |
| Quoting | ✅ | single quotes, double quotes, backslash escapes |
| Pipelines (`\|`) | ✅ | N stages, stdout→stdin wiring |
| Redirection (`>`, `>>`, `<`) | ✅ | truncate, append, input |
| Builtins | ✅ | `cd`, `pwd`, `exit` (+ `jobs`/`fg`/`bg` on Unix) |
| Ctrl-C / Ctrl-D handling | ✅ | abort line / exit shell |
| Variable expansion (`$VAR`, `~`, `$(...)`) | ✅ | `$VAR`, `${VAR}`, tilde, command substitution (no word-splitting yet) |
| Globbing (`*`, `?`, `[…]`) | ✅ | hand-rolled matcher; ranges, `[!…]`, multi-component (`src/*.rs`); dotfiles skipped unless pattern starts with `.` |
| Operators (`&&`, `\|\|`, `;`) | ✅ | left-to-right, exit-status short-circuiting |
| Background & job control (`&`, Ctrl-Z, `fg`/`bg`, `jobs`) | ✅ | **Unix only** — process groups, terminal hand-off, signals (`libc`) |

## Build & Run

```sh
cargo build --release
cargo run            # start the interactive shell
cargo test           # run the lexer/parser unit tests
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

See **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** for the full architecture,
data-flow diagrams, module reference, and roadmap.

## Project Layout

```
src/
  main.rs       REPL: read → parse → run loop, history, prompt
  lexer.rs      tokenizer: input string → Vec<Token> (words keep their quoting)
  parser.rs     grammar: Vec<Token> → CommandList (pipelines + &&/||/; )
  expand.rs     expansion: $VAR, ~, $(...), globs → concrete Pipeline
  glob.rs       hand-rolled filename matcher (*, ?, [..]) + directory walk
  exec.rs       runtime: sequence the list, spawn processes, wire pipes & redirects
  job.rs        Unix job control: process groups, terminal, signals, fg/bg/jobs
  builtins.rs   in-process commands: cd, pwd, exit (+ jobs/fg/bg on Unix)
```
