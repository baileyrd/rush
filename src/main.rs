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

// Every module lives in the `rush` lib crate (`src/lib.rs`) now — pulled
// in here as a glob import so the rest of this file's bare `vars::`/
// `builtins::`/… paths (and `mod tests`' `use super::*`) keep resolving
// exactly as they did when these were local `mod` declarations. Split out
// so `fuzz/`/`benches/` can link against the same modules directly,
// in-process, instead of through a subprocess.
use rush::*;

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
    fn host_binding(&self, tag: &str, line: &mut String, cursor: &mut usize) {
        // `bind -x` (C128): run the bound command with READLINE_LINE /
        // READLINE_POINT set, then read them back into the buffer.
        let Some(cmd) = builtins::host_binding_command(tag) else { return };
        vars::set("READLINE_LINE", line);
        vars::set("READLINE_POINT", &cursor.to_string());
        if let Ok(list) = parser::parse(&cmd) {
            let _ = exec::run_list(&list);
        }
        if let Some(new_line) = vars::get("READLINE_LINE") {
            *line = new_line;
        }
        *cursor = vars::get("READLINE_POINT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(line.len())
            .min(line.len());
    }
}

/// `$HISTFILE` (C122), defaulting to `~/.rush_history`.
fn history_path() -> Option<PathBuf> {
    if let Some(f) = vars::get("HISTFILE").filter(|f| !f.is_empty()) {
        return Some(PathBuf::from(f));
    }
    let mut p = PathBuf::from(std::env::var_os("HOME")?);
    p.push(".rush_history");
    Some(p)
}

/// Whether a just-accepted command should be recorded, per
/// `$HISTCONTROL` (`ignorespace`/`ignoredups`/`ignoreboth`) and
/// `$HISTIGNORE` (colon-separated glob patterns) — C122.
fn history_should_record(cmd: &str, last: Option<&str>) -> bool {
    let control = vars::get("HISTCONTROL").unwrap_or_default();
    let has = |name: &str| control.split(':').any(|c| c == name || c == "ignoreboth");
    if has("ignorespace") && cmd.starts_with(' ') {
        return false;
    }
    if has("ignoredups") && last == Some(cmd) {
        return false;
    }
    if let Some(patterns) = vars::get("HISTIGNORE") {
        for pat in patterns.split(':').filter(|p| !p.is_empty()) {
            if glob::match_component(pat, cmd) {
                return false;
            }
        }
    }
    true
}

/// Apply the `$HISTSIZE`/`erasedups` knobs to the editor (C122).
fn apply_history_knobs(rl: &mut Editor) {
    if let Some(n) = vars::get("HISTSIZE").and_then(|v| v.parse::<usize>().ok()) {
        rl.set_max_history_len(n);
    }
    let control = vars::get("HISTCONTROL").unwrap_or_default();
    rl.set_history_dedup(control.split(':').any(|c| c == "erasedups"));
    // `$HISTTIMEFORMAT` set → persist `#<epoch>` timestamp lines in the
    // history file, bash's format (C122).
    rl.set_history_timestamps(vars::get("HISTTIMEFORMAT").is_some_and(|f| !f.is_empty()));
}

thread_local! {
    // `--norc` / `--rcfile FILE` (C104).
    static RC_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RC_OVERRIDE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn rc_path() -> Option<PathBuf> {
    if RC_DISABLED.with(std::cell::Cell::get) {
        return None;
    }
    if let Some(f) = RC_OVERRIDE.with(|p| p.borrow().clone()) {
        return Some(PathBuf::from(f));
    }
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
    match vars::get("PS1") {
        // After the escape pass, `$(...)`/`${...}` expand too (bash's
        // default `promptvars`, C126) — how every git-branch-in-prompt
        // setup works.
        Some(ps1) => {
            let escaped = expand::expand_ps1(&ps1);
            expand::expand_dollars(&escaped).unwrap_or(escaped)
        }
        None => default_prompt(),
    }
}

/// `$PS2` with the same escape + `$` expansion as `$PS1` (C127) — the
/// continuation prompt used to be a hardcoded `"> "`.
fn prompt_ps2() -> String {
    match vars::get("PS2") {
        Some(ps2) => {
            let escaped = expand::expand_ps1(&ps2);
            expand::expand_dollars(&escaped).unwrap_or(escaped)
        }
        None => "> ".to_string(),
    }
}

/// Run `$PROMPT_COMMAND` before each primary prompt (C126) — exit-status
/// coloring, terminal titles, `history -a`-style hooks. `$?` is preserved
/// so the prompt itself can still show the last command's status.
fn run_prompt_command() {
    if let Some(cmd) = vars::get("PROMPT_COMMAND").filter(|c| !c.is_empty()) {
        let saved = vars::last_status();
        if let Ok(list) = parser::parse(&cmd) {
            let _ = exec::run_list(&list);
        }
        vars::set_last_status(saved);
    }
}

fn default_prompt() -> String {
    format!("{} $ ", expand::cwd_string())
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
        sys::signal(sys::SIGPIPE, sys::SIG_DFL);
    }
    // `TERM`/`HUP` traps (C21) need to work in every mode, not just
    // interactively — the target use case (a container's PID 1 catching
    // `TERM` to shut down gracefully) has no terminal at all.
    #[cfg(unix)]
    trap::install_signal_handlers();

    // Remember the original stdin handle before any redirect can swap the
    // std-handle slot — how `read` later tells "fd 0 is a redirect target"
    // apart from "fd 0 is the shell's own stdin" (see `winstdio`).
    #[cfg(not(unix))]
    winstdio::capture_startup_stdin();

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
    // Windows' own environment block can store this as "Path" (or another
    // casing) rather than POSIX's "PATH" — the loop above seeds whatever
    // casing `std::env::vars()`'s enumeration happens to report, but every
    // `PATH` lookup in this shell (`vars::get("PATH")`) is a literal,
    // case-sensitive match — POSIX correctness: `$PATH` and `$path` really
    // are different variables, so that can't change. `std::env::var`
    // itself resolves case-insensitively on Windows (`GetEnvironmentVariableW`),
    // so re-seeding straight through it lands the value under the exact
    // name every PATH lookup here expects, whatever case it actually
    // arrived in. Without this, external-command resolution silently
    // finds nothing at all on Windows: `vars::get("PATH")` sees `None`,
    // not merely a differently-cased miss.
    #[cfg(not(unix))]
    if let Ok(path) = std::env::var("PATH") {
        vars::set_exported("PATH", &path);
    }

    // `$$` — capture the original shell pid before any subshell fork, so
    // it stays stable in `( )` (C132; `$BASHPID` tracks the live pid).
    vars::set_shell_pid();

    // `$PPID` (C41): the invoking process's pid, seeded once at startup as
    // an ordinary (non-exported, same as bash) shell variable — after the
    // environment loop above, so a stale inherited `PPID` from a parent
    // shell's environment can't shadow the real value.
    #[cfg(unix)]
    vars::set("PPID", &unsafe { sys::getppid() }.to_string());

    // The standard identity/platform variables (C106), seeded like PPID
    // (after the environment loop, so stale inherited copies can't shadow
    // the real values). `UID`/`EUID` are readonly, same as bash.
    #[cfg(unix)]
    {
        let uid = unsafe { sys::getuid() }.to_string();
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
    // `$BASH_VERSION`/`$BASH_VERSINFO` (compat shim): plenty of real-world
    // scripts gate bash-only syntax behind `[ -n "$BASH_VERSION" ]` or
    // inspect `$BASH_VERSINFO` for a minimum version before using it, and
    // silently fall back to a POSIX-only path under any other shell —
    // rush qualifies for neither check today. `docs/CAPABILITY_GAPS.md`
    // verifies rush's own parity against bash 5.2 specifically, so that's
    // the version reported here: an honest "if it works on bash 5.2, it
    // should work here" signal, not a claim to *be* bash — `$RUSH_VERSION`
    // still identifies the real shell. `BASH_VERSINFO` is readonly,
    // matching bash; `BASH_VERSION` stays a plain assignable variable,
    // also matching bash (scripts do sometimes override it to force a
    // compat path).
    vars::set("BASH_VERSION", "5.2.21(1)-release");
    vars::set_array(
        "BASH_VERSINFO",
        vec![
            "5".to_string(),
            "2".to_string(),
            "21".to_string(),
            "1".to_string(),
            "release".to_string(),
            vars::get("MACHTYPE").unwrap_or_default(),
        ],
    );
    vars::set_attrs("BASH_VERSINFO", vars::Attrs { readonly: true, ..Default::default() });
    // `$DIRSTACK` (bash's array view of the `pushd`/`popd` stack):
    // `DIRSTACK[0]` is always the current directory even before any
    // `pushd`, so seed it once at startup rather than waiting for the
    // first stack mutation.
    builtins::sync_dirstack();
    // Functions exported by a parent shell (`BASH_FUNC_name%%` entries,
    // C98) become defined functions.
    builtins::import_functions();
    // `$COLUMNS`/`$LINES` (C129): seed from the terminal via `stty size`
    // (rush has no termios binding of its own — see the rusty_lines
    // handoff; `checkwinsize` refresh happens in the REPL loop).
    update_winsize();
    // `$IFS` is always *set* in bash (space-tab-newline default) — rush's
    // splitter already treated unset-IFS as that default, but `"$IFS"`
    // itself expanded empty, and a prefix-assignment restore (C75) needs a
    // real prior value to put back.
    if vars::get("IFS").is_none() {
        vars::set("IFS", " \t\n");
    }
    // `$SHLVL` increments per nested shell (C106) and stays exported.
    let shlvl = vars::get("SHLVL").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
    vars::set_exported("SHLVL", &shlvl.to_string());

    let args: Vec<String> = std::env::args().collect();

    // Hidden self-re-exec entry point (`exec::capture_via_self_reexec`):
    // off Unix, `$(builtin_or_function ...)` spawns this same binary as a
    // real child to get subshell isolation without `fork`. Checked before
    // any ordinary flag parsing — this is never a real invocation a user
    // would type.
    #[cfg(not(unix))]
    if args.get(1).map(String::as_str) == Some("--rush-internal-run-builtin") {
        std::process::exit(exec::run_internal_capture(args[2..].to_vec()));
    }

    // Invocation flags (C104), bash's set: `-c cmd`, `-s` (stdin with
    // positional args — the `curl | sh -s -- args` pipeline shape), `-i`
    // (force interactive), `-l`/`--login`, `-r`/`--restricted`, `-n`
    // (C51's lint mode), `--posix` (accepted), `--norc`/`--rcfile FILE`,
    // `-O`/`+O shopt_name`, and `--`. Single-letter flags cluster
    // (`-lc`). An argv[0] starting with `-` is a login shell too.
    let mut command: Option<String> = None;
    let mut read_stdin = false;
    let mut force_interactive = false;
    let mut login = args.first().is_some_and(|a| a.starts_with('-'));
    let mut idx = 1;
    while idx < args.len() {
        let arg = args[idx].as_str();
        match arg {
            "--" => {
                idx += 1;
                break;
            }
            "--login" => login = true,
            "--restricted" => vars::set_restricted(true),
            "--posix" => {} // accepted: rush's default behavior is the target
            "--norc" => RC_DISABLED.with(|f| f.set(true)),
            "--rcfile" | "--init-file" => {
                idx += 1;
                match args.get(idx) {
                    Some(f) => RC_OVERRIDE.with(|p| *p.borrow_mut() = Some(f.clone())),
                    None => {
                        eprintln!("rush: {arg}: option requires an argument");
                        std::process::exit(2);
                    }
                }
            }
            "-O" | "+O" => {
                idx += 1;
                match args.get(idx) {
                    Some(name) => {
                        let _ = vars::set_shopt(name, arg == "-O");
                    }
                    None => {
                        eprintln!("rush: {arg}: option requires an argument");
                        std::process::exit(2);
                    }
                }
            }
            flags
                if flags.len() > 1
                    && flags.starts_with('-')
                    && flags[1..].chars().all(|c| matches!(c, 'c' | 's' | 'i' | 'l' | 'r' | 'n')) =>
            {
                for c in flags[1..].chars() {
                    match c {
                        'c' => {
                            vars::set_invoked_with_c(); // `$-` includes `c` (C131)
                            idx += 1;
                            match args.get(idx) {
                                Some(cmd) => command = Some(cmd.clone()),
                                None => {
                                    eprintln!("rush: -c: option requires an argument");
                                    std::process::exit(2);
                                }
                            }
                        }
                        's' => read_stdin = true,
                        'i' => force_interactive = true,
                        'l' => login = true,
                        'r' => vars::set_restricted(true),
                        'n' => vars::set_noexec(true),
                        _ => unreachable!(),
                    }
                }
            }
            _ => break, // first non-flag word: the script file (or -c's name)
        }
        idx += 1;
    }
    if login {
        vars::set_shopt("login_shell", true);
    }

    // A login shell sources ~/.profile before anything else (kept to the
    // one file every shell family reads — no /etc/profile pass).
    if login
        && let Some(home) = vars::get("HOME")
        && let Ok(src) = std::fs::read_to_string(format!("{home}/.profile"))
    {
        run_source(&src);
    }

    if let Some(cmd) = command {
        let name = args.get(idx).cloned().unwrap_or_else(|| "rush".to_string());
        vars::set_args(name, args.get(idx + 1..).unwrap_or(&[]).to_vec());
        source_bash_env();
        trap::exit_shell(run_source(&cmd));
    }
    // `-s`, or plain `rush` with leftover args after `--`: read stdin,
    // with the remaining words as positional parameters.
    if read_stdin || (idx < args.len() && args[idx - 1] == "--") {
        let name = args.first().cloned().unwrap_or_else(|| "rush".to_string());
        vars::set_args(name, args.get(idx..).unwrap_or(&[]).to_vec());
        vars::set_invoked_with_s(); // `$-` includes `s` when reading stdin
        if force_interactive {
            return interactive();
        }
        let mut src = String::new();
        use std::io::Read as _;
        if std::io::stdin().read_to_string(&mut src).is_err() {
            trap::exit_shell(1);
        }
        source_bash_env();
        trap::exit_shell(run_source(&src));
    }

    // Non-interactive modes: `rush FILE [args…]`; otherwise the REPL.
    match args.get(idx).map(String::as_str) {
        Some(file) => {
            vars::set_args(file.to_string(), args.get(idx + 1..).unwrap_or(&[]).to_vec());
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
        // `-i` forces the interactive REPL even on a pipe (bash's own
        // rule, and how interactive features are driven in tests).
        None if force_interactive => interactive(),
        // A plain `rush` with a terminal on stdin is the interactive REPL;
        // with a pipe or file redirected onto stdin it reads commands from
        // stdin as a *non-interactive* script — so `$-` gets `s`, not `i`,
        // and interactive-only behaviour stays off (matching bash).
        None if !std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
            vars::set_invoked_with_s();
            let mut src = String::new();
            use std::io::Read as _;
            if std::io::stdin().read_to_string(&mut src).is_err() {
                trap::exit_shell(1);
            }
            source_bash_env();
            trap::exit_shell(run_source(&src));
        }
        None => interactive(),
    }
}

/// Refresh `$COLUMNS`/`$LINES` from `stty size` (C129) — a portable
/// stopgap until rusty_lines exposes `TIOCGWINSZ` (see the handoff doc).
/// No-op when stdin isn't a terminal or `stty` isn't available.
fn update_winsize() {
    // Native `TIOCGWINSZ` via rusty_lines (C129) — the stty shell-out is
    // gone now that the crate exposes it.
    if let Some((cols, rows)) = rusty_lines::terminal_size() {
        vars::set("COLUMNS", &cols.to_string());
        vars::set("LINES", &rows.to_string());
    }
}

/// Drain the `bind` builtin's queued rebindings into the live editor
/// (C128); refresh the snapshot `bind -P` reads from.
fn apply_pending_binds(rl: &mut Editor) {
    for pending in builtins::take_pending_binds() {
        use builtins::PendingBind;
        let _ = match pending {
            PendingBind::Action(keys, action) => match editor_action(&action) {
                Some(a) => rl.bind(&keys, a),
                None => Ok(()),
            },
            PendingBind::Host(keys, _cmd) => rl.bind_host(&keys, keys.clone()),
            PendingBind::Unbind(keys) => rl.unbind(&keys),
        };
    }
    builtins::set_bindings_snapshot(
        rl.bindings().map(|(spec, action)| (spec, format!("{action:?}"))).collect(),
    );
}

/// Map rush's action-name string to a `rusty_lines::EditorAction`.
fn editor_action(name: &str) -> Option<rusty_lines::EditorAction> {
    use rusty_lines::EditorAction as A;
    Some(match name {
        "BeginningOfLine" => A::BeginningOfLine,
        "EndOfLine" => A::EndOfLine,
        "ForwardChar" => A::ForwardChar,
        "BackwardChar" => A::BackwardChar,
        "ForwardWord" => A::ForwardWord,
        "BackwardWord" => A::BackwardWord,
        "KillLine" => A::KillLine,
        "UnixLineDiscard" => A::UnixLineDiscard,
        "UnixWordRubout" => A::UnixWordRubout,
        "KillWord" => A::KillWord,
        "BackwardKillWord" => A::BackwardKillWord,
        "DeleteChar" => A::DeleteChar,
        "BackwardDeleteChar" => A::BackwardDeleteChar,
        "Yank" => A::Yank,
        "YankPop" => A::YankPop,
        "TransposeChars" => A::TransposeChars,
        "TransposeWords" => A::TransposeWords,
        "UpcaseWord" => A::UpcaseWord,
        "DowncaseWord" => A::DowncaseWord,
        "CapitalizeWord" => A::CapitalizeWord,
        "Undo" => A::Undo,
        "RevertLine" => A::RevertLine,
        "InsertLastArgument" => A::InsertLastArgument,
        "PreviousHistory" => A::PreviousHistory,
        "NextHistory" => A::NextHistory,
        "BeginningOfHistory" => A::BeginningOfHistory,
        "EndOfHistory" => A::EndOfHistory,
        "HistorySearchBackward" => A::HistorySearchBackward,
        "HistorySearchForward" => A::HistorySearchForward,
        "ReverseSearchHistory" => A::ReverseSearchHistory,
        "ForwardSearchHistory" => A::ForwardSearchHistory,
        "ClearScreen" => A::ClearScreen,
        "Complete" => A::Complete,
        "MenuComplete" => A::MenuComplete,
        "QuotedInsert" => A::QuotedInsert,
        "EditAndExecuteCommand" => A::EditAndExecuteCommand,
        "AcceptLine" => A::AcceptLine,
        _ => return None,
    })
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
    // Seed the `history` builtin's mirror with what the file held, and
    // apply $HISTSIZE/$HISTCONTROL (C122).
    for entry in rl.history() {
        builtins::history_record(entry);
    }
    apply_history_knobs(&mut rl);
    apply_pending_binds(&mut rl); // ~/.rushrc may have called `bind` (C128)

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
    // Consecutive Ctrl-D presses at an empty prompt, for `$IGNOREEOF` (C130's
    // rush-side half): with it set to n, only the (n+1)th EOF exits.
    let mut eof_count = 0u32;

    loop {
        // Report any background jobs that finished or stopped since last prompt.
        #[cfg(unix)]
        job::reap_background();
        #[cfg(not(unix))]
        winjob::reap_background();
        // Fire (or default-terminate on) any TERM/HUP received since the last
        // prompt — same idea as `reap_background`, for signals instead of jobs.
        #[cfg(unix)]
        trap::check_pending();

        if buffer.is_empty() {
            if vars::shopt("checkwinsize") {
                update_winsize(); // C129 — bash 5's default
            }
            run_prompt_command(); // C126
        }
        let prompt = if buffer.is_empty() { prompt() } else { prompt_ps2() };
        // The right prompt ($RPS1, C71): ordinary `$`-expansion each
        // time, like $PS3 — empty (hidden) when unset.
        let rprompt = vars::get("RPS1")
            .and_then(|raw| expand::expand_dollars(&raw).ok())
            .unwrap_or_default();
        // `$TMOUT` (C130): idle auto-logout — a read with no complete
        // line before the deadline returns `TimedOut`.
        let tmout = vars::get("TMOUT")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .map(std::time::Duration::from_secs);
        match rl.read_line_timeout(&prompt, &rprompt, &ShellHooks, tmout)? {
            ReadResult::Line(line) => {
                eof_count = 0;
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
                if !buffer.is_empty() {
                    buffer.push('\n');
                }
                buffer.push_str(&line);

                // One history entry per complete *command* (`cmdhist`,
                // C124) — the editor joins embedded newlines with `; `,
                // so a multi-line `for` recalls as one runnable line —
                // recorded only when $HISTCONTROL/$HISTIGNORE allow
                // (C122), and appended to $HISTFILE right away so a
                // crashed session loses nothing and concurrent sessions
                // interleave instead of clobbering (C123).
                let record = |rl: &mut Editor, buffer: &str| {
                    if history_should_record(buffer, rl.history().last().map(String::as_str)) {
                        rl.add_history_entry(buffer);
                        builtins::history_record(&buffer.replace('\n', "; "));
                        if let Some(ref h) = history_path() {
                            let _ = rl.append_history(h);
                        }
                    }
                };
                match parser::parse(&buffer) {
                    Ok(list) => {
                        record(&mut rl, &buffer);
                        // `$PS0` (C126's remainder): printed after a
                        // complete command is read, before it executes —
                        // same expansion as PS1.
                        if let Some(ps0) = vars::get("PS0").filter(|p| !p.is_empty()) {
                            let escaped = expand::expand_ps1(&ps0);
                            print!("{}", expand::expand_dollars(&escaped).unwrap_or(escaped));
                            use std::io::Write as _;
                            let _ = std::io::stdout().flush();
                        }
                        if let Err(e) = exec::run_list(&list) {
                            eprintln!("rush: {e}");
                        }
                        buffer.clear();
                    }
                    // A valid prefix: keep reading more lines.
                    Err(parser::ParseError::Incomplete) => {}
                    Err(parser::ParseError::Syntax(e)) => {
                        record(&mut rl, &buffer);
                        eprintln!("rush: {e}");
                        buffer.clear();
                    }
                }
                // `history -c`/`-d`/`-r`/`-s` mutated the mirror: sync the
                // editor's own list in place — `replace_history` keeps the
                // kill ring and undo stacks that a fresh `Editor` would
                // drop (C103, using the rusty_lines API from the handoff).
                if builtins::history_reset_pending() {
                    rl.replace_history(builtins::history_entries());
                }
                apply_history_knobs(&mut rl);
                // A `bind` run this turn takes effect before the next prompt.
                apply_pending_binds(&mut rl);
            }
            // Ctrl-C at an idle prompt (not a running foreground job — that's
            // a child process under job control, and never reaches here).
            ReadResult::Interrupted => {
                trap::fire("INT");
                buffer.clear();
                continue;
            }
            // Ctrl-D on an empty line: exit — unless `$IGNOREEOF` asks
            // for more presses first (guards the classic accidental
            // Ctrl-D killing an SSH session).
            ReadResult::Eof => {
                let required = vars::get("IGNOREEOF")
                    .map(|v| v.parse::<u32>().unwrap_or(10))
                    .unwrap_or(0);
                if eof_count < required {
                    eof_count += 1;
                    eprintln!("Use \"exit\" to leave the shell.");
                    continue;
                }
                break;
            }
            // `$TMOUT` idle timeout (C130): bash's "timed out waiting for
            // input" auto-logout.
            ReadResult::TimedOut => {
                eprintln!("\ntimed out waiting for input: auto-logout");
                break;
            }
        }
    }

    // History is appended incrementally after each command (C123) — no
    // whole-file overwrite at exit, so concurrent sessions interleave.
    trap::fire("EXIT");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_text_passes_through() {
        assert_eq!(expand::expand_ps1("plain > "), "plain > ");
    }

    #[test]
    fn ansi_and_bracket_escapes() {
        // C125: `\e` becomes ESC and `\[`/`\]` vanish — the standard
        // colored PS1 used to render as literal `\[\e[32m\]` garbage.
        assert_eq!(expand::expand_ps1(r"\[\e[32m\]x\[\e[0m\]"), "\x1b[32mx\x1b[0m");
        assert_eq!(expand::expand_ps1(r"\a\r"), "\x07\r");
        assert_eq!(expand::expand_ps1(r"\007"), "\x07");
        assert_eq!(expand::expand_ps1(r"\s"), "rush");
    }

    #[test]
    fn newline_and_backslash_escapes() {
        assert_eq!(expand::expand_ps1(r"a\nb"), "a\nb");
        assert_eq!(expand::expand_ps1(r"a\\b"), r"a\b");
    }

    #[test]
    fn unknown_escape_kept_literal() {
        assert_eq!(expand::expand_ps1(r"\z"), r"\z");
    }

    #[test]
    fn trailing_backslash_kept_literal() {
        assert_eq!(expand::expand_ps1(r"end\"), r"end\");
    }

    #[test]
    fn exit_status_escape() {
        vars::set_last_status(42);
        assert_eq!(expand::expand_ps1(r"[\?]"), "[42]");
    }
}
