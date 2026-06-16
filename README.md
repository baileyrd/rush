# rush

A small, bash-compatible shell written in Rust — built to grow into a daily-use tool.

`rush` reads a command line, lexes and parses it, then executes the resulting
pipeline. The current version (`v0`) supports interactive editing with
persistent history, pipelines, file redirection, and the handful of builtins
that must run inside the shell process.

```
/home/baileyrd/projects/rust_bash $ ls | grep rs | wc -l
5
/home/baileyrd/projects/rust_bash $ echo "hello world" > out.txt
/home/baileyrd/projects/rust_bash $ sort < out.txt
hello world
```

## Features

| Feature | Status | Notes |
|---|---|---|
| REPL with line editing | ✅ | via [`rustyline`](https://crates.io/crates/rustyline) |
| Persistent history | ✅ | stored in `~/.rush_history` |
| Quoting | ✅ | single quotes, double quotes, backslash escapes |
| Pipelines (`\|`) | ✅ | N stages, stdout→stdin wiring |
| Redirection (`>`, `>>`, `<`) | ✅ | truncate, append, input |
| Builtins | ✅ | `cd`, `pwd`, `exit` |
| Ctrl-C / Ctrl-D handling | ✅ | abort line / exit shell |
| Variable expansion (`$VAR`, `~`, `$(...)`) | ⬜ | planned |
| Globbing (`*`, `?`) | ⬜ | planned |
| Operators (`&&`, `\|\|`, `;`) | ⬜ | planned |
| Job control (Ctrl-Z, `fg`/`bg`, signals) | ⬜ | planned (the big one) |

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
exit 0                        # leave the shell
```

- **Ctrl-C** abandons the current line and keeps the shell running.
- **Ctrl-D** on an empty line exits.

## Documentation

See **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** for the full architecture,
data-flow diagrams, module reference, and roadmap.

## Project Layout

```
src/
  main.rs       REPL: read → parse → execute loop, history, prompt
  lexer.rs      tokenizer: input string → Vec<Token>
  parser.rs     grammar: Vec<Token> → Pipeline of Commands
  exec.rs       runtime: spawn processes, wire pipes & redirects
  builtins.rs   in-process commands: cd, pwd, exit
```
