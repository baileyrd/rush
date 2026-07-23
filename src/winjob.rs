//! Windows background jobs: `cmd &`, `jobs`, `$!` — the Windows counterpart
//! of `job.rs`, per `docs/WINDOWS_JOB_CONTROL.md`. Not a port of `job.rs`'s
//! `libc` process-group/signal calls, which have no Windows target: this is
//! a new mechanism built on Windows Job Objects (`rusty_win32::job`) and a
//! raw suspended-spawn (`rusty_win32::process::spawn_suspended`), the
//! primitive that lets a process be assigned to a job *before* its main
//! thread — or anything it later spawns — runs a single instruction.
//!
//! **Milestones 1–4 of the design doc's staging plan, plus `disown` and
//! pipelines of external commands, are implemented.** A pipeline stage
//! that's a builtin, function, or compound command is a plain error, not
//! a silent narrowing: Windows has no `fork()` for it to run in a
//! background child the way `job.rs`'s Unix
//! `spawn_builtin_stage`/`spawn_compound_stage` do — that's a permanent
//! limitation of this platform, not a staging gap.
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
    /// Owns kill-on-close semantics: closing this handle (including
    /// implicitly, at plain shell exit) kills every process in it,
    /// including any grandchild the backgrounded command spawned itself —
    /// unless [`disown_cmd`] has explicitly reversed that first.
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
    // Every pid this shell has itself waited to completion via `wait`,
    // `jobs`, or the per-prompt reap, with its exit code — so `wait` can
    // still report a background job's status even after something else
    // already reaped/pruned it, matching `job.rs`'s own `REAPED` map (see
    // its doc comment for why: waiting twice on the same pid still works).
    static REAPED: RefCell<std::collections::HashMap<u32, i32>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Names dispatched by [`builtin`], for `builtins::other_is_builtin`/
/// `other_names`.
pub const NAMES: &[&str] = &["jobs", "wait", "kill", "disown"];

pub fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Dispatch the job-control builtins this module currently implements.
/// Returns `Some(code)` if handled.
pub fn builtin(argv: &[String]) -> Option<i32> {
    match argv.first().map(String::as_str)? {
        "jobs" => Some(jobs_cmd(argv)),
        "wait" => Some(wait_cmd(argv)),
        "kill" => Some(kill_cmd(argv)),
        "disown" => Some(disown_cmd(argv)),
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

/// Run `pipeline` in the background: spawn every stage suspended
/// (connected by real pipes, for a multi-stage pipeline), put them all in
/// one fresh Job Object, resume each, and record the whole pipeline as a
/// single job. Every stage must be an external command — a builtin,
/// function, or compound command as a stage is a plain error, not a
/// silent narrowing, since Windows has no `fork()` for it to run in a
/// background child the way `job.rs`'s Unix
/// `spawn_builtin_stage`/`spawn_compound_stage` do.
pub fn run_background(pipeline: &Pipeline) -> Result<(), String> {
    let mut cmds: Vec<&crate::exec::Command> = Vec::with_capacity(pipeline.commands.len());
    for stage in &pipeline.commands {
        match stage {
            Stage::Simple(cmd) => {
                if cmd
                    .argv
                    .first()
                    .is_some_and(|n| crate::func::exists(n) || crate::builtins::is_builtin(n))
                {
                    return Err(
                        "backgrounding a builtin or function is not supported on this platform"
                            .into(),
                    );
                }
                cmds.push(cmd);
            }
            Stage::Compound(_) => {
                return Err(
                    "a compound command as a background pipeline stage is not supported on \
                     this platform"
                        .into(),
                );
            }
        }
    }
    let program = cmds
        .first()
        .and_then(|c| c.argv.first())
        .map(String::as_str)
        .unwrap_or_default();
    if crate::vars::restricted()
        && cmds
            .iter()
            .any(|c| c.argv.first().is_some_and(|p| p.contains('/')))
    {
        return Err(format!(
            "{program}: restricted: cannot specify `/' in command names"
        ));
    }

    // No process spawned yet past this point, so a failure here has
    // nothing to clean up beyond the job handle itself.
    let job = rusty_win32::job::create().map_err(|e| win_err(program, e))?;
    // SAFETY: `job` was just created, valid, not yet closed.
    if let Err(e) = unsafe { rusty_win32::job::set_kill_on_close(job) } {
        close_job(job);
        return Err(win_err(program, e));
    }

    let spawned = match spawn_pipeline_into_job(job, &cmds) {
        Ok(s) => s,
        Err(e) => {
            // SAFETY: `job` is valid; this tears down whatever stage(s)
            // already made it into the job before the one that failed —
            // `spawn_pipeline_into_job` itself only cleans up the failing
            // stage's own leftover handles, not earlier ones already
            // running, by design (this is the one place that needs to
            // know about all of them at once).
            unsafe {
                let _ = rusty_win32::job::terminate(job, 1);
            }
            close_job(job);
            // Matches job.rs's own background-spawn-failure handling: the
            // script isn't aborted, `$?`/pipestatus for `cmd &` stay 0
            // regardless (set unconditionally by `run_job`).
            eprintln!("rush: {e}");
            return Ok(());
        }
    };

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
    // `$!` — the *last* stage's own pid (verified against real bash
    // directly by `job.rs`'s own comment on the same convention); for a
    // single-command background job they're the same pid anyway.
    crate::vars::set_last_bg_pid(spawned.process_id as i32);
    // Only announced interactively, matching job.rs's own gating (a
    // non-interactive script prints nothing here).
    if crate::vars::interactive() {
        println!("[{id}] {}", spawned.process_id);
    }
    Ok(())
}

/// Spawn every stage of `cmds` suspended, connected by real pipes,
/// assigning each to `job` and resuming it before moving to the next —
/// job membership is guaranteed before each stage (or anything it spawns)
/// runs, same guarantee the single-stage case always had, just applied
/// per stage here. Returns the *last* stage's [`rusty_win32::process::SpawnedProcess`] —
/// the only one `winjob.rs` tracks for `wait`/`jobs` polling afterward
/// (matching `$!`'s own "last stage" convention); earlier stages' own
/// process/thread handles are closed here once assigned+resumed, since
/// their lifetime from then on is governed by `job` itself (reachable via
/// `kill %n`), not individually polled — tracking every stage's own exit
/// status (for a Windows `${PIPESTATUS[@]}` equivalent) is a possible
/// follow-up, not attempted here.
///
/// On failure partway through, only the *failing* stage's own leftover
/// handles are cleaned up here — any earlier stage that already made it
/// into `job` is deliberately left running, still assigned; the caller
/// tears down the whole job (and everything already in it) in one call
/// once this returns `Err`, rather than this function trying to track and
/// unwind each already-succeeded stage individually.
fn spawn_pipeline_into_job(
    job: rusty_win32::RawHandle,
    cmds: &[&crate::exec::Command],
) -> Result<rusty_win32::process::SpawnedProcess, String> {
    let n = cmds.len();
    let mut prev_read: Option<rusty_win32::RawHandle> = None;
    let mut last_spawned: Option<rusty_win32::process::SpawnedProcess> = None;

    for (i, cmd) in cmds.iter().enumerate() {
        let is_last = i == n - 1;
        let program = cmd.argv.first().map(String::as_str).unwrap_or_default();

        let next_pipe = if is_last {
            None
        } else {
            Some(rusty_win32::handle::create_pipe().map_err(|e| win_err(program, e))?)
        };
        let stdout_dst = next_pipe.map(|(_, w)| w);

        let spawn_result = spawn_stage(cmd, prev_read, stdout_dst);
        // Whatever happened, this process's own copies of the boundary
        // pipe ends (if any) aren't needed again either way — a spawned
        // child inherited its own; a failed spawn never needed them at
        // all. Closing them now, regardless of outcome, is what stops
        // them leaking into whatever spawns next via `inherit_handles`.
        if let Some(h) = prev_read {
            // SAFETY: opened by the previous iteration's `create_pipe`,
            // not used again after this.
            unsafe {
                let _ = rusty_win32::handle::close(h);
            }
        }
        if let Some(h) = stdout_dst {
            // SAFETY: opened just above, not used again after this.
            unsafe {
                let _ = rusty_win32::handle::close(h);
            }
        }
        let spawned = match spawn_result {
            Ok(s) => s,
            Err(e) => {
                if let Some((r, _)) = next_pipe {
                    // SAFETY: opened just above, never handed to anything.
                    unsafe {
                        let _ = rusty_win32::handle::close(r);
                    }
                }
                return Err(e);
            }
        };

        // SAFETY: `job`/`spawned.process` are both valid; the process is
        // still suspended (not resumed until below), so job membership is
        // guaranteed before it — or anything it later spawns — executes a
        // single instruction.
        if let Err(e) = unsafe { rusty_win32::job::assign(job, spawned.process) } {
            // SAFETY: neither handle has been used elsewhere; each closed
            // exactly once.
            unsafe {
                let _ = rusty_win32::handle::close(spawned.process);
                let _ = rusty_win32::handle::close(spawned.thread);
            }
            if let Some((r, _)) = next_pipe {
                unsafe {
                    let _ = rusty_win32::handle::close(r);
                }
            }
            return Err(win_err(program, e));
        }
        // SAFETY: `spawned.thread` is freshly created, valid, and
        // not-yet-resumed; job assignment above already committed, so
        // this is safe to clean up via the job either way.
        if let Err(e) = unsafe { rusty_win32::process::resume(spawned.thread) } {
            // SAFETY: `job` is valid; `spawned.process` is a member of it,
            // so terminating via the job is the simplest way to reach it
            // here, unlike the assign-failure case above (a raw
            // `TerminateProcess`, via `rusty_win32::process::terminate`,
            // would work too, but there's no reason to prefer it over the
            // job here — this process is already a member either way).
            unsafe {
                let _ = rusty_win32::job::terminate(job, 1);
                let _ = rusty_win32::handle::close(spawned.process);
                let _ = rusty_win32::handle::close(spawned.thread);
            }
            if let Some((r, _)) = next_pipe {
                unsafe {
                    let _ = rusty_win32::handle::close(r);
                }
            }
            return Err(win_err(program, e));
        }
        // SAFETY: no longer needed once resumed.
        unsafe {
            let _ = rusty_win32::handle::close(spawned.thread);
        }
        if !is_last {
            // Not tracked individually past this point — see this
            // function's own doc comment.
            // SAFETY: no longer needed; this stage's lifetime is governed
            // by `job` from here on.
            unsafe {
                let _ = rusty_win32::handle::close(spawned.process);
            }
        }

        prev_read = next_pipe.map(|(r, _)| r);
        last_spawned = Some(spawned);
    }

    Ok(last_spawned.expect("cmds is non-empty: run_background never calls this with none"))
}

/// Spawn one already-validated external-command pipeline stage.
/// `stdin_src`/`stdout_dst` — an adjacent stage's pipe end, if any — are
/// wired as this stage's *default* stdio; an explicit redirect in
/// `cmd.redirects` still wins over that default, matching
/// `exec::build_stage`'s own precedence for the foreground path. Returns
/// the still-suspended, not-yet-job-assigned child — the caller
/// ([`spawn_pipeline_into_job`]) owns assigning it to the pipeline's
/// shared job and resuming it.
fn spawn_stage(
    cmd: &crate::exec::Command,
    stdin_src: Option<rusty_win32::RawHandle>,
    stdout_dst: Option<rusty_win32::RawHandle>,
) -> Result<rusty_win32::process::SpawnedProcess, String> {
    let program = cmd
        .argv
        .first()
        .ok_or_else(|| "empty command".to_string())?;

    let mut resolved_argv = cmd.argv.clone();
    resolved_argv[0] = crate::exec::resolve_program(program);
    let command_line = build_command_line(&resolved_argv);
    let env_vars = environment_pairs(cmd);
    let env_block = rusty_win32::process::environment_block(
        env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );

    let prev_stdin = crate::winstdio::get(crate::winstdio::STD_INPUT_HANDLE);
    let prev_stdout = crate::winstdio::get(crate::winstdio::STD_OUTPUT_HANDLE);
    let prev_stderr = crate::winstdio::get(crate::winstdio::STD_ERROR_HANDLE);
    if let Some(h) = stdin_src {
        crate::winstdio::set(crate::winstdio::STD_INPUT_HANDLE, h);
    }
    if let Some(h) = stdout_dst {
        crate::winstdio::set(crate::winstdio::STD_OUTPUT_HANDLE, h);
    }

    // Redirects apply only for the instant of spawning: `CreateProcessW`
    // snapshots the std-handle slots it inherits at that moment, so the
    // guard is dropped (restoring whatever this function itself just set)
    // immediately after.
    let result =
        crate::exec::redirect_stdio(&cmd.redirects, cmd.heredoc.as_deref()).and_then(|guard| {
            // Neither the pipe ends `create_pipe` hands back (the
            // pipe-boundary default set above) nor a file `redirect_stdio`
            // itself just opened are inheritable by default — a Windows
            // `HANDLE` starts non-inheritable regardless of how it was
            // created (`rusty_win32::handle`'s own doc comment). Mark
            // inheritable whichever std slot(s) now differ from this
            // stage's own pre-spawn baseline (captured above, before
            // either the pipe-boundary swap or `redirect_stdio` touched
            // anything) — a slot still at its baseline is the shell's own
            // real stdio, untouched by this stage, and is never marked.
            let touched: Vec<rusty_win32::RawHandle> = [
                (crate::winstdio::STD_INPUT_HANDLE, prev_stdin),
                (crate::winstdio::STD_OUTPUT_HANDLE, prev_stdout),
                (crate::winstdio::STD_ERROR_HANDLE, prev_stderr),
            ]
            .into_iter()
            .filter_map(|(slot, baseline)| {
                let current = crate::winstdio::get(slot);
                (current != baseline && !current.is_null()).then_some(current)
            })
            .collect();
            for h in &touched {
                // SAFETY: `h` is whatever this stage's own pipe-boundary
                // swap or `redirect_stdio` just placed in a std slot —
                // freshly opened (a pipe end or a redirect target file),
                // valid, and open for the life of this spawn attempt.
                if let Err(e) = unsafe { rusty_win32::handle::set_inheritable(*h, true) } {
                    return Err(win_err(program, e));
                }
            }

            // SAFETY: `command_line` was built by `build_command_line`,
            // which quotes every argument (including the resolved program
            // path); `env_block` was built by `environment_block`, which
            // always double-NUL-terminates.
            let spawned = unsafe {
                rusty_win32::process::spawn_suspended(&command_line, true, Some(&env_block))
            };
            for h in &touched {
                // Best-effort: spawning is already over either way by this
                // point, and every handle here is either closed when
                // `guard` drops right below (a redirect target file, the
                // here-doc pipe's read end) or by the caller immediately
                // after this function returns (a pipeline-boundary pipe
                // end) — nothing depends on its inheritable flag surviving
                // past this one spawn attempt.
                unsafe {
                    let _ = rusty_win32::handle::set_inheritable(*h, false);
                }
            }
            drop(guard);
            spawned.map_err(|e| win_err(program, e))
        });

    crate::winstdio::set(crate::winstdio::STD_INPUT_HANDLE, prev_stdin);
    crate::winstdio::set(crate::winstdio::STD_OUTPUT_HANDLE, prev_stdout);
    result
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
    let newly_done: Vec<(u32, u32)> = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mut done = Vec::new();
        for j in &mut s.jobs {
            if j.exit_code.is_none() {
                // SAFETY: `j.process` is a valid, currently-open handle
                // this job entry owns exclusively until closed by
                // `reap_background`.
                if let Ok(Some(code)) = unsafe { rusty_win32::process::wait(j.process, Some(0)) } {
                    j.exit_code = Some(code);
                    done.push((j.pid, code));
                }
            }
        }
        done
    });
    REAPED.with(|r| {
        let mut r = r.borrow_mut();
        for (pid, code) in newly_done {
            r.insert(pid, code as i32);
        }
    });
}

/// Block on `process` (which must belong to job `pid`) until it exits,
/// recording the exit code in both the job table (if the entry is still
/// there) and [`REAPED`] (so a later `wait` on the same pid still reports
/// it even once the job table entry itself is gone).
fn wait_and_record(pid: u32, process: rusty_win32::RawHandle) -> i32 {
    // SAFETY: `process` is a valid, currently-open handle — callers look
    // it up from a live `JobEntry` (or, for `-n`, poll it before it's
    // pruned) immediately before calling this.
    let code = unsafe { rusty_win32::process::wait(process, None) }
        .ok()
        .flatten()
        .unwrap_or(1) as i32;
    STATE.with(|s| {
        if let Some(j) = s.borrow_mut().jobs.iter_mut().find(|j| j.pid == pid) {
            j.exit_code = Some(code as u32);
        }
    });
    REAPED.with(|r| {
        r.borrow_mut().insert(pid, code);
    });
    code
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

/// `wait [-n] [-p var] [pid|%job ...]` — block until the given background
/// jobs/pids (or, with none given, every one this shell knows about)
/// finish. With no operands this always succeeds (status 0); with one or
/// more, blocks on each in turn and reports the *last* one's own exit
/// status. Mirrors `job.rs::wait_cmd`'s argument handling; `-f` (accepted
/// there as a no-op, since `job.rs`'s own plain wait already does full
/// termination) has no Windows equivalent to matter for, so it's simply
/// not recognized here.
fn wait_cmd(argv: &[String]) -> i32 {
    let mut next = false;
    let mut pid_var: Option<String> = None;
    let mut targets: Vec<&String> = Vec::new();
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-n" => next = true,
            "-p" => match args.next() {
                Some(name) => pid_var = Some(name.clone()),
                None => {
                    eprintln!("wait: -p: option requires an argument");
                    return 2;
                }
            },
            _ => targets.push(arg),
        }
    }
    if let Some(name) = &pid_var {
        crate::vars::unset(name); // bash: unset until something is reaped
    }
    if next {
        let (pid, status) = wait_next();
        if let (Some(name), Some(pid)) = (&pid_var, pid) {
            crate::vars::set(name, &pid.to_string());
        }
        return status;
    }
    if targets.is_empty() {
        wait_all();
        return 0;
    }
    let mut status = 0;
    for target in &targets {
        status = wait_one(target);
    }
    status
}

/// `wait -n`: block until any currently-tracked job exits, record it, and
/// return its pid and exit status; 127 when there's nothing left to wait
/// for. Blocks on every not-yet-finished job's handle at once via
/// `rusty_win32::process::wait_any` (`WaitForMultipleObjects`) rather than
/// polling — the follow-up `docs/WINDOWS_JOB_CONTROL.md` flagged once that
/// wrapper existed. `WaitForMultipleObjects` caps at
/// [`rusty_win32::process::MAXIMUM_WAIT_OBJECTS`] (64) handles per call:
/// this shell's own job table is realistically never that large, but the
/// rare overflow still falls back to a short-sleep poll across sweeps
/// (the old, always-correct-if-coarser behavior) rather than silently
/// ignoring anything past the 64th tracked job.
fn wait_next() -> (Option<u32>, i32) {
    loop {
        let pending: Vec<(u32, rusty_win32::RawHandle)> = STATE.with(|s| {
            s.borrow()
                .jobs
                .iter()
                .filter(|j| j.exit_code.is_none())
                .map(|j| (j.pid, j.process))
                .collect()
        });
        if pending.is_empty() {
            return (None, 127);
        }
        let overflow = pending.len() > rusty_win32::process::MAXIMUM_WAIT_OBJECTS;
        let batch: Vec<rusty_win32::RawHandle> = pending
            .iter()
            .take(rusty_win32::process::MAXIMUM_WAIT_OBJECTS)
            .map(|(_, process)| *process)
            .collect();
        // A real timeout only in the overflow case, so a sweep that misses
        // the tail (jobs 65+) comes back around instead of blocking
        // forever on a batch that might never include the one that exits.
        let timeout = overflow.then_some(20);
        // SAFETY: every handle in `batch` was read from a live `JobEntry`
        // just above; nothing else closes a job's process handle except
        // `reap_background`, which only runs between prompts, not
        // concurrently with this loop (single-threaded shell).
        if let Ok(Some((index, code))) = unsafe { rusty_win32::process::wait_any(&batch, timeout) }
        {
            let code = code as i32;
            let pid = pending[index].0;
            STATE.with(|s| {
                if let Some(j) = s.borrow_mut().jobs.iter_mut().find(|j| j.pid == pid) {
                    j.exit_code = Some(code as u32);
                }
            });
            REAPED.with(|r| {
                r.borrow_mut().insert(pid, code);
            });
            return (Some(pid), code);
        }
    }
}

/// Block until every job this shell currently knows isn't finished has
/// finished, silently (unlike a completion notice, `wait` never prints one
/// itself — that's `reap_background`'s job, at the next prompt).
fn wait_all() {
    loop {
        let next = STATE.with(|s| {
            s.borrow()
                .jobs
                .iter()
                .find(|j| j.exit_code.is_none())
                .map(|j| (j.pid, j.process))
        });
        let Some((pid, process)) = next else { break };
        wait_and_record(pid, process);
    }
}

/// Wait for one `wait` operand — a `%job` spec or a bare pid — returning
/// its exit status (or a `wait`-specific error status if it names nothing
/// real).
fn wait_one(target: &str) -> i32 {
    if let Some(spec) = target.strip_prefix('%') {
        let found = spec.parse::<usize>().ok().and_then(|id| {
            STATE.with(|s| {
                s.borrow()
                    .jobs
                    .iter()
                    .find(|j| j.id == id)
                    .map(|j| (j.pid, j.process))
            })
        });
        let Some((pid, process)) = found else {
            eprintln!("wait: {target}: no such job");
            return 127;
        };
        return wait_and_record(pid, process);
    }

    let Ok(pid) = target.parse::<u32>() else {
        eprintln!("wait: `{target}': not a pid or valid job spec");
        return 1;
    };
    if let Some(code) = REAPED.with(|r| r.borrow().get(&pid).copied()) {
        return code;
    }
    let process = STATE.with(|s| {
        s.borrow()
            .jobs
            .iter()
            .find(|j| j.pid == pid)
            .map(|j| j.process)
    });
    let Some(process) = process else {
        eprintln!("wait: pid {pid} is not a child of this shell");
        return 127;
    };
    wait_and_record(pid, process)
}

/// `kill [-SIG|-s SIG] %job|pid ...` — terminate a tracked background job
/// via its Job Object, or an arbitrary pid via `OpenProcess`/`TerminateProcess`
/// (`rusty_win32::process::open_by_pid`/`terminate`). Windows has no real
/// signal delivery, so unlike `job.rs::kill_cmd`'s Unix counterpart the
/// requested signal name/number can't actually be honored beyond
/// "terminate it" — the flag is still accepted (so a script written for
/// portability, e.g. `kill -9 %1`, doesn't hard-error over a distinction
/// Windows genuinely can't make), but every kill reports the same
/// conventional exit code back through `wait`/`$?` (128 + 15, matching
/// what an ordinary `kill`-via-SIGTERM would report on Unix).
///
/// A bare pid need not be one of this shell's own tracked jobs: unlike
/// `wait`, which can only ever act on a child, POSIX `kill` can signal any
/// process the caller has permission to — `OpenProcess` (with the minimal
/// `PROCESS_TERMINATE` right) is what makes that possible here rather than
/// requiring a `%n`-tracked `JobEntry`.
fn kill_cmd(argv: &[String]) -> i32 {
    const KILLED_EXIT_CODE: u32 = 128 + 15;

    let mut start = 1;
    if argv.get(1).map(String::as_str) == Some("-s") {
        if argv.get(2).is_none() {
            eprintln!("kill: -s: option requires an argument");
            return 1;
        }
        start = 3;
    } else if argv.get(1).is_some_and(|a| a.starts_with('-')) {
        start = 2;
    }
    if argv.len() <= start {
        eprintln!("kill: usage: kill [-signal] %job|pid ...");
        return 1;
    }

    let mut status = 0;
    for target in &argv[start..] {
        if let Some(spec) = target.strip_prefix('%') {
            let Some(id) = spec.parse::<usize>().ok() else {
                eprintln!("kill: %{spec}: no such job");
                status = 1;
                continue;
            };
            let job = STATE.with(|s| s.borrow().jobs.iter().find(|j| j.id == id).map(|j| j.job));
            let Some(job) = job else {
                eprintln!("kill: %{id}: no such job");
                status = 1;
                continue;
            };
            // SAFETY: `job` is a valid handle this job entry still owns.
            if let Err(e) = unsafe { rusty_win32::job::terminate(job, KILLED_EXIT_CODE) } {
                eprintln!("kill: %{id}: {}", std::io::Error::from(e));
                status = 1;
            }
            continue;
        }

        let Ok(pid) = target.parse::<u32>() else {
            eprintln!("kill: {target}: arguments must be process or job IDs");
            status = 1;
            continue;
        };
        match rusty_win32::process::open_by_pid(pid, rusty_win32::process::PROCESS_TERMINATE) {
            Ok(handle) => {
                // SAFETY: `handle` was just opened above by `OpenProcess`
                // with `PROCESS_TERMINATE`, is valid, and is closed exactly
                // once below.
                let result = unsafe { rusty_win32::process::terminate(handle, KILLED_EXIT_CODE) };
                let _ = unsafe { rusty_win32::handle::close(handle) };
                if let Err(e) = result {
                    eprintln!("kill: ({pid}) - {}", std::io::Error::from(e));
                    status = 1;
                }
            }
            Err(e) => {
                eprintln!("kill: ({pid}) - {}", std::io::Error::from(e));
                status = 1;
            }
        }
    }
    status
}

/// `disown [%n|n]` — drop a job from the shell's job table (the most
/// recent not-yet-finished one when no spec is given, matching
/// `job.rs::disown_cmd`'s own `select_index` fallback) without
/// terminating it.
///
/// Unlike Unix — where a pid is already independent of anything the shell
/// holds, so "stop tracking it" is the *entire* operation — a Windows job
/// created with kill-on-close ties its member process's lifetime to the
/// job handle staying open in *this* process. Simply dropping the table
/// entry and closing the handles the way Unix `disown` conceptually does
/// would kill the process on the spot (or, if the handles were merely
/// leaked instead, when the shell itself later exits and the OS closes
/// them anyway) — the opposite of what `disown` is for. So this explicitly
/// reverses kill-on-close first (`rusty_win32::job::clear_kill_on_close`,
/// added specifically for this) before releasing the handles, which is
/// the actual "detach" operation on this platform.
///
/// **Known caveat**, confirmed via real Windows CI rather than assumed:
/// this only clears kill-on-close on the job *this shell created* for its
/// own tracking. If the shell's own process is itself already a member of
/// some *ambient* job (Windows automatically nests every child a job
/// member spawns into that same job too — not something a caller opts
/// into), a disowned process can still die once the shell's own process
/// exits, because that ambient job's own kill-on-close (if any) is
/// untouched by anything this function does. Environments that wrap a
/// process tree in such a job for their own cleanup purposes — GitHub
/// Actions' Windows runners, e.g. — will still tear down a "disowned" job
/// once the shell that spawned it exits. There's no portable way to
/// detect or opt out of this from inside the shell.
fn disown_cmd(argv: &[String]) -> i32 {
    let idx = STATE.with(|s| {
        let s = s.borrow();
        match argv.get(1) {
            Some(spec) => {
                let id: usize = spec.trim_start_matches('%').parse().ok()?;
                s.jobs.iter().position(|j| j.id == id)
            }
            None => s.jobs.iter().rposition(|j| j.exit_code.is_none()),
        }
    });
    let Some(idx) = idx else {
        eprintln!("disown: current: no such job");
        return 1;
    };
    let job = STATE.with(|s| s.borrow_mut().jobs.remove(idx));
    // SAFETY: `job.job` is a valid, currently-open Job Object handle this
    // entry exclusively owned until it was just removed from the table
    // above.
    if let Err(e) = unsafe { rusty_win32::job::clear_kill_on_close(job.job) } {
        eprintln!("disown: {}", std::io::Error::from(e));
        // Couldn't make it safe to detach — put it back rather than lose
        // track of (and leak) it silently.
        STATE.with(|s| s.borrow_mut().jobs.push(job));
        return 1;
    }
    // SAFETY: `job.process`/`job.job` are both valid, each closed exactly
    // once — safe now that kill-on-close no longer applies to either, and
    // a plain process handle never carries kill-on-close semantics of its
    // own regardless (closing it was always just dropping a reference).
    unsafe {
        let _ = rusty_win32::handle::close(job.process);
        let _ = rusty_win32::handle::close(job.job);
    }
    0
}

/// `jobs [-l|-p|-r|-s]` — matching `job.rs::jobs_cmd`'s flag set, except
/// `-n` (changed-since-last-notification only): that needs per-job
/// notified-state bookkeeping this module doesn't keep (every job here is
/// either still running or freshly finished and about to be pruned by the
/// next `reap_background`, so there's no persistent "already told you"
/// state to filter on the way `job.rs`'s does). `-s` (stopped only) always
/// prints nothing — Windows background jobs have no Stopped state (no
/// Ctrl-Z) — but is still accepted rather than rejected, for the same
/// portability reason `kill`'s signal flags are accepted without being
/// honorable.
fn jobs_cmd(argv: &[String]) -> i32 {
    let mut long = false;
    let mut pids_only = false;
    let mut running_only = false;
    let mut stopped_only = false;
    for arg in &argv[1..] {
        match arg.as_str() {
            "-l" => long = true,
            "-p" => pids_only = true,
            "-r" => running_only = true,
            "-s" => stopped_only = true,
            other => {
                eprintln!("jobs: {other}: invalid option");
                return 2;
            }
        }
    }
    refresh_all();
    STATE.with(|s| {
        for j in &s.borrow().jobs {
            let done = j.exit_code.is_some();
            if stopped_only || (running_only && done) {
                continue;
            }
            if pids_only {
                println!("{}", j.pid);
                continue;
            }
            let label = if done { "Done" } else { "Running" };
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
