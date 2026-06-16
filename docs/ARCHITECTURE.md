# rush — Architecture

This document describes how `rush` is structured and how a line of input flows
from your keyboard to a running process and back.

- [1. Overview](#1-overview)
- [2. The processing pipeline](#2-the-processing-pipeline)
- [3. Module reference](#3-module-reference)
- [4. Data model](#4-data-model)
- [5. Execution model](#5-execution-model)
- [6. Worked example](#6-worked-example)
- [7. Design decisions](#7-design-decisions)
- [8. Roadmap](#8-roadmap)

---

## 1. Overview

`rush` is a classic **read → parse → execute** shell. There is no background
thread, event loop, or async runtime: the main thread blocks on a line of
input, transforms it through a series of pure-ish stages, executes it, then
loops.

The codebase is intentionally small (~490 lines) and split along the stages of
that pipeline, so each module has a single, well-defined responsibility.

```mermaid
flowchart LR
    subgraph shell["rush process"]
        direction LR
        main["main.rs<br/>REPL loop"]
        lexer["lexer.rs<br/>tokenize"]
        parser["parser.rs<br/>build AST"]
        exec["exec.rs<br/>run pipeline"]
        builtins["builtins.rs<br/>cd / pwd / exit"]
    end

    user(["User / TTY"]) -->|line of text| main
    main -->|"&str"| parser
    parser -->|"&str"| lexer
    lexer -->|"Vec&lt;Token&gt;"| parser
    parser -->|"Pipeline"| exec
    exec -->|"single cmd?"| builtins
    exec -->|"spawn"| os[["OS processes"]]
    os -->|stdout / stderr| user

    classDef mod fill:#1f2937,stroke:#60a5fa,color:#e5e7eb;
    class main,lexer,parser,exec,builtins mod;
```

> Note: `parser::parse` is the public entry point; it calls `lexer::lex`
> internally. `main` never talks to the lexer directly.

---

## 2. The processing pipeline

Every non-empty line travels through a small chain of transformations. Each
stage has a narrow contract and surfaces errors as a `Result`, which `main`
reports without crashing the shell.

```mermaid
flowchart TD
    A["Raw line<br/><code>echo $HOME | wc -c</code>"]
    B["Tokens<br/>words carry quoting as WordParts"]
    C["RawPipeline (AST)<br/>unexpanded words"]
    F["Pipeline<br/>concrete argv + redirects"]
    D["Spawned processes<br/>+ wired fds"]
    E["Exit / output"]

    A -->|"lexer::lex"| B
    B -->|"parser::parse"| C
    C -->|"expand::expand"| F
    F -->|"exec::run_pipeline"| D
    D --> E

    A -.->|"empty line → skipped in main"| E
```

| Stage | Function | Input | Output | Fails on |
|---|---|---|---|---|
| Lex | `lexer::lex` | `&str` | `Vec<Token>` | unterminated `"`, unterminated `$(` |
| Parse | `parser::parse` | `&str` | `RawPipeline` | dangling `\|`, missing redirect target, empty command |
| Expand | `expand::expand` | `RawPipeline` | `Pipeline` | unterminated `${`/`$(`, sub-command parse error |
| Execute | `exec::run_pipeline` | `&Pipeline` | `()` | spawn failure, missing redirect file |
| Builtin | `builtins::try_run` | `&[String]` | `Option<i32>` | — (errors printed inline) |

Expansion sits deliberately between parse and exec: the parser preserves each
word's quoting (single-quoted text is literal, double-quoted and bare text may
expand), and the expansion stage resolves `~`, `$VAR`/`${VAR}`, and `$(...)`
against the environment and sub-shells before any process is spawned.

---

## 3. Module reference

### `main.rs` — the REPL
Owns the read-eval-print loop and all I/O concerns:
- Builds the prompt from the current working directory (`cwd $ `).
- Loads `~/.rush_history` at startup and saves it on exit.
- Translates `rustyline` signals: **Ctrl-C** (`Interrupted`) abandons the line
  and continues; **Ctrl-D** (`Eof`) on an empty line breaks the loop.
- Delegates parsing and execution, printing any error as `rush: …` to stderr
  without exiting.

### `lexer.rs` — tokenizer
A hand-written, single-pass scanner over a `Peekable<Chars>`. It produces a flat
`Vec<Token>`, stripping quote *characters* but **preserving quote context** as
`WordPart`s so the expansion stage knows what may expand:
- **Single quotes** (`'…'`) and **backslash** escapes become `Literal` parts —
  never expanded.
- **Double quotes** (`"…"`) become `Quoted` parts; backslash escapes `"`, `\`,
  and `$`.
- Bare text becomes `Unquoted` parts (eligible for `~`, `$`, and later glob).
- A `$(...)` substitution is swallowed whole — balanced parens, quotes and all —
  so inner spaces and `|` don't split the word.
- Operators `|`, `<`, `>`, `>>` become distinct tokens; `>>` is detected by
  peeking after `>`.
- Lexer errors: an unterminated double quote or an unterminated `$(`.

### `parser.rs` — grammar
Consumes tokens into a `RawPipeline` (words still unexpanded). The grammar (v0):
```
pipeline := command ( '|' command )*
command  := word+ redirection*
redirect := ('<' | '>' | '>>') word
```
- `Word` tokens append to the current command's `argv` (each a `Vec<WordPart>`).
- `Pipe` finalizes the current command (erroring if it's empty) and starts a new
  one via `std::mem::replace`.
- Redirect operators consume the following word as a filename (`expect_word`),
  erroring if it isn't a word.
- A trailing empty command (e.g. `ls |`) is rejected.

### `expand.rs` — expansion
Lowers a `RawPipeline` into an `exec::Pipeline` of concrete strings:
- **Tilde:** a leading `~` on the first, unquoted part of a word becomes `$HOME`
  (falling back to `$USERPROFILE`); `~user` is left untouched.
- **Variables:** `$VAR` and `${VAR}` read the environment; unset → empty.
- **Command substitution:** `$(...)` re-enters `parse → expand` on the inner
  text and runs it via `exec::capture`, inlining stdout with trailing newlines
  trimmed.
- **Quoting:** `Literal` parts pass through verbatim; `Quoted`/`Unquoted` parts
  are scanned for `$`. A word that is entirely unquoted and expands to empty
  (e.g. `$UNSET`) drops out, mirroring shell field-splitting; a quoted empty
  (`""`) is kept. Word-splitting and globbing of results are *not* done yet, so
  one word yields one argument.

### `exec.rs` — runtime
Turns a `Pipeline` into running processes:
- **Single-command fast path:** if the pipeline is one command, try
  `builtins::try_run` first so `cd`/`exit` affect the shell process.
- Otherwise spawn each stage with `std::process::Command`, threading the
  previous child's stdout into the next child's stdin.
- Redirection rules per stage: an explicit `< file` / `> file` / `>> file`
  **wins** over pipe wiring; otherwise non-final stages get a piped stdout and
  the final stage inherits the terminal.
- `capture` runs the same wiring but pipes the final stage's stdout into a
  string — the engine behind `$(...)`.
- After spawning all stages, it waits on each child and reports a non-zero exit
  status of the last stage (non-fatal — the shell keeps running).

### `builtins.rs` — in-process commands
`try_run` returns `Some(code)` if `argv[0]` is a builtin, else `None`:
- `cd [dir]` — changes the shell's own working directory (no arg → `$HOME`).
- `pwd` — prints the current directory.
- `exit [code]` — terminates the process (diverges; defaults to `0`).

These **must** run in-process: a `cd` executed in a child would change the
child's directory and die with it, leaving the shell where it was.

---

## 4. Data model

The data model is a small, owned AST in two layers: the parser's **raw** form,
where words keep their quoting (`Vec<WordPart>`), and exec's **resolved** form,
where every word is a concrete `String`. The expansion stage maps the first onto
the second. There is no borrowing from the input string, which keeps lifetimes
simple at v0 scale.

```mermaid
classDiagram
    class Token {
        <<enum>>
        Word(Vec~WordPart~)
        Pipe
        Less
        Great
        DGreat
    }
    class WordPart {
        <<enum>>
        Literal(String)
        Unquoted(String)
        Quoted(String)
    }
    class RawPipeline {
        +Vec~RawCommand~ commands
    }
    class RawCommand {
        +Vec~Word~ argv
        +Vec~RawRedirect~ redirects
    }
    class Pipeline {
        +Vec~Command~ commands
    }
    class Command {
        +Vec~String~ argv
        +Vec~Redirect~ redirects
    }
    class Redirect {
        <<enum>>
        Stdin(String)
        Stdout(file, append)
    }

    Token *-- WordPart
    RawPipeline "1" *-- "1..*" RawCommand
    Pipeline "1" *-- "1..*" Command
    Command "1" *-- "0..*" Redirect
    Token ..> RawCommand : parsed into
    RawPipeline ..> Pipeline : expand::expand
```

A `Pipeline` always has at least one `Command` (the parser guarantees this).
Each `Command` carries its full `argv` (program + arguments) and any redirects,
in source order. When multiple redirects of the same kind appear, exec uses the
**last** one (`.rev().find_map(...)`), matching shell semantics like
`cmd > a > b` writing to `b`.

---

## 5. Execution model

The interesting part is how exec wires file descriptors across pipeline stages.
For each stage it decides stdin and stdout independently:

```mermaid
flowchart TD
    start(["for each command i in pipeline"]) --> stdin{"explicit<br/>&lt; file ?"}
    stdin -->|yes| sfile["stdin = open(file)"]
    stdin -->|no| sprev{"previous<br/>pipe ?"}
    sprev -->|yes| spipe["stdin = prev stdout"]
    sprev -->|no| sinherit["stdin = inherit (TTY)"]

    sfile --> out
    spipe --> out
    sinherit --> out

    out{"explicit<br/>&gt; / &gt;&gt; ?"} -->|yes| ofile["stdout = open(file, trunc/append)"]
    out -->|no| olast{"last stage ?"}
    olast -->|no| opipe["stdout = piped()"]
    olast -->|yes| oinherit["stdout = inherit (TTY)"]

    ofile --> spawn
    opipe --> spawn
    oinherit --> spawn

    spawn["command.spawn()"] --> save["keep child;<br/>hand its stdout to next stage"]
    save --> start
    save --> wait(["wait() on all children"])
```

Pipe wiring across two stages looks like this:

```mermaid
sequenceDiagram
    participant E as exec::run_pipeline
    participant P1 as Child: ls
    participant P2 as Child: grep

    E->>P1: spawn (stdout = piped)
    E->>E: take P1.stdout → prev_stdout
    E->>P2: spawn (stdin = prev_stdout, stdout = inherit)
    Note over P1,P2: ls writes → kernel pipe → grep reads
    E->>P1: wait()
    E->>P2: wait()
    P2-->>E: exit status (reported if non-zero)
```

Key properties:
- All stages are spawned **before** any `wait()`, so they run concurrently and
  the kernel pipe buffer provides back-pressure — exactly like a real shell.
- Only the **last** stage's non-zero exit status is surfaced (and only as a
  message; v0 does not yet track `$?`).

---

## 6. Worked example

Input: `cat log.txt | grep ERROR >> errors.txt`

1. **Lex** →
   `[Word("cat"), Word("log.txt"), Pipe, Word("grep"), Word("ERROR"), DGreat, Word("errors.txt")]`
2. **Parse** → `Pipeline { commands: [`
   - `Command { argv: ["cat", "log.txt"], redirects: [] }`,
   - `Command { argv: ["grep", "ERROR"], redirects: [Stdout { file: "errors.txt", append: true }] }`
   `] }`
3. **Execute**
   - Not a single command → skip builtins.
   - Stage 0 `cat log.txt`: stdin inherits, stdout = piped (not last).
   - Stage 1 `grep ERROR`: stdin = stage 0's pipe, stdout = `errors.txt` opened
     with `append=true, truncate=false` (explicit redirect beats pipe-to-next).
   - Wait on both; report if `grep` exits non-zero.

---

## 7. Design decisions

- **Tokens carry no positions.** v0 errors are descriptive strings, not spans.
  Good enough for a REPL; revisit if we add multi-line input.
- **Owned `String`s throughout the AST.** Avoids lifetime plumbing; the input
  line is small and short-lived, so the allocation cost is irrelevant.
- **Builtins only in the single-command fast path.** A builtin mid-pipeline
  (`echo hi | cd x`) is rare and semantically fuzzy; v0 punts and would try to
  exec `cd` as an external program (which fails) — documented, not fixed.
- **Errors never kill the shell** (except `exit`). Parse and exec failures print
  to stderr and the loop continues, matching interactive-shell expectations.
- **No `nix`/`libc` yet.** Everything uses `std`. Real job control (process
  groups, `tcsetpgrp`, signal forwarding) will require dropping to `nix`, which
  is the main architectural change on the horizon.

---

## 8. Roadmap

Ordered roughly by dependency and effort:

```mermaid
flowchart LR
    v0["v0 ✅<br/>REPL · lex · parse<br/>pipes · redirects · builtins"]
    e1["Expansion ✅<br/>$VAR · ~ · $(...)"]
    e2["Globbing<br/>* · ? · [..]"]
    e3["Operators<br/>&amp;&amp; · || · ;"]
    e4["Job control<br/>pgrps · Ctrl-Z · fg/bg · signals"]

    v0 --> e1 --> e2 --> e3 --> e4

    classDef done fill:#064e3b,stroke:#34d399,color:#d1fae5;
    classDef todo fill:#1f2937,stroke:#60a5fa,color:#e5e7eb;
    class v0 done;
    class e1 done;
    class e2,e3,e4 todo;
```

| Milestone | Touches | Notes |
|---|---|---|
| Variable & tilde expansion | ✅ `expand.rs`, between parse and exec | `$VAR`, `${VAR}`, `~`, command substitution `$(...)`; no word-splitting yet |
| Globbing | expansion stage | expand `*`, `?`, `[…]` against the filesystem |
| Control operators | lexer + parser + a new AST node | `&&`, `\|\|`, `;` sequence/short-circuit |
| Job control | exec rewrite on `nix` | process groups, terminal control, `Ctrl-Z`/`fg`/`bg`, signal forwarding — the headline feature for daily use |
