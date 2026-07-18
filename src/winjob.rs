//! Windows background jobs: `cmd &`, `jobs`, `$!` — the Windows counterpart
//! of `job.rs`, per `docs/WINDOWS_JOB_CONTROL.md`. Not a port of `job.rs`'s
//! `libc` process-group/signal calls, which have no Windows target: this is
//! a new mechanism built on Windows Job Objects (`rusty_win32::job`) and a
//! raw suspended-spawn (`rusty_win32::process::spawn_suspended`), the
//! primitive that lets a process be assigned to a job *before* its main
//! thread — or anything it later spawns — runs a single instruction.
//!
//! **Milestone 1 (this file's current state, see the design doc's staging
//! plan): a single external command only.** No pipelines, no compound
//! stages, no backgrounding a builtin or function — each rejected with a
//! plain error rather than silently doing something wrong. `wait`/`kill`/
//! `disown`/multi-job `jobs -l` parity are follow-up milestones.
//!
//! `fg`/`bg`/Ctrl-Z terminal hand-off are permanently out of scope (see the
//! design doc's "Deliberately out of scope" section) — Windows consoles
//! have no equivalent of `tcsetpgrp`. This module never claims that
//! capability the way `job.rs`'s `job_control_enabled()` does; whether to
//! announce `[id] pid` for an interactive shell is decided directly from
//! `vars::interactive()` instead.

use std::cell::RefCell;

use crate::exec::{Pipeline, Stage};

struct JobEntry {
    id: usize,
    /// Owns kill-on-close semantics: closing this handle (e.g. on shell
    /// exit, once that's wired up) kills every process in it, including
    /// any grandchild the backgrounded command spawned itself.
    job: rusty_win32::RawHandle,
    process: rusty_win32::RawHandle,
    pid: u32,
    cmd: String,
    /// `None` while still running. Windows has no `SIGCHLD` push
    /// notification, so this is filled in by polling (`refresh_all`) — the
    /// design doc's endorsed first cut, ahead of a completion-port-based
    /// upgrade.
    exit_code: Option<u32>,
}

#[derive(Default)]
struct State {
    jobs: Vec<JobEntry>,
    next_id: usize,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Names dispatched by [`builtin`], for `builtins::other_is_builtin`/
/// `other_names`. Grows as `wait`/`kill`/`disown` land (see the module doc).
pub const NAMES: &[&str] = &["jobs"];

pub fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Dispatch the job-control builtins this module currently implements.
/// Returns `Some(code)` if handled.
pub fn builtin(argv: &[String]) -> Option<i32> {
    match argv.first().map(String::as_str)? {
        "jobs" => Some(jobs_cmd(argv)),
        _ => None,
    }
}

/// Every current job's own id — for completion, matching `job.rs::ids`.
pub fn ids() -> Vec<usize> {
    STATE.with(|s| s.borrow().jobs.iter().map(|j| j.id).collect())
}

/// How many jobs the table currently tracks — the prompt's `\j` (matching
/// `job.rs::count`).
pub fn count() -> usize {
    STATE.with(|s| s.borrow().jobs.len())
}

/// Run `pipeline` in the background: spawn it suspended, put it in a fresh
/// Job Object, resume it, and record it. See the module doc for milestone
/// 1's scope — anything beyond a single external command is a plain error,
/// not a silent narrowing.
pub fn run_background(pipeline: &Pipeline) -> Result<(), String> {
    let [Stage::Simple(cmd)] = pipeline.commands.as_slice() else {
        return Err("background pipelines are not supported on this platform yet".into());
    };
    if cmd
        .argv
        .first()
        .is_some_and(|n| crate::func::exists(n) || crate::builtins::is_builtin(n))
    {
        return Err(
            "backgrounding a builtin or function is not supported on this platform yet".into(),
        );
    }
    let program = cmd
        .argv
        .first()
        .ok_or_else(|| "empty command".to_string())?;
    if crate::vars::restricted() && program.contains('/') {
        return Err(format!(
            "{program}: restricted: cannot specify `/' in command names"
        ));
    }

    let mut resolved_argv = cmd.argv.clone();
    resolved_argv[0] = crate::exec::resolve_program(program);
    let command_line = build_command_line(&resolved_argv);
    let env_vars = environment_pairs(cmd);
    let env_block = rusty_win32::process::environment_block(
        env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    // No process spawned yet past this point, so a failure here has
    // nothing to clean up beyond the job handle itself.
    let job = rusty_win32::job::create().map_err(|e| win_err(program, e))?;
    // SAFETY: `job` was just created, valid, not yet closed.
    if let Err(e) = unsafe { rusty_win32::job::set_kill_on_close(job) } {
        close_job(job);
        return Err(win_err(program, e));
    }

    // Redirects apply only for the instant of spawning: `CreateProcessW`
    // snapshots the std-handle slots it inherits at that moment, so the
    // guard is dropped (restoring the shell's own stdio) immediately
    // after, before this returns control to the prompt.
    let guard = match crate::exec::redirect_stdio(&cmd.redirects, cmd.heredoc.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            close_job(job);
            return Err(e);
        }
    };
    // SAFETY: `command_line` was built by `build_command_line`, which
    // quotes every argument (including the resolved program path);
    // `env_block` was built by `environment_block`, which always
    // double-NUL-terminates.
    let spawned =
        unsafe { rusty_win32::process::spawn_suspended(&command_line, true, Some(&env_block)) };
    drop(guard);
    let spawned = match spawned {
        Ok(s) => s,
        Err(e) => {
            close_job(job);
            // Matches job.rs's own background-spawn-failure handling: the
            // script isn't aborted, `$?`/pipestatus for `cmd &` stay 0
            // regardless (set unconditionally by `run_job`), and this
            // reports the same "not found"/"found but couldn't run"
            // message a foreground spawn failure would.
            crate::exec::spawn_failure_status(&cmd.argv, &std::io::Error::from(e));
            return Ok(());
        }
    };

    // SAFETY: `job`/`spawned.process` are both valid; the child is still
    // suspended (not resumed until below), so job membership is guaranteed
    // before it — or anything it later spawns — executes a single
    // instruction.
    if let Err(e) = unsafe { rusty_win32::job::assign(job, spawned.process) } {
        // Known, narrow gap: `rusty_win32` has no raw `TerminateProcess`,
        // and the process was never a job member (assignment is what just
        // failed), so there's no way to kill it from here — it's left
        // suspended and orphaned rather than resumed untracked, which
        // would be the worse failure mode. Expected to be very rare:
        // `AssignProcessToJobObject` failing on a process this function
        // itself just created isn't an ordinary, command-specific error.
        // SAFETY: both handles are valid, each closed exactly once.
        unsafe {
            let _ = rusty_win32::handle::close(spawned.process);
            let _ = rusty_win32::handle::close(spawned.thread);
        }
        close_job(job);
        return Err(win_err(program, e));
    }
    // SAFETY: `spawned.thread` is a freshly created, valid, not-yet-resumed
    // thread handle; job assignment above already committed, so this is
    // safe to clean up via the job either way.
    if let Err(e) = unsafe { rusty_win32::process::resume(spawned.thread) } {
        // Unlike the assign failure above, the process *is* a job member
        // now, so `TerminateJobObject` can actually reach and kill it.
        // SAFETY: `job` is valid; `spawned.process` is a member of it.
        unsafe {
            let _ = rusty_win32::job::terminate(job, 1);
            let _ = rusty_win32::handle::close(spawned.process);
            let _ = rusty_win32::handle::close(spawned.thread);
        }
        close_job(job);
        return Err(win_err(program, e));
    }
    // SAFETY: no longer needed once resumed — `wait` only ever touches
    // `spawned.process`, and closing the thread handle doesn't affect the
    // (now running) thread itself.
    unsafe {
        let _ = rusty_win32::handle::close(spawned.thread);
    }

    let cmd_text = crate::exec::pipeline_text(pipeline);
    let id = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.next_id += 1;
        let id = s.next_id;
        s.jobs.push(JobEntry {
            id,
            job,
            process: spawned.process,
            pid: spawned.process_id,
            cmd: cmd_text,
            exit_code: None,
        });
        id
    });
    // `$!` (matching job.rs: the directly-spawned process's own pid).
    crate::vars::set_last_bg_pid(spawned.process_id as i32);
    // Only announced interactively, matching job.rs's own gating (a
    // non-interactive script prints nothing here).
    if crate::vars::interactive() {
        println!("[{id}] {}", spawned.process_id);
    }
    Ok(())
}

/// Poll every not-yet-known-finished job's tracked process handle
/// (`GetExitCodeProcess` via `rusty_win32::process::wait` with a zero
/// timeout) — the design doc's endorsed first cut ahead of a
/// completion-port-based push notification. Only reaches the *directly
/// spawned* process, not the whole job subtree — correct for milestone 1's
/// single-external-command scope; a job whose command itself backgrounds
/// further children would need `job::process_ids`/completion ports to
/// track accurately, a follow-up.
fn refresh_all() {
    STATE.with(|s| {
        for j in &mut s.borrow_mut().jobs {
            if j.exit_code.is_none() {
                // SAFETY: `j.process` is a valid, currently-open handle
                // this job entry owns exclusively until closed by
                // `reap_background`.
                if let Ok(Some(code)) = unsafe { rusty_win32::process::wait(j.process, Some(0)) } {
                    j.exit_code = Some(code);
                }
            }
        }
    });
}

/// Report and drop finished background jobs — called once before each
/// prompt (`main.rs`'s non-Unix counterpart to `job::reap_background`).
pub fn reap_background() {
    refresh_all();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for j in s.jobs.iter().filter(|j| j.exit_code.is_some()) {
            eprintln!("[{}]  Done\t{}", j.id, j.cmd);
            // SAFETY: `j.process`/`j.job` are valid, each closed exactly
            // once here, right before the entry itself is dropped below.
            unsafe {
                let _ = rusty_win32::handle::close(j.process);
                let _ = rusty_win32::handle::close(j.job);
            }
        }
        s.jobs.retain(|j| j.exit_code.is_none());
    });
}

fn jobs_cmd(argv: &[String]) -> i32 {
    let mut long = false;
    for arg in &argv[1..] {
        match arg.as_str() {
            "-l" => long = true,
            other => {
                eprintln!("jobs: {other}: invalid option");
                return 2;
            }
        }
    }
    refresh_all();
    STATE.with(|s| {
        for j in &s.borrow().jobs {
            let label = if j.exit_code.is_some() {
                "Done"
            } else {
                "Running"
            };
            if long {
                println!("[{}]  {} {}\t{}", j.id, j.pid, label, j.cmd);
            } else {
                println!("[{}]  {}\t{}", j.id, label, j.cmd);
            }
        }
    });
    0
}

fn close_job(job: rusty_win32::RawHandle) {
    // SAFETY: `job` is a valid handle owned solely by the caller at this
    // point (no `JobEntry` has been created for it yet on any of this
    // function's error paths), closed exactly once.
    unsafe {
        let _ = rusty_win32::handle::close(job);
    }
}

fn win_err(program: &str, e: rusty_win32::Win32Error) -> String {
    format!("{program}: {}", std::io::Error::from(e))
}

/// This command's environment: exported shell variables, overridden by its
/// own `NAME=value` prefix assignments — the same precedence
/// `exec::build_stage` uses for `std::process::Command`, reimplemented here
/// since `spawn_suspended` needs a caller-built block rather than
/// `Command`'s own `.env_clear()`/`.envs()`.
fn environment_pairs(cmd: &crate::exec::Command) -> Vec<(String, String)> {
    let mut vars = crate::vars::exported();
    for (name, op) in &cmd.assignments {
        // Matches `build_stage`: a prefix assignment naming a readonly
        // variable errors but still runs the command, with the assignment
        // dropped.
        if crate::vars::is_readonly(name) {
            eprintln!("rush: {name}: readonly variable");
            continue;
        }
        let Some(value) = crate::exec::prefix_env_value(name, op) else {
            continue;
        };
        match vars.iter_mut().find(|(n, _)| n == name) {
            Some(existing) => existing.1 = value,
            None => vars.push((name.clone(), value)),
        }
    }
    vars
}

/// Join `argv` into one correctly-quoted Windows command line, for
/// `spawn_suspended` (which — like `CreateProcessW` itself — takes a
/// single string, not an argv array, and does no quoting of its own).
/// `std::process::Command` solves this internally but doesn't expose it
/// (see `rusty_win32::process`'s own module doc); this is the same
/// algorithm the Rust standard library's Windows backend uses (the
/// MSVCRT/`CommandLineToArgvW` convention), reimplemented here since
/// there's no public API to call into instead.
fn build_command_line(argv: &[String]) -> String {
    let mut out = String::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        quote_arg(arg, &mut out);
    }
    out
}

fn quote_arg(arg: &str, out: &mut String) {
    let quote = arg.is_empty() || arg.contains(' ') || arg.contains('\t');
    if quote {
        out.push('"');
    }
    let mut backslashes = 0usize;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                // `n` backslashes followed by a `"` need `2n+1` backslashes
                // before the literal `"` — `n` to escape themselves, one
                // more so the quote itself is escaped rather than closing
                // the argument.
                for _ in 0..=backslashes {
                    out.push('\\');
                }
            }
            backslashes = 0;
        }
        out.push(c);
    }
    if quote {
        // Trailing backslashes right before the closing quote need
        // doubling too, or they'd escape *it* instead of standing for
        // themselves.
        for _ in 0..backslashes {
            out.push('\\');
        }
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_arg_leaves_simple_words_unquoted() {
        let mut out = String::new();
        quote_arg("hello", &mut out);
        assert_eq!(out, "hello");
    }

    #[test]
    fn quote_arg_quotes_a_word_containing_a_space() {
        let mut out = String::new();
        quote_arg("hello world", &mut out);
        assert_eq!(out, "\"hello world\"");
    }

    #[test]
    fn quote_arg_quotes_the_empty_string() {
        let mut out = String::new();
        quote_arg("", &mut out);
        assert_eq!(out, "\"\"");
    }

    #[test]
    fn quote_arg_escapes_an_embedded_quote() {
        let mut out = String::new();
        quote_arg("say \"hi\"", &mut out);
        assert_eq!(out, "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn quote_arg_doubles_trailing_backslashes_before_the_closing_quote() {
        // A trailing backslash right before the closing quote must double
        // (else it would escape the quote instead of standing for itself);
        // a backslash *not* immediately followed by a quote is literal and
        // needs no escaping at all.
        let mut out = String::new();
        quote_arg(r"C:\path with spaces\", &mut out);
        assert_eq!(out, r#""C:\path with spaces\\""#);

        let mut out = String::new();
        quote_arg(r"C:\no\spaces\here", &mut out);
        assert_eq!(out, r"C:\no\spaces\here");
    }

    #[test]
    fn build_command_line_joins_and_quotes_each_argument() {
        let argv = vec![
            "C:\\Program Files\\app.exe".to_string(),
            "a b".to_string(),
            "c".to_string(),
        ];
        assert_eq!(
            build_command_line(&argv),
            "\"C:\\Program Files\\app.exe\" \"a b\" c"
        );
    }
}
