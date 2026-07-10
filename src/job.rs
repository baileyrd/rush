//! Unix job control: process groups, terminal hand-off, and `fg`/`bg`/`jobs`.
//!
//! This module follows the structure of the classic glibc "Implementing a Job
//! Control Shell" example:
//!
//!   * At startup the shell ignores the job-control signals (`SIGINT`,
//!     `SIGTSTP`, …) and puts itself in its own process group that owns the
//!     terminal. Each child *resets* those signals to default and joins the
//!     job's process group before `exec`, so keystrokes like Ctrl-C / Ctrl-Z
//!     are delivered to the foreground job, not the shell.
//!   * A foreground job is handed the terminal (`tcsetpgrp`); the shell waits
//!     for it to exit *or stop* (`WUNTRACED`) and then reclaims the terminal.
//!   * A background job (`&`) is left in its own group without the terminal.
//!     Finished/stopped background jobs are reported at the next prompt.
//!
//! It is compiled only on Unix; the rest of the shell degrades to a plain
//! spawn-and-wait on other platforms.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::CommandExt;
use std::process::Stdio;

use libc::{c_int, pid_t};

use crate::exec::{CompoundStage, Pipeline, Stage};

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
    Done,
}

struct JobEntry {
    id: usize,
    pgid: pid_t,
    pids: Vec<pid_t>,
    live: usize,
    cmd: String,
    state: JobState,
    notified: bool,
}

#[derive(Default)]
struct State {
    shell_pgid: pid_t,
    job_control: bool,
    jobs: Vec<JobEntry>,
    next_id: usize,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    // Every pid this shell has itself reaped via `waitpid`, with its exit
    // code — so `wait` (C13) can still report a background job's status
    // even after something else (background polling, an earlier `wait`)
    // already reaped it; entries are never removed, matching bash's own
    // "waiting twice on the same pid still works" behavior (verified
    // against it directly).
    static REAPED: RefCell<HashMap<pid_t, i32>> = RefCell::new(HashMap::new());
}

/// The job-control signals the shell ignores and children reset to default.
const JOB_SIGNALS: [c_int; 5] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
];

/// Set up job control: only when stdin is a terminal. Idempotent enough to call
/// once at startup.
pub fn init() {
    let interactive = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
    let pid = unsafe { libc::getpid() };

    if interactive {
        unsafe {
            for &sig in &JOB_SIGNALS {
                libc::signal(sig, libc::SIG_IGN);
            }
            // Become a process-group leader and take the terminal.
            libc::setpgid(pid, pid);
            libc::tcsetpgrp(libc::STDIN_FILENO, pid);
        }
    }

    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.shell_pgid = pid;
        s.job_control = interactive;
    });
}

/// Spawn every stage of a pipeline into a single new process group, returning
/// `(pgid, pids)`. Children reset signal dispositions and join the group before
/// running; the parent also calls `setpgid` to avoid racing terminal hand-off.
///
/// The inter-stage connector is a real fd (`File`), not `Stdio`: a compound
/// stage (`if`/`while`/`(...)`/…) runs by forking rather than `exec`ing, and
/// a forked child needs an introspectable fd to `dup2` from, which `Stdio`
/// doesn't expose (it's built for handing to a `Command`, not reading back
/// out of). A `Simple` stage converts it to `Stdio` at the point it's fed
/// into `build_stage`.
/// What [`spawn_pipeline`] produced: either a real, live process group to
/// wait on, or — for a standalone (non-piped) command whose spawn failed
/// with an ordinary "not found"/"not executable" error (C37) — a status to
/// report directly, with no process at all behind it.
enum SpawnOutcome {
    Live { pgid: pid_t, pids: Vec<pid_t> },
    Immediate(i32),
}

fn spawn_pipeline(pipeline: &Pipeline) -> Result<SpawnOutcome, String> {
    let n = pipeline.commands.len();
    let mut pids = Vec::with_capacity(n);
    let mut pgid: pid_t = 0;
    let mut prev_stdin: Option<File> = None;

    for (i, stage) in pipeline.commands.iter().enumerate() {
        let is_last = i == n - 1;
        // `0` for the leader means "new group whose id is my pid" — computed
        // from `pgid`'s value *before* this iteration's update below, same as
        // the original single-branch version of this loop.
        let target_pgid = pgid;

        let pid = match stage {
            Stage::Simple(cmd) => {
                let (mut command, real_pipe_read) = crate::exec::build_stage(
                    cmd,
                    prev_stdin.take().map(Stdio::from),
                    is_last,
                    false,
                )?;

                unsafe {
                    command.pre_exec(move || {
                        libc::setpgid(0, target_pgid);
                        for &sig in &JOB_SIGNALS {
                            libc::signal(sig, libc::SIG_DFL);
                        }
                        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
                        Ok(())
                    });
                }

                let mut child = match command.spawn() {
                    Ok(c) => c,
                    // A standalone command (not one stage among several):
                    // no process group established yet, nothing else to
                    // unwind or wait for, so there's a real, simple status
                    // to report directly instead of aborting the script —
                    // matching real bash's own "command not found"/status
                    // 127 (or 126 for "found but couldn't run") rather than
                    // propagating the raw OS spawn error as a hard `Err`.
                    // A failing stage *within* a multi-command pipeline
                    // (`i > 0`, or more stages still to come) keeps today's
                    // existing behavior — an accepted, documented gap:
                    // unwinding an already-established process group and
                    // synthesizing a fake exit code mid-pipeline needs real
                    // architectural work this item's effort budget doesn't
                    // cover.
                    Err(e) if i == 0 && is_last => {
                        return Ok(SpawnOutcome::Immediate(crate::exec::spawn_failure_status(
                            &cmd.argv[0],
                            &e,
                        )));
                    }
                    Err(e) => return Err(format!("{}: {e}", cmd.argv[0])),
                };
                crate::exec::feed_heredoc(&mut child, cmd);
                let pid = child.id() as pid_t;

                prev_stdin = if let Some(read) = real_pipe_read {
                    // `2>&1` forced a real pipe (see `build_stage`): its read
                    // end feeds the next stage's stdin.
                    Some(read)
                } else if !is_last {
                    // SAFETY: `ChildStdout` uniquely owns this fd; taking it
                    // and rewrapping as a `File` only changes the type doing
                    // the owning, not the fd itself.
                    child.stdout.take().map(|s| unsafe { File::from_raw_fd(s.into_raw_fd()) })
                } else {
                    None
                };
                // We reap via `waitpid`, so let the std handle drop (its
                // Drop neither waits nor kills on Unix).
                pid
            }
            Stage::Compound(compound) => {
                let (pid, next_stdin) =
                    spawn_compound_stage(compound, prev_stdin.take(), is_last, target_pgid)?;
                prev_stdin = next_stdin;
                pid
            }
        };

        if i == 0 {
            pgid = pid;
        }
        unsafe {
            libc::setpgid(pid, pgid);
        }
        pids.push(pid);
    }

    Ok(SpawnOutcome::Live { pgid, pids })
}

/// Fork a compound command (`if`/`while`/`(...)`/…) as one stage of a real
/// pipeline. The child gets the same process-group and signal treatment as
/// an exec'd stage, so it behaves consistently under Ctrl-C/Ctrl-Z; its
/// stdin/stdout are wired via `dup2` from `stdin_src`/a freshly-made pipe, and
/// any redirects trailing the compound's own close (`(cmd) < file | grep x`)
/// are applied *after* that baseline — same precedence `build_stage` uses —
/// via the same `redirect_stdio` a lone builtin or a whole compound run
/// in-process uses, just never restored (this child exits right after
/// running the compound, so there's nothing to give the fds back to).
/// Returns `(pid, next_stdin)`: the child's pid, and — if `!is_last` — the
/// read end of a pipe the next stage should use as its own stdin.
fn spawn_compound_stage(
    stage: &CompoundStage,
    stdin_src: Option<File>,
    is_last: bool,
    target_pgid: pid_t,
) -> Result<(pid_t, Option<File>), String> {
    let next_pipe = if is_last { None } else { Some(crate::exec::make_pipe()?) };

    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error().to_string()),
        0 => {
            unsafe {
                libc::setpgid(0, target_pgid);
                for &sig in &JOB_SIGNALS {
                    libc::signal(sig, libc::SIG_DFL);
                }
                libc::signal(libc::SIGCHLD, libc::SIG_DFL);
            }
            if let Some(stdin) = &stdin_src {
                unsafe {
                    libc::dup2(stdin.as_raw_fd(), 0);
                }
            }
            if let Some((_, write)) = &next_pipe {
                unsafe {
                    libc::dup2(write.as_raw_fd(), 1);
                }
            }
            drop(stdin_src);
            drop(next_pipe);
            match crate::exec::redirect_stdio(&stage.redirects, stage.heredoc.as_deref()) {
                Ok(guard) => std::mem::forget(guard),
                Err(e) => {
                    eprintln!("rush: {e}");
                    crate::trap::exit_shell(1);
                }
            }
            let status = crate::exec::run_compound(&stage.compound).unwrap_or(1);
            crate::trap::exit_shell(status);
        }
        pid => {
            let next_stdin = next_pipe.map(|(read, _)| read);
            Ok((pid, next_stdin))
        }
    }
}

/// Run a pipeline in the foreground, returning its exit status. If it stops
/// (Ctrl-Z), it is added to the job table and we return `128 + SIGTSTP`.
pub fn run_foreground(pipeline: &Pipeline) -> Result<i32, String> {
    let (pgid, pids) = match spawn_pipeline(pipeline)? {
        // A standalone command that failed to spawn (C37) — already
        // reported, nothing to wait for.
        SpawnOutcome::Immediate(status) => return Ok(status),
        SpawnOutcome::Live { pgid, pids } => (pgid, pids),
    };
    give_terminal(pgid);

    let result = wait_pgid(pgid, &pids);
    reclaim_terminal();

    Ok(match result {
        Wait::Done(code) => code,
        Wait::Stopped(code) => {
            let cmd = crate::exec::pipeline_text(pipeline);
            let id = add_job(pgid, &pids, &cmd, JobState::Stopped);
            eprintln!("\n[{id}]+  Stopped\t{cmd}");
            code
        }
    })
}

/// Run a pipeline in the background: record it and print `[id] pgid`.
pub fn run_background(pipeline: &Pipeline) -> Result<(), String> {
    let (pgid, pids) = match spawn_pipeline(pipeline)? {
        // A standalone command that failed to spawn (C37) — already
        // reported. Real bash still gets a real, if short-lived, pid here
        // (it forks unconditionally, and only the exec step inside that
        // child can fail); Rust's `Command::spawn` hides that distinction
        // entirely and reports the failure atomically with no pid exposed
        // at all, so there's nothing to give `$!`/`jobs` here — an
        // accepted, documented gap. The script still isn't aborted, which
        // is the actual headline bug this item fixes.
        SpawnOutcome::Immediate(_) => return Ok(()),
        SpawnOutcome::Live { pgid, pids } => (pgid, pids),
    };
    let cmd = crate::exec::pipeline_text(pipeline);
    let id = add_job(pgid, &pids, &cmd, JobState::Running);
    // `$!` is the *last* stage's own pid (verified against real bash
    // directly), not the pgid — for a single-command background job
    // they're the same pid anyway, since the leader is that one process.
    crate::vars::set_last_bg_pid(*pids.last().expect("pipeline has at least one stage"));
    // Only announced interactively — a non-interactive script (`bash -c`,
    // a file) prints nothing here, matching real bash.
    if job_control_enabled() {
        println!("[{id}] {pgid}");
    }
    Ok(())
}

enum Wait {
    Done(i32),
    Stopped(i32),
}

/// Wait for a process group to finish or stop, reaping its members.
fn wait_pgid(pgid: pid_t, pids: &[pid_t]) -> Wait {
    let mut live: usize = pids.len();
    // Per-stage exit codes, in pipeline order — `set -o pipefail` needs all
    // of them, not just the last stage's (see `exec::pipeline_status`).
    let mut codes = vec![0; pids.len()];

    while live > 0 {
        let mut status: c_int = 0;
        let wpid = unsafe { libc::waitpid(-pgid, &mut status, libc::WUNTRACED) };
        if wpid == -1 {
            if retry_after_interrupt() {
                continue;
            }
            break; // ECHILD: nothing left to wait for
        }
        if wpid == 0 {
            break;
        }
        if wifstopped(status) {
            return Wait::Stopped(128 + libc::WSTOPSIG(status) as i32);
        }
        // Exited or killed by a signal.
        live -= 1;
        if let Some(i) = pids.iter().position(|&p| p == wpid) {
            codes[i] = exit_code(status);
        }
    }

    Wait::Done(crate::exec::pipeline_status(&codes))
}

/// `true` if the just-failed syscall was interrupted by a caught signal
/// (`EINTR`) rather than a real error — and, if so, handles whatever
/// TERM/HUP trap prompted it before reporting "go ahead and retry". This is
/// what makes a trap fire *immediately*, mid-wait, instead of only once the
/// foreground job finishes on its own — verified directly against real
/// bash: if the trap doesn't itself exit, the wait simply resumes, exactly
/// like this call site does.
fn retry_after_interrupt() -> bool {
    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
        return false;
    }
    crate::trap::check_pending();
    true
}

/// Reap finished/stopped/continued background jobs without blocking, reporting
/// state changes. Called once before each prompt.
pub fn reap_background() {
    if !job_control_enabled() {
        return;
    }

    loop {
        let mut status: c_int = 0;
        let flags = libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED;
        let wpid = unsafe { libc::waitpid(-1, &mut status, flags) };
        if wpid <= 0 {
            break; // 0: no change; -1: no children
        }
        update_by_pid(wpid, status);
    }

    notify_and_prune();
}

// ---- builtins: jobs / fg / bg ------------------------------------------------

/// Names dispatched by `builtin`, for `builtins::is_builtin`/`all_names`.
pub(crate) const NAMES: &[&str] = &["jobs", "fg", "bg", "kill", "wait"];

pub(crate) fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Dispatch the job-control builtins. Returns `Some(code)` if handled.
pub fn builtin(argv: &[String]) -> Option<i32> {
    match argv.first().map(String::as_str)? {
        "jobs" => Some(jobs_cmd()),
        "fg" => Some(fg_cmd(argv)),
        "bg" => Some(bg_cmd(argv)),
        "kill" => Some(kill_cmd(argv)),
        "wait" => Some(wait_cmd(argv)),
        _ => None,
    }
}

/// `wait [pid|%job ...]` — block until the given background jobs/pids (or,
/// with none given, every one this shell knows about) finish. With no
/// operands this always succeeds (status 0, POSIX's rule); with one or
/// more, blocks on each in turn and reports the *last* one's own exit
/// status. A pid/job already reaped — by an earlier `wait`, by `fg`, or by
/// the interactive prompt's own background polling — still reports its
/// remembered status (`REAPED`) rather than erroring.
fn wait_cmd(argv: &[String]) -> i32 {
    if argv.len() == 1 {
        wait_all();
        return 0;
    }
    let mut status = 0;
    for target in &argv[1..] {
        status = wait_one(target);
    }
    status
}

/// Block until every job this shell currently knows isn't finished has
/// finished, silently (unlike `fg`, `wait` never takes the terminal or
/// prints a completion notice — that's the interactive prompt's own job).
fn wait_all() {
    loop {
        let pgid =
            STATE.with(|s| s.borrow().jobs.iter().find(|j| j.state != JobState::Done).map(|j| j.pgid));
        let Some(pgid) = pgid else { break };
        wait_job_pgid(pgid);
    }
}

/// Block until every pid in process group `pgid` has exited, recording each
/// one's exit code and updating the job table as it goes — like
/// `wait_pgid`, but doesn't stop early on a stop signal (`wait` isn't
/// interactive job control) and returns nothing (callers look the exit
/// code up from `REAPED` afterward, since the *pid* being waited on may
/// differ from the pgid, e.g. `wait` on a specific pid within a piped job).
fn wait_job_pgid(pgid: pid_t) {
    loop {
        let mut status: c_int = 0;
        let wpid = unsafe { libc::waitpid(-pgid, &mut status, 0) };
        if wpid == -1 {
            if retry_after_interrupt() {
                continue;
            }
            break; // ECHILD: nothing left in this group to reap
        }
        if wpid == 0 {
            break;
        }
        update_by_pid(wpid, status);
    }
}

/// Wait for one `wait` operand — a `%job` spec or a bare pid — returning
/// its exit status (or a `wait`-specific error status if it names nothing
/// real).
fn wait_one(target: &str) -> i32 {
    if let Some(spec) = target.strip_prefix('%') {
        let Some(pgid) = spec.parse::<usize>().ok().and_then(job_pgid) else {
            eprintln!("wait: {target}: no such job");
            return 127;
        };
        let last_pid =
            STATE.with(|s| s.borrow().jobs.iter().find(|j| j.pgid == pgid).and_then(|j| j.pids.last().copied()));
        wait_job_pgid(pgid);
        return last_pid.and_then(|p| REAPED.with(|r| r.borrow().get(&p).copied())).unwrap_or(0);
    }

    let Ok(pid) = target.parse::<pid_t>() else {
        eprintln!("wait: `{target}': not a pid or valid job spec");
        return 1;
    };
    if let Some(code) = REAPED.with(|r| r.borrow().get(&pid).copied()) {
        return code;
    }
    let mut status: c_int = 0;
    let wpid = loop {
        let wpid = unsafe { libc::waitpid(pid, &mut status, 0) };
        if wpid == -1 && retry_after_interrupt() {
            continue;
        }
        break wpid;
    };
    if wpid <= 0 {
        eprintln!("wait: pid {pid} is not a child of this shell");
        return 127;
    }
    update_by_pid(wpid, status);
    exit_code(status)
}

/// `kill [-SIG] %job|pid …` — signal a job (by `%n`) or process. The default
/// signal is `TERM`; `-9`, `-KILL`, `-SIGKILL`, etc. are accepted.
fn kill_cmd(argv: &[String]) -> i32 {
    let mut sig = libc::SIGTERM;
    let mut start = 1;
    if let Some(first) = argv.get(1).and_then(|a| a.strip_prefix('-')) {
        match parse_signal(first) {
            Some(s) => {
                sig = s;
                start = 2;
            }
            None => {
                eprintln!("kill: {first}: invalid signal specification");
                return 1;
            }
        }
    }
    if argv.len() <= start {
        eprintln!("kill: usage: kill [-signal] %job | pid ...");
        return 1;
    }

    let mut status = 0;
    for target in &argv[start..] {
        if let Some(spec) = target.strip_prefix('%') {
            match spec.parse::<usize>().ok().and_then(job_pgid) {
                Some(pgid) => unsafe {
                    libc::killpg(pgid, sig);
                },
                None => {
                    eprintln!("kill: %{spec}: no such job");
                    status = 1;
                }
            }
        } else if let Ok(pid) = target.parse::<pid_t>() {
            unsafe {
                libc::kill(pid, sig);
            }
        } else {
            eprintln!("kill: {target}: arguments must be job or process IDs");
            status = 1;
        }
    }
    status
}

fn job_pgid(id: usize) -> Option<pid_t> {
    STATE.with(|s| s.borrow().jobs.iter().find(|j| j.id == id).map(|j| j.pgid))
}

/// Every current job's own id — for completion (`%n` job-spec arguments to
/// `fg`/`bg`/`kill`/`wait`), matching the plain `%N` format those builtins
/// themselves parse (see `select_index`/`wait_one`/`kill_cmd`).
pub fn ids() -> Vec<usize> {
    STATE.with(|s| s.borrow().jobs.iter().map(|j| j.id).collect())
}

fn parse_signal(name: &str) -> Option<c_int> {
    if let Ok(n) = name.parse::<c_int>() {
        return Some(n);
    }
    let upper = name.to_ascii_uppercase();
    match upper.strip_prefix("SIG").unwrap_or(&upper) {
        "TERM" => Some(libc::SIGTERM),
        "KILL" => Some(libc::SIGKILL),
        "INT" => Some(libc::SIGINT),
        "HUP" => Some(libc::SIGHUP),
        "QUIT" => Some(libc::SIGQUIT),
        "STOP" => Some(libc::SIGSTOP),
        "CONT" => Some(libc::SIGCONT),
        _ => None,
    }
}

fn jobs_cmd() -> i32 {
    STATE.with(|s| {
        for j in &s.borrow().jobs {
            println!("[{}]  {}\t{}", j.id, state_label(j.state), j.cmd);
        }
    });
    0
}

fn fg_cmd(argv: &[String]) -> i32 {
    let mut job = match take_selected(argv) {
        Some(j) => j,
        None => {
            eprintln!("fg: no current job");
            return 1;
        }
    };

    println!("{}", job.cmd);
    give_terminal(job.pgid);
    unsafe {
        libc::killpg(job.pgid, libc::SIGCONT);
    }

    let result = wait_pgid(job.pgid, &job.pids);
    reclaim_terminal();

    match result {
        Wait::Done(code) => code,
        Wait::Stopped(code) => {
            job.state = JobState::Stopped;
            eprintln!("\n[{}]+  Stopped\t{}", job.id, job.cmd);
            reinsert(job);
            code
        }
    }
}

fn bg_cmd(argv: &[String]) -> i32 {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let idx = match select_index(&s, argv) {
            Some(i) => i,
            None => {
                eprintln!("bg: no current job");
                return 1;
            }
        };
        let job = &mut s.jobs[idx];
        job.state = JobState::Running;
        unsafe {
            libc::killpg(job.pgid, libc::SIGCONT);
        }
        println!("[{}] {} &", job.id, job.cmd);
        0
    })
}

// ---- job table helpers -------------------------------------------------------

fn add_job(pgid: pid_t, pids: &[pid_t], cmd: &str, state: JobState) -> usize {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.next_id += 1;
        let id = s.next_id;
        s.jobs.push(JobEntry {
            id,
            pgid,
            pids: pids.to_vec(),
            live: pids.len(),
            cmd: cmd.to_string(),
            state,
            notified: false,
        });
        id
    })
}

fn reinsert(job: JobEntry) {
    STATE.with(|s| s.borrow_mut().jobs.push(job));
}

/// Remove and return the job selected by `argv` (a `%n`/`n` spec, or the most
/// recent job when no spec is given).
fn take_selected(argv: &[String]) -> Option<JobEntry> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let idx = select_index(&s, argv)?;
        Some(s.jobs.remove(idx))
    })
}

fn select_index(s: &State, argv: &[String]) -> Option<usize> {
    match argv.get(1) {
        Some(spec) => {
            let n: usize = spec.trim_start_matches('%').parse().ok()?;
            s.jobs.iter().position(|j| j.id == n)
        }
        // Most recent job that isn't already finished.
        None => s.jobs.iter().rposition(|j| j.state != JobState::Done),
    }
}

fn update_by_pid(wpid: pid_t, status: c_int) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(job) = s.jobs.iter_mut().find(|j| j.pids.contains(&wpid)) else {
            return;
        };
        if wifstopped(status) {
            job.state = JobState::Stopped;
            job.notified = false;
        } else if wifcontinued(status) {
            job.state = JobState::Running;
        } else {
            // Exited or signaled.
            job.live = job.live.saturating_sub(1);
            REAPED.with(|r| {
                r.borrow_mut().insert(wpid, exit_code(status));
            });
            if job.live == 0 {
                job.state = JobState::Done;
            }
        }
    });
}

fn notify_and_prune() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for j in &mut s.jobs {
            if j.state == JobState::Done && !j.notified {
                eprintln!("[{}]  Done\t{}", j.id, j.cmd);
                j.notified = true;
            }
        }
        s.jobs.retain(|j| j.state != JobState::Done);
    });
}

// ---- terminal + status helpers ----------------------------------------------

fn give_terminal(pgid: pid_t) {
    if job_control_enabled() {
        unsafe {
            // SIGTTOU is ignored in the shell, so this never stops us.
            libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
        }
    }
}

fn reclaim_terminal() {
    let shell_pgid = STATE.with(|s| s.borrow().shell_pgid);
    give_terminal(shell_pgid);
}

pub(crate) fn job_control_enabled() -> bool {
    STATE.with(|s| s.borrow().job_control)
}

fn state_label(state: JobState) -> &'static str {
    match state {
        JobState::Running => "Running",
        JobState::Stopped => "Stopped",
        JobState::Done => "Done",
    }
}

pub(crate) fn exit_code(status: c_int) -> i32 {
    if wifexited(status) {
        libc::WEXITSTATUS(status)
    } else if wifsignaled(status) {
        128 + libc::WTERMSIG(status) as i32
    } else {
        0
    }
}

// libc exposes the wait-status macros as functions; thin wrappers keep the call
// sites readable and centralise the `c_int` plumbing.
fn wifexited(status: c_int) -> bool {
    libc::WIFEXITED(status)
}
fn wifsignaled(status: c_int) -> bool {
    libc::WIFSIGNALED(status)
}
fn wifstopped(status: c_int) -> bool {
    libc::WIFSTOPPED(status)
}
fn wifcontinued(status: c_int) -> bool {
    libc::WIFCONTINUED(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Command;

    fn cmd(argv: &[&str]) -> Stage {
        // Seed `vars`'s own `PATH` from the real one: production code never
        // needs this (`main.rs` seeds every inherited environment variable
        // at startup, C36), but these tests spawn real processes directly,
        // bypassing `main()` entirely — and `exec::resolve_program` (C40)
        // now resolves a bare command name via `vars::get("PATH")` only, no
        // `std::env` fallback, so a real spawn here needs this done by hand.
        if crate::vars::get("PATH").is_none()
            && let Ok(path) = std::env::var("PATH")
        {
            crate::vars::set_exported("PATH", &path);
        }
        Stage::Simple(Command {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            redirects: vec![],
            assignments: vec![],
            heredoc: None,
            local_decls: vec![],
        })
    }

    // None of these call `init()` with a real tty (there isn't one under
    // `cargo test`), so `job_control_enabled()` stays false throughout —
    // `give_terminal`/`reclaim_terminal` are no-ops, same as running rush
    // non-interactively (`-c`/a script). That's exactly the path these
    // exercise: spawn, wire process groups, wait, decode the status.

    #[test]
    fn foreground_single_command_reports_exit_status() {
        let pipeline = Pipeline { commands: vec![cmd(&["true"])] };
        assert_eq!(run_foreground(&pipeline).unwrap(), 0);

        let pipeline = Pipeline { commands: vec![cmd(&["false"])] };
        assert_eq!(run_foreground(&pipeline).unwrap(), 1);
    }

    #[test]
    fn foreground_pipeline_reports_last_stage_status() {
        let pipeline = Pipeline {
            commands: vec![cmd(&["false"]), cmd(&["true"])],
        };
        // Every stage still runs (no short-circuiting within a pipeline);
        // only the last stage's status is the pipeline's status.
        assert_eq!(run_foreground(&pipeline).unwrap(), 0);
    }

    #[test]
    fn foreground_pipeline_reports_signal_death() {
        // A child killed by a signal reports the conventional 128+signal
        // code — exercises `exit_code`'s `wifsignaled` branch via a real
        // signaled process, not a hand-encoded status.
        let pipeline = Pipeline { commands: vec![cmd(&["sh", "-c", "kill -TERM $$"])] };
        assert_eq!(run_foreground(&pipeline).unwrap(), 128 + 15);
    }

    #[test]
    fn job_table_tracks_and_prunes_finished_jobs() {
        // `run_background` is safe to call directly (it doesn't gate on
        // `job_control_enabled()`); but its own `reap_background` does (real
        // job control needs a tty to hand the terminal back to), so it would
        // silently no-op here. Drive the same underlying bookkeeping
        // (`update_by_pid`/`notify_and_prune`) directly instead.
        let pipeline = Pipeline { commands: vec![cmd(&["true"])] };
        run_background(&pipeline).unwrap();

        let pid = STATE.with(|s| s.borrow().jobs.last().unwrap().pids[0]);
        let mut status: c_int = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        assert_eq!(waited, pid, "the background child should still be ours to reap");
        update_by_pid(pid, status);
        notify_and_prune();

        let still_present = STATE.with(|s| s.borrow().jobs.iter().any(|j| j.pids.contains(&pid)));
        assert!(!still_present, "a finished job should be pruned after notify_and_prune");
    }
}
