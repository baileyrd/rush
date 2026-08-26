# rush — archived, moved to Rusty Mill

**This repository is archived.** `rush` now lives in the
[Rusty Mill monorepo](https://github.com/Rusty-Mill/rusty_mill) as
[`crates/rush`](https://github.com/Rusty-Mill/rusty_mill/tree/main/crates/rush),
with full commit history preserved via `git subtree`. Please file issues
and pull requests against the new location — this repo is read-only.

If you depend on this crate via a `git` dependency, repoint it at
`https://github.com/Rusty-Mill/rusty_mill.git` (the crate name is
unchanged, so Cargo's git dependency resolution still finds it).

---

*The README below is preserved as it was at the time of the merge, for
historical reference.*

# rush

[![CI](https://github.com/baileyrd/rush/actions/workflows/ci.yml/badge.svg)](https://github.com/baileyrd/rush/actions/workflows/ci.yml)

A small, bash-compatible shell written in Rust — built to grow into a daily-use tool.

**Status: experimental.** rush covers most of the core POSIX shell language and
has a growing test suite, but it hasn't been hardened as a daily-driver shell
yet — treat it as a project to explore and contribute to, not (yet) a drop-in
`chsh` replacement. Full job control (`fg`/`bg`, Ctrl-Z, process groups) is
**Unix only**; on Windows, `cmd &`/`jobs`/`wait`/`kill %n`/`disown`/`$!`
work for external commands, single-stage or piped together (via Windows
Job Objects, see `docs/WINDOWS_JOB_CONTROL.md`), but a builtin/function as
a background pipeline stage (no Windows `fork()`) and `fg`/`bg`/Ctrl-Z
don't.

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

## Build & Run

```sh
cargo build --release
cargo run                       # start the interactive shell
cargo run -- script.sh a b c    # run a script with positional args
cargo run -- -c 'echo $1' x y   # run a command string
cargo test                      # run the unit tests
```

Requires a Rust toolchain with **edition 2024** support.

## Documentation

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — full architecture, data-flow
  diagrams, module reference, and roadmap.
- **[docs/CAPABILITY_GAPS.md](docs/CAPABILITY_GAPS.md)** — capability
  assessment against dash/bash/ksh93/zsh/fish.
- **[CHANGELOG.md](CHANGELOG.md)** — what's been built, by area.

See the crate's own docs under `crates/rush/docs/` in the new monorepo
location for the full feature matrix, usage examples, and project layout
that used to live here.

## License

MIT.
