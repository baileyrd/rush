//! rush — a small Rust shell.
//!
//! v0 scope: a REPL with persistent history, pipelines (`|`), redirections
//! (`>`, `>>`, `<`), and the builtins that must run in-process (`cd`, `exit`,
//! `pwd`). Quoting is handled by a small hand-written lexer so that
//! `echo "hello world"` is one argument. An expansion stage resolves `$VAR`,
//! `~`, `$(...)`, and filename globs (`*`, `?`, `[…]`) before a command runs,
//! and control operators (`&&`, `||`, `;`, `&`) sequence whole jobs. On Unix,
//! background and stopped jobs are managed with real job control (`fg`/`bg`/
//! `jobs`, Ctrl-Z); other platforms run foreground-only.

mod alias;
mod arith;
mod builtins;
mod completion;
mod exec;
mod expand;
mod func;
mod glob;
mod history_expand;
#[cfg(unix)]
mod job;
mod lexer;
mod parser;
#[cfg(unix)]
mod sys;
mod trap;
mod vars;

use std::path::PathBuf;

use rusty_lines::{Editor, Hooks, ReadResult};

/// rush's side of the editor's `Hooks` seam: completion, hints,
/// highlighting, and abbreviations come from `completion.rs`; the
/// vi-mode flag and `$VISUAL`/`$EDITOR` from the shell's own variable
/// table (so `set -o vi` and an in-shell `EDITOR=…` both take effect
/// live); an interrupted read fires pending signal traps.
struct ShellHooks;

impl Hooks for ShellHooks {
    fn complete(&self, line: &str, pos: usize) -> (usize, Vec<rusty_lines::Candidate>) {
        completion::complete(line, pos)
    }
    fn hint(&self, line: &str, history: &[String]) -> Option<String> {
        completion::hint(line, history)
    }
    fn highlight(&self, line: &str) -> String {
        completion::highlight_line(line)
    }
    fn expand_abbreviation(&self, line: &str, cursor: usize) -> Option<(usize, String)> {
        completion::abbr_expansion(line, cursor)
    }
    fn vi_mode(&self) -> bool {
        vars::edit_mode_vi()
    }
    fn external_editor(&self) -> Option<String> {
        vars::get("VISUAL")
            .filter(|e| !e.is_empty())
            .or_else(|| vars::get("EDITOR").filter(|e| !e.is_empty()))
    }
    fn on_interrupted_read(&self) {
        // A deferred TERM/HUP landed mid-read; let trap machinery see it
        // at this safe point.
        #[cfg(unix)]
        trap::check_pending();
    }
}

fn history_path() -> Option<PathBuf> {
    let mut p = PathBuf::from(std::env::var_os("HOME")?);
    p.push(".rush_history");
    Some(p)
}

fn rc_path() -> Option<PathBuf> {
    let mut p = PathBuf::from(std::env::var_os("HOME")?);
    p.push(".rushrc");
    Some(p)
}

/// The interactive prompt: `$PS1` with its escapes expanded, or the original
/// hardcoded default if `PS1` isn't set. `vars::get` alone is a complete
/// answer — every inherited environment variable (including a real `PS1`)
/// is seeded into it at startup (C36), and falling back to `std::env::var`
/// on top would resurrect its original value even after `unset` (C40).
fn prompt() -> String {
    match crate::vars::get("PS1") {
        Some(ps1) => expand_ps1(&ps1),
        None => default_prompt(),
    }
}

fn default_prompt() -> String {
    format!("{} $ ", cwd_string())
}

/// A small, rush-specific escape set (not the full bash set): `\w`/`\W` (cwd,
/// cwd basename), `\u`/`\h` (user, host), `\$` (`#` for root, else `$`), `\?`
/// (last exit status — bash has no equivalent; real PS1s get this via a
/// command substitution instead), `\n`, `\\`. An unrecognized escape is kept
/// literal (backslash and all) rather than silently dropped.
fn expand_ps1(ps1: &str) -> String {
    let mut out = String::new();
    let mut chars = ps1.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('w') => out.push_str(&cwd_string()),
            Some('W') => out.push_str(&cwd_basename()),
            Some('u') => out.push_str(&username()),
            Some('h') => out.push_str(&hostname()),
            Some('$') => out.push(prompt_char()),
            Some('?') => out.push_str(&crate::vars::last_status().to_string()),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn cwd_string() -> String {
    std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".into())
}

fn cwd_basename() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "/".into())
}

fn username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "host".into())
}

#[cfg(unix)]
fn prompt_char() -> char {
    if unsafe { crate::sys::getuid() } == 0 { '#' } else { '$' }
}

#[cfg(not(unix))]
fn prompt_char() -> char {
    '$'
}

fn main() -> std::io::Result<()> {
    // Rust's runtime sets `SIGPIPE` to `SIG_IGN` at startup, so a builtin's
    // `print!`/`println!` surfaces a closed pipe as an `Err` that those
    // macros then *panic* on — a real, general bug found while verifying
    // process substitution (C31), but not specific to it at all: any
    // builtin writing into a pipe whose reader has already gone (`rush -c
    // 'while true; do echo x; done' | head` is the plainest reproduction)
    // panics instead of the process just quietly dying the way a normal
    // Unix command does. Reset it to the default disposition, matching
    // real bash's own C-program behavior (verified directly: bash's own
    // builtin `echo` exhibits the exact same race against a `>(...)`
    // whose reader exits without reading — bash just dies silently there,
    // rather than panicking).
    #[cfg(unix)]
    unsafe {
        crate::sys::signal(crate::sys::SIGPIPE, crate::sys::SIG_DFL);
    }
    // `TERM`/`HUP` traps (C21) need to work in every mode, not just
    // interactively — the target use case (a container's PID 1 catching
    // `TERM` to shut down gracefully) has no terminal at all.
    #[cfg(unix)]
    trap::install_signal_handlers();

    // Seed the shell's own variable table with the inherited process
    // environment, marked exported — matching real bash: an env-inherited
    // variable stays exported through a later *plain* reassignment (no
    // fresh `export` keyword needed), since `vars::set`'s existing-entry
    // path preserves whatever `exported` flag is already there. Without
    // this, a bare `PATH=$PATH:dir` (no `export`) would insert a *new*,
    // non-exported `PATH` entry — internal PATH lookups (`vars::get`) would
    // see the update, but the value threaded into any child process
    // spawned afterward (`exec::build_stage`'s `vars::exported()`) would
    // not, silently reverting to the original PATH for `dir`'s contents.
    // Found and fixed alongside C36, which is this same root cause's
    // narrower, easier-to-hit symptom (`command -v`/`type`/`hash` calling
    // `std::env::var_os("PATH")` directly instead of the shell's own).
    for (name, value) in std::env::vars() {
        vars::set_exported(&name, &value);
    }

    // `$PPID` (C41): the invoking process's pid, seeded once at startup as
    // an ordinary (non-exported, same as bash) shell variable — after the
    // environment loop above, so a stale inherited `PPID` from a parent
    // shell's environment can't shadow the real value.
    #[cfg(unix)]
    vars::set("PPID", &unsafe { crate::sys::getppid() }.to_string());

    // The standard identity/platform variables (C106), seeded like PPID
    // (after the environment loop, so stale inherited copies can't shadow
    // the real values). `UID`/`EUID` are readonly, same as bash.
    #[cfg(unix)]
    {
        let uid = unsafe { crate::sys::getuid() }.to_string();
        vars::set("UID", &uid);
        vars::set("EUID", &uid); // sys carries no geteuid; same value
        for name in ["UID", "EUID"] {
            vars::set_attrs(name, vars::Attrs { readonly: true, ..Default::default() });
        }
    }
    if vars::get("HOSTNAME").is_none()
        && let Some(host) = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .or_else(|_| std::fs::read_to_string("/etc/hostname"))
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
    {
        vars::set("HOSTNAME", &host);
    }
    vars::set("HOSTTYPE", std::env::consts::ARCH);
    vars::set(
        "OSTYPE",
        if cfg!(target_os = "linux") { "linux-gnu" } else { std::env::consts::OS },
    );
    vars::set(
        "MACHTYPE",
        &if cfg!(target_os = "linux") {
            format!("{}-pc-linux-gnu", std::env::consts::ARCH)
        } else {
            format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
        },
    );
    vars::set("RUSH_VERSION", env!("CARGO_PKG_VERSION"));
    // `$SHLVL` increments per nested shell (C106) and stays exported.
    let shlvl = vars::get("SHLVL").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
    vars::set_exported("SHLVL", &shlvl.to_string());

    let args: Vec<String> = std::env::args().collect();

    // `rush -n …` (C51): the standard `sh -n script.sh` linting idiom —
    // parse everything, report syntax errors (status 2), execute nothing
    // (status 0 when the syntax is clean). Composes with both `-c` and a
    // script file by simply pre-setting the same noexec flag `set -n`
    // uses; `run_source`'s parse still happens, `exec::run_andor` skips
    // every job.
    let args: Vec<String> = if args.get(1).map(String::as_str) == Some("-n") {
        vars::set_noexec(true);
        let mut rest = args.clone();
        rest.remove(1);
        rest
    } else {
        args
    };

    // Non-interactive modes: `rush -c "cmd" [name args…]` and `rush FILE [args…]`.
    match args.get(1).map(String::as_str) {
        Some("-c") => {
            let cmd = args.get(2).cloned().unwrap_or_default();
            let name = args.get(3).cloned().unwrap_or_else(|| "rush".to_string());
            vars::set_args(name, args.get(4..).unwrap_or(&[]).to_vec());
            source_bash_env();
            trap::exit_shell(run_source(&cmd));
        }
        Some(file) => {
            vars::set_args(file.to_string(), args.get(2..).unwrap_or(&[]).to_vec());
            vars::push_source(file); // `${BASH_SOURCE[0]}` (C67); empty under `-c`, same as bash
            source_bash_env();
            match std::fs::read_to_string(file) {
                Ok(src) => trap::exit_shell(run_source(&src)),
                Err(e) => {
                    eprintln!("rush: {file}: {e}");
                    trap::exit_shell(1);
                }
            }
        }
        None => interactive(),
    }
}

/// `$BASH_ENV` (C105): a non-interactive shell sources the named file
/// before running anything — CI/wrapper-injected setup. Errors are
/// non-fatal, same as bash's own behavior for an unreadable file.
fn source_bash_env() {
    if let Some(file) = vars::get("BASH_ENV").filter(|f| !f.is_empty()) {
        let _ = exec::source_file(&file, &[]);
    }
}

/// Parse and run a whole script (or `-c` string), returning an exit status.
fn run_source(src: &str) -> i32 {
    match parser::parse(src) {
        Ok(list) => match exec::run_list(&list) {
            Ok(status) => status,
            Err(e) => {
                eprintln!("rush: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("rush: {e}");
            2
        }
    }
}

fn interactive() -> std::io::Result<()> {
    vars::set_interactive(true); // `$-` includes `i` in the REPL (C41)
    // The line editor lives in the rusty_lines crate (extracted from
    // this repo's former src/editor.rs); rush plugs in completion,
    // hints, highlighting, abbreviations, the vi-mode flag, and trap
    // handling through `ShellHooks`. The right prompt ($RPS1, C71) is
    // read_line's second argument.
    let mut rl = Editor::new();
    let hist = history_path();
    if let Some(ref h) = hist {
        let _ = rl.load_history(h);
    }

    // Claim the terminal and set up signal handling for job control.
    #[cfg(unix)]
    job::init();

    // Source ~/.rushrc, if any — same as a script, errors go to stderr but
    // don't stop the shell from starting. Missing/unreadable is silently fine
    // (like a fresh install with no rc file yet).
    if let Some(rc) = rc_path()
        && let Ok(src) = std::fs::read_to_string(&rc)
    {
        run_source(&src);
    }

    // Accumulates lines until a complete command is parsed — so an `if`/`while`
    // can span several lines, with a `> ` continuation prompt.
    let mut buffer = String::new();

    loop {
        // Report any background jobs that finished or stopped since last prompt.
        #[cfg(unix)]
        job::reap_background();
        // Fire (or default-terminate on) any TERM/HUP received since the last
        // prompt — same idea as `reap_background`, for signals instead of jobs.
        #[cfg(unix)]
        trap::check_pending();

        let prompt = if buffer.is_empty() { prompt() } else { "> ".to_string() };
        // The right prompt ($RPS1, C71): ordinary `$`-expansion each
        // time, like $PS3 — empty (hidden) when unset.
        let rprompt = vars::get("RPS1")
            .and_then(|raw| expand::expand_dollars(&raw).ok())
            .unwrap_or_default();
        match rl.read_line(&prompt, &rprompt, &ShellHooks)? {
            ReadResult::Line(line) => {
                if buffer.is_empty() && line.trim().is_empty() {
                    continue;
                }
                let line = match history_expand::expand(&line, rl.history()) {
                    Ok(None) => line,
                    Ok(Some(expanded)) => {
                        println!("{expanded}");
                        expanded
                    }
                    Err(e) => {
                        eprintln!("rush: {e}");
                        continue;
                    }
                };
                rl.add_history_entry(&line);
                if !buffer.is_empty() {
                    buffer.push('\n');
                }
                buffer.push_str(&line);

                match parser::parse(&buffer) {
                    Ok(list) => {
                        if let Err(e) = exec::run_list(&list) {
                            eprintln!("rush: {e}");
                        }
                        buffer.clear();
                    }
                    // A valid prefix: keep reading more lines.
                    Err(parser::ParseError::Incomplete) => {}
                    Err(parser::ParseError::Syntax(e)) => {
                        eprintln!("rush: {e}");
                        buffer.clear();
                    }
                }
            }
            // Ctrl-C at an idle prompt (not a running foreground job — that's
            // a child process under job control, and never reaches here).
            ReadResult::Interrupted => {
                trap::fire("INT");
                buffer.clear();
                continue;
            }
            // Ctrl-D on an empty line: exit.
            ReadResult::Eof => break,
        }
    }

    if let Some(ref h) = hist {
        let _ = rl.save_history(h);
    }
    trap::fire("EXIT");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_text_passes_through() {
        assert_eq!(expand_ps1("plain > "), "plain > ");
    }

    #[test]
    fn newline_and_backslash_escapes() {
        assert_eq!(expand_ps1(r"a\nb"), "a\nb");
        assert_eq!(expand_ps1(r"a\\b"), r"a\b");
    }

    #[test]
    fn unknown_escape_kept_literal() {
        assert_eq!(expand_ps1(r"\z"), r"\z");
    }

    #[test]
    fn trailing_backslash_kept_literal() {
        assert_eq!(expand_ps1(r"end\"), r"end\");
    }

    #[test]
    fn exit_status_escape() {
        vars::set_last_status(42);
        assert_eq!(expand_ps1(r"[\?]"), "[42]");
    }
}
