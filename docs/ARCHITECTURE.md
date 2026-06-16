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

The codebase is intentionally small and split along the stages of that
pipeline, so each module has a single, well-defined responsibility.

```mermaid
flowchart LR
    subgraph shell["rush process"]
        direction LR
        main["main.rs<br/>REPL loop"]
        lexer["lexer.rs<br/>tokenize"]
        parser["parser.rs<br/>build AST"]
        expand["expand.rs + glob.rs<br/>expand words"]
        exec["exec.rs<br/>run jobs"]
        job["job.rs<br/>job control (Unix)"]
        builtins["builtins.rs<br/>cd / pwd / exit / fg / bg"]
    end

    user(["User / TTY"]) -->|line of text| main
    main -->|"&str"| parser
    parser -->|"&str"| lexer
    lexer -->|"Vec&lt;Token&gt;"| parser
    parser -->|"CommandList"| exec
    exec -->|"RawPipeline"| expand
    expand -->|"Pipeline"| exec
    exec -->|"single cmd?"| builtins
    exec -->|"fg / bg (Unix)"| job
    exec -->|"spawn"| os[["OS processes"]]
    os -->|stdout / stderr| user

    classDef mod fill:#1f2937,stroke:#60a5fa,color:#e5e7eb;
    class main,lexer,parser,expand,exec,job,builtins mod;
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
    A["Raw line<br/><code>mkdir d && ls *.rs</code>"]
    B["Tokens<br/>words carry quoting as WordParts"]
    C["CommandList (AST)<br/>pipelines joined by &amp;&amp; || ;"]
    F["Pipeline<br/>concrete argv + redirects"]
    D["Spawned processes<br/>+ wired fds"]
    E["Exit / output"]

    A -->|"lexer::lex"| B
    B -->|"parser::parse"| C
    C -->|"exec::run_list → expand::expand<br/>(per pipeline, left to right)"| F
    F -->|"job::run_foreground / run_background (Unix)<br/>exec::run (other)"| D
    D --> E

    A -.->|"empty line → skipped in main"| E
```

| Stage | Function | Input | Output | Fails on |
|---|---|---|---|---|
| Lex | `lexer::lex` | `&str` | `Vec<Token>` | unterminated `"`, unterminated `$(` |
| Parse | `parser::parse` | `&str` | `CommandList` | dangling `\|`, missing redirect target, empty command |
| Run list | `exec::run_list` | `&CommandList` | `i32` (status) | propagated from expand/exec |
| Expand | `expand::expand` | `&RawPipeline` | `Pipeline` | unterminated `${`/`$(`, sub-command parse error |
| Execute (fg) | `job::run_foreground` (Unix) / `exec::run` | `&Pipeline` | `i32` / `(i32, String)` | spawn failure, missing redirect file |
| Builtin | `builtins::try_run` | `&[String]` | `Option<i32>` | — (errors printed inline) |

Expansion sits deliberately between parse and exec, and runs **lazily, one
pipeline at a time** as `run_list` walks the list left to right — so `cd d &&
ls *` globs in the new directory. The parser preserves each word's quoting
(single-quoted text is literal, double-quoted and bare text may expand), and the
expansion stage resolves `~`, `$VAR`/`${VAR}`, `$(...)`, and filename globs
(`*`, `?`, `[…]`) against the environment, sub-shells, and the filesystem before
a process is spawned. Globbing can turn one word into several arguments, so the
stage is a flat-map, not a one-to-one map.

Control operators form two levels of grouping. `&&` and `||` bind a chain of
pipelines into one **job** (equal precedence, left-associative); `run_list`
decides whether to run each pipeline from the previous one's exit status (`&&`
needs `0`, `||` needs non-zero). Then `;` and `&` separate jobs, with `&`
marking the preceding job to run in the background. On Unix, foreground and
background pipelines go through `job` (process groups + terminal control); off
Unix there is no job control and `&` is an error.

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
Consumes tokens into a `CommandList` (a `Vec<Job>`), words still unexpanded.
The grammar (v0):
```
list     := job ( (';' | '&') job )* (';' | '&')?
job      := pipeline ( ('&&' | '||') pipeline )*
pipeline := command ( '|' command )*
command  := word+ redirection*
redirect := ('<' | '>' | '>>') word
```
- `parse_andor` builds one job's `&&`/`||` chain; `parse_pipeline` builds one
  pipeline, each stopping (without consuming) at the next operator or end.
- `Word` tokens append to the current command's `argv` (each a `Vec<WordPart>`).
- `Pipe` finalizes the current command (erroring if it's empty) and starts a new
  one via `std::mem::replace`.
- Redirect operators consume the following word as a filename (`expect_word`),
  erroring if it isn't a word.
- A trailing `;`/`&` is allowed; a trailing empty command (`ls |`) and an empty
  job between operators (`a ;; b`, `&& a`) are rejected.

### `expand.rs` — expansion
Lowers a `RawPipeline` into an `exec::Pipeline` of concrete strings:
- **Tilde:** a leading `~` on the first, unquoted part of a word becomes `$HOME`
  (falling back to `$USERPROFILE`); `~user` is left untouched.
- **Variables:** `$VAR` and `${VAR}` resolve to a shell variable (`vars`) if set,
  else the environment, else empty. `$?` expands to the last pipeline's exit
  status. Leading `NAME=value` words on a command are split off as assignments
  (`expand_command`): with no program word they set shell variables, otherwise
  they seed that command's environment (alongside exported variables).
- **Command substitution:** `$(...)` re-enters `parse → expand` on the inner
  text and runs it via `exec::capture`, inlining stdout with trailing newlines
  trimmed.
- **Word-splitting:** whitespace inside an *unquoted* expansion splits the word
  into fields (`x="a b"; echo $x` → two args). A `Splitter` assembles the word's
  parts into fields: quoted/literal text is added unsplit, unquoted-expansion
  text splits on whitespace runs. Because the lexer already split on literal
  whitespace, any whitespace left in an unquoted part must have come from an
  expansion — so this needs no IFS bookkeeping.
- **Globbing:** each field carries a glob *pattern* alongside its plain text,
  escaping metacharacters from quoted/literal parts so only unquoted `*?[` stay
  active. A field that matches files (via `glob::glob`) is replaced by the
  sorted matches; otherwise its literal text is kept (POSIX no-match).
- **Emptiness:** a field that is entirely unquoted and empty (e.g. `$UNSET`)
  drops out, mirroring shell field-splitting; a quoted empty (`""`) is kept.

### `glob.rs` — filename matching
A from-scratch globber, no external crate:
- `match_component` matches one path component with `*` (within a component),
  `?`, `[…]` (ranges and `[!…]`/`[^…]` negation); a backslash escapes the next
  character so quoted metacharacters are literal.
- `glob` walks the filesystem component-by-component, so `src/*.rs` and
  `*/*.rs` descend directories. A leading `.` in a filename matches only when
  the pattern component begins with a literal `.`, so `*` skips dotfiles.

### `exec.rs` — runtime
Sequences a `CommandList` and turns each pipeline into running processes:
- `run_list` runs each job in turn. A foreground job runs its `&&`/`||` chain
  via `run_andor`, expanding each `RawPipeline` *just before* it runs and using
  `should_run(connector, prev_status)` to short-circuit. A background job (one
  pipeline) is handed to `run_background`.
- `capture_list` runs every job in the foreground with a plain spawn-and-wait
  and concatenates stdout — the engine behind `$(...)` (the `&` marker is
  ignored inside a substitution).
- **Single-command fast path:** if a pipeline is one command, try
  `builtins::try_run` first so `cd`/`exit`/`jobs` affect the shell process
  (skipped when capturing, so substitutions see external commands).
- `build_stage` constructs one stage's `std::process::Command` and stdio.
  Redirection rules: an explicit `< file` / `> file` / `>> file` **wins** over
  pipe wiring; otherwise non-final stages (and any stage when capturing) get a
  piped stdout. It is shared by the plain runner and the Unix job runner.
- On Unix, foreground/background spawning, terminal control, and waiting live in
  `job` (below). Off Unix, `run` spawns each stage, threads stdout→stdin, waits,
  and reports the last stage's exit code as the pipeline status.

### `job.rs` — Unix job control *(compiled only on Unix)*
Implements the parts that need POSIX process groups and signals, via the `libc`
crate, following the classic glibc job-control structure:
- `init` (called once at startup, only when stdin is a tty) ignores the
  job-control signals (`SIGINT`, `SIGTSTP`, …) in the shell, puts the shell in
  its own process group, and grabs the terminal.
- `spawn_pipeline` launches every stage into one new process group; each child
  resets those signals to default and `setpgid`s before `exec` (via
  `CommandExt::pre_exec`), so Ctrl-C/Ctrl-Z reach the job, not the shell.
- `run_foreground` hands the job the terminal (`tcsetpgrp`), waits with
  `WUNTRACED` (so a Ctrl-Z stop is detected and recorded), then reclaims the
  terminal. `run_background` records the job and prints `[id] pgid`.
- A thread-local job table backs `jobs`/`fg`/`bg` and the `reap_background`
  sweep run before each prompt (a non-blocking `waitpid` that reports finished,
  stopped, and continued jobs).

### `builtins.rs` — in-process commands
`try_run` returns `Some(code)` if `argv[0]` is a builtin, else `None`:
- `cd [dir]` — changes the shell's own working directory (no arg → `$HOME`).
- `pwd` — prints the current directory.
- `echo [-n] [args…]` — joins args with spaces (no `-e` escapes).
- `export NAME[=value]` — marks a shell variable exported (see `vars`).
- `exit [code]` — terminates the process (diverges; defaults to `0`).
- On Unix, `jobs`/`fg`/`bg` are dispatched to `job::builtin`.

These **must** run in-process: a `cd` executed in a child would change the
child's directory and die with it; `fg`/`bg` must manipulate the shell's own
job table and terminal.

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
    class CommandList {
        +Vec~Job~ jobs
    }
    class Job {
        +AndOrList list
        +bool background
    }
    class AndOrList {
        +RawPipeline first
        +Vec~(Connector, RawPipeline)~ rest
    }
    class Connector {
        <<enum>>
        And
        Or
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
        +Vec~(String, String)~ assignments
    }
    class Redirect {
        <<enum>>
        Stdin(String)
        Stdout(file, append)
    }

    Token *-- WordPart
    CommandList "1" *-- "1..*" Job
    Job "1" *-- "1" AndOrList
    AndOrList "1" *-- "1..*" RawPipeline
    AndOrList ..> Connector : joins with
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
    participant E as exec::run
    participant P1 as Child: ls
    participant P2 as Child: grep

    E->>P1: spawn (stdout = piped)
    E->>E: take P1.stdout → prev_stdout
    E->>P2: spawn (stdin = prev_stdout, stdout = inherit)
    Note over P1,P2: ls writes → kernel pipe → grep reads
    E->>P1: wait()
    E->>P2: wait()
    P2-->>E: exit status (becomes the pipeline's status)
```

Key properties:
- All stages are spawned **before** any `wait()`, so they run concurrently and
  the kernel pipe buffer provides back-pressure — exactly like a real shell.
- The **last** stage's exit code is the pipeline's status, which feeds the
  `&&`/`||` decision in `run_list`. (v0 does not yet expose it as `$?`.)

---

## 6. Worked example

Input: `cat log.txt | grep ERROR >> errors.txt`

1. **Lex** →
   `[Word("cat"), Word("log.txt"), Pipe, Word("grep"), Word("ERROR"), DGreat, Word("errors.txt")]`
2. **Parse** → `CommandList { first: Pipeline { commands: [`
   - `RawCommand { argv: [["cat"], ["log.txt"]], redirects: [] }`,
   - `RawCommand { argv: [["grep"], ["ERROR"]], redirects: [Stdout { "errors.txt", append: true }] }`
   `] }, rest: [] }` (one pipeline, no operators)
3. **Run list / Expand** → the single pipeline is expanded (here a no-op — no
   `$`, `~`, or globs) into the concrete `Pipeline` of `String` argv.
4. **Execute**
   - Not a single command → skip builtins.
   - Stage 0 `cat log.txt`: stdin inherits, stdout = piped (not last).
   - Stage 1 `grep ERROR`: stdin = stage 0's pipe, stdout = `errors.txt` opened
     with `append=true, truncate=false` (explicit redirect beats pipe-to-next).
   - Wait on both; `grep`'s exit code becomes the pipeline's (and the list's)
     status.

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
- **`libc` only where it's unavoidable.** Everything except job control uses
  `std`. Job control (process groups, `tcsetpgrp`, `waitpid(WUNTRACED)`, signal
  handling) needs raw POSIX, so `job.rs` uses `libc` directly and is gated to
  `#[cfg(unix)]`; `libc` is a `[target.'cfg(unix)'.dependencies]` entry, so
  Windows builds never pull it. Off Unix the shell degrades to foreground-only.
- **Job control follows the glibc reference.** The shell ignores the
  job-control signals and owns the terminal; each child resets them and joins
  the job's process group before `exec`; the terminal is handed to the
  foreground job and reclaimed when it stops or exits.

---

## 8. Roadmap

Ordered roughly by dependency and effort:

```mermaid
flowchart LR
    v0["v0 ✅<br/>REPL · lex · parse<br/>pipes · redirects · builtins"]
    e1["Expansion ✅<br/>$VAR · ~ · $(...)"]
    e2["Globbing ✅<br/>* · ? · [..]"]
    e3["Operators ✅<br/>&amp;&amp; · || · ;"]
    e4["Job control ✅<br/>pgrps · Ctrl-Z · fg/bg · signals"]

    v0 --> e1 --> e2 --> e3 --> e4

    classDef done fill:#064e3b,stroke:#34d399,color:#d1fae5;
    classDef todo fill:#1f2937,stroke:#60a5fa,color:#e5e7eb;
    class v0 done;
    class e1 done;
    class e2 done;
    class e3 done;
    class e4 done;
```

| Milestone | Touches | Notes |
|---|---|---|
| Variable & tilde expansion | ✅ `expand.rs`, between parse and exec | `$VAR`, `${VAR}`, `~`, command substitution `$(...)`; no word-splitting yet |
| Globbing | ✅ `glob.rs`, driven from the expansion stage | `*`, `?`, `[…]` with ranges/negation, multi-component, dotfile rule |
| Control operators | ✅ lexer + parser `CommandList` + `exec::run_list` | `&&`, `\|\|`, `;` sequence/short-circuit by exit status |
| Job control | ✅ `job.rs` (`#[cfg(unix)]`, `libc`) | `&`, process groups, terminal control, `Ctrl-Z`/`fg`/`bg`/`jobs`, signals |

All four roadmap milestones are now in. Beyond the roadmap, natural next steps
are word-splitting of expansions, `$?`/`$#`/`$@` and variable assignment,
backgrounding `&&`/`||` lists via a subshell, and more builtins (`export`,
`echo`, `kill %n`).
