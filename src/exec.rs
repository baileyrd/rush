//! Execute a parsed command list.
//!
//! A [`CommandList`] is a sequence of jobs separated by `;`/`&`. Each job is an
//! and-or chain of pipelines (`&&`/`||`); a job marked `background` runs without
//! blocking the shell. Every pipeline is expanded (variables, globs, …) *just
//! before it runs*, left to right, so a `cd` takes effect for later pipelines.
//!
//! On Unix, foreground and background pipelines go through [`crate::job`], which
//! adds process groups, terminal control, and stop/`fg`/`bg` handling. On other
//! platforms there is no job control: foreground pipelines run with a plain
//! spawn-and-wait, and `&` is rejected.
//!
//! Within a pipeline, builtins only run in-process when the pipeline is a
//! single command — a builtin in the middle of a pipe (`echo hi | cd`) is a
//! rare case we punt on for now.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::process::{Child, Command as OsCommand, Stdio};

use crate::builtins;
use crate::parser::{AndOrList, CommandList, Compound, Connector, Job, RawCommand, RawPipeline};

#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
    /// Leading `NAME=value` assignments. With no `argv` they set shell variables;
    /// otherwise they apply to this command's environment only.
    pub assignments: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum Redirect {
    /// `< file`
    Stdin(String),
    /// `> file` (truncate) or `>> file` (append)
    Stdout { file: String, append: bool },
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<Command>,
}

/// Run a whole command line, returning the exit status of the last foreground
/// job that ran. A `break`/`continue` that escapes all loops is discarded here.
pub fn run_list(list: &CommandList) -> Result<i32, String> {
    let status = exec_list(list)?;
    crate::vars::set_loop_ctl(None);
    Ok(status)
}

/// Run a list, stopping early if a `break`/`continue` becomes pending — used for
/// both the top level and the bodies of compound commands.
fn exec_list(list: &CommandList) -> Result<i32, String> {
    let mut status = 0;
    for job in &list.jobs {
        status = run_job(job)?;
        if crate::vars::loop_ctl().is_some() {
            break;
        }
    }
    Ok(status)
}

fn run_job(job: &Job) -> Result<i32, String> {
    if job.background {
        // Backgrounding an `&&`/`||` list would need a subshell; we support the
        // common case of a single (possibly piped) command.
        if !job.list.rest.is_empty() {
            return Err("background '&&'/'||' lists are not supported".into());
        }
        let pipeline = crate::expand::expand(&job.list.first)?;
        run_background(&pipeline)?;
        crate::vars::set_last_status(0);
        Ok(0)
    } else {
        run_andor(&job.list)
    }
}

fn run_andor(list: &AndOrList) -> Result<i32, String> {
    // Update `$?` after every pipeline, so a later one in the same line can read
    // it (e.g. `false || echo $?`).
    let mut status = run_pipeline_node(&list.first)?;
    crate::vars::set_last_status(status);
    if crate::vars::loop_ctl().is_some() {
        return Ok(status);
    }
    for (connector, raw) in &list.rest {
        if should_run(*connector, status) {
            status = run_pipeline_node(raw)?;
            crate::vars::set_last_status(status);
            if crate::vars::loop_ctl().is_some() {
                break;
            }
        }
    }
    Ok(status)
}

/// A pipeline that is a single compound command (`if`/`while`/`for`) is run
/// directly; everything else goes through the simple-command path.
fn run_pipeline_node(raw: &RawPipeline) -> Result<i32, String> {
    if let [RawCommand::Compound(compound)] = raw.commands.as_slice() {
        return run_compound(compound);
    }
    run_foreground(raw)
}

fn run_compound(compound: &Compound) -> Result<i32, String> {
    match compound {
        Compound::If { branches, else_body } => {
            for (cond, body) in branches {
                if exec_list(cond)? == 0 {
                    return exec_list(body);
                }
            }
            match else_body {
                Some(body) => exec_list(body),
                None => Ok(0),
            }
        }
        Compound::Loop { until, cond, body } => {
            let mut status = 0;
            loop {
                let met = exec_list(cond)? == 0;
                if met == *until {
                    break; // while: stop when not met; until: stop when met
                }
                status = exec_list(body)?;
                if loop_step()? {
                    break;
                }
            }
            Ok(status)
        }
        Compound::For { var, words, body } => {
            let mut status = 0;
            for value in crate::expand::expand_words(words)? {
                crate::vars::set(var, &value);
                status = exec_list(body)?;
                if loop_step()? {
                    break;
                }
            }
            Ok(status)
        }
    }
}

/// After running a loop body, consume one level of any pending `break`/
/// `continue`. Returns `true` if this loop should stop iterating.
fn loop_step() -> Result<bool, String> {
    use crate::vars::LoopCtl;
    match crate::vars::loop_ctl() {
        None => Ok(false),
        Some(LoopCtl::Continue(1)) => {
            crate::vars::set_loop_ctl(None);
            Ok(false) // keep looping
        }
        Some(LoopCtl::Break(1)) => {
            crate::vars::set_loop_ctl(None);
            Ok(true)
        }
        // `break N` / `continue N` for an outer loop: drop a level and stop this
        // one, leaving the request pending for the enclosing loop to handle.
        Some(LoopCtl::Break(n)) => {
            crate::vars::set_loop_ctl(Some(LoopCtl::Break(n - 1)));
            Ok(true)
        }
        Some(LoopCtl::Continue(n)) => {
            crate::vars::set_loop_ctl(Some(LoopCtl::Continue(n - 1)));
            Ok(true)
        }
    }
}

fn should_run(connector: Connector, prev_status: i32) -> bool {
    match connector {
        Connector::And => prev_status == 0,
        Connector::Or => prev_status != 0,
    }
}

/// A single command that is only `NAME=value` assignments (no program word):
/// `FOO=bar`. These set shell variables rather than spawning anything.
fn assignment_only(pipeline: &Pipeline) -> bool {
    pipeline.commands.len() == 1
        && pipeline.commands[0].argv.is_empty()
        && !pipeline.commands[0].assignments.is_empty()
}

fn apply_assignments(pipeline: &Pipeline) {
    for (name, value) in &pipeline.commands[0].assignments {
        crate::vars::set(name, value);
    }
}

/// Expand and run a single pipeline in the foreground.
fn run_foreground(raw: &RawPipeline) -> Result<i32, String> {
    let pipeline = crate::expand::expand(raw)?;

    if assignment_only(&pipeline) {
        apply_assignments(&pipeline);
        return Ok(0);
    }

    if pipeline.commands.len() == 1 {
        if let Some(code) = builtins::try_run(&pipeline.commands[0].argv) {
            return Ok(code);
        }
    }

    #[cfg(unix)]
    {
        crate::job::run_foreground(&pipeline)
    }
    #[cfg(not(unix))]
    {
        run(&pipeline, false).map(|(status, _)| status)
    }
}

/// Run an already-expanded pipeline in the background. Unix only.
#[cfg(unix)]
fn run_background(pipeline: &Pipeline) -> Result<(), String> {
    crate::job::run_background(pipeline)
}
#[cfg(not(unix))]
fn run_background(_pipeline: &Pipeline) -> Result<(), String> {
    Err("background jobs are not supported on this platform".into())
}

/// Run a command list and return its stdout — the engine behind `$(...)`.
/// Substitutions are synchronous: every job runs in the foreground with a plain
/// spawn-and-wait (no job control), and the `&` background marker is ignored.
pub fn capture_list(list: &CommandList) -> Result<String, String> {
    let mut out = String::new();
    for job in &list.jobs {
        capture_andor(&job.list, &mut out)?;
    }
    Ok(out)
}

fn capture_andor(list: &AndOrList, out: &mut String) -> Result<i32, String> {
    let mut status = capture_pipeline(&list.first, out)?;
    for (connector, raw) in &list.rest {
        if should_run(*connector, status) {
            status = capture_pipeline(raw, out)?;
        }
    }
    Ok(status)
}

fn capture_pipeline(raw: &RawPipeline, out: &mut String) -> Result<i32, String> {
    let pipeline = crate::expand::expand(raw)?;
    if assignment_only(&pipeline) {
        apply_assignments(&pipeline);
        return Ok(0);
    }
    let (status, captured) = run(&pipeline, true)?;
    out.push_str(&captured);
    Ok(status)
}

/// Plain spawn-and-wait runner: used for capture, and as the foreground runner
/// on non-Unix platforms. Returns `(exit status, captured stdout)`; the string
/// is empty unless `capture` is set.
fn run(pipeline: &Pipeline, capture: bool) -> Result<(i32, String), String> {
    let n = pipeline.commands.len();
    let mut children: Vec<Child> = Vec::with_capacity(n);
    // Stdin for the next stage: the read end of the previous stage's pipe.
    let mut prev_stdout: Option<Stdio> = None;
    let mut captured = String::new();

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        let is_last = i == n - 1;
        let mut command = build_stage(cmd, prev_stdout.take(), is_last, capture)?;

        let mut child = command
            .spawn()
            .map_err(|e| format!("{}: {e}", cmd.argv[0]))?;

        if !is_last {
            prev_stdout = child.stdout.take().map(Stdio::from);
        } else if capture {
            if let Some(mut out) = child.stdout.take() {
                out.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            }
        }
        children.push(child);
    }

    let mut status = 0;
    for (i, mut child) in children.into_iter().enumerate() {
        let exit = child.wait().map_err(|e| e.to_string())?;
        if i == n - 1 {
            status = exit.code().unwrap_or(1);
        }
    }

    Ok((status, captured))
}

/// Build the `std::process::Command` for one pipeline stage: program, args, and
/// stdio. An explicit `<`/`>`/`>>` redirect wins over pipe wiring; otherwise a
/// non-final stage (or any stage when capturing) gets a piped stdout. Shared by
/// the plain runner and the Unix job runner.
pub(crate) fn build_stage(
    cmd: &Command,
    stdin_src: Option<Stdio>,
    is_last: bool,
    capture: bool,
) -> Result<OsCommand, String> {
    let program = cmd
        .argv
        .first()
        .ok_or_else(|| "empty command".to_string())?;
    let mut command = OsCommand::new(program);
    command.args(&cmd.argv[1..]);

    // Seed the environment: exported shell variables first, then this command's
    // own `NAME=value` prefixes (which override).
    command.envs(crate::vars::exported());
    command.envs(cmd.assignments.iter().map(|(k, v)| (k, v)));

    // stdin: explicit `< file` wins, else the previous pipe, else inherit.
    if let Some(file) = stdin_redirect(&cmd.redirects) {
        let f = File::open(file).map_err(|e| format!("{file}: {e}"))?;
        command.stdin(Stdio::from(f));
    } else if let Some(src) = stdin_src {
        command.stdin(src);
    }

    // stdout: explicit `>`/`>>` wins; else pipe to the next stage / capture buffer.
    if let Some((file, append)) = stdout_redirect(&cmd.redirects) {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(file)
            .map_err(|e| format!("{file}: {e}"))?;
        command.stdout(Stdio::from(f));
    } else if !is_last || capture {
        command.stdout(Stdio::piped());
    }

    Ok(command)
}

/// A human-readable rendering of a pipeline, for the `jobs` listing. Only the
/// Unix job runner uses it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn pipeline_text(pipeline: &Pipeline) -> String {
    pipeline
        .commands
        .iter()
        .map(|c| c.argv.join(" "))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn stdin_redirect(redirects: &[Redirect]) -> Option<&str> {
    redirects.iter().rev().find_map(|r| match r {
        Redirect::Stdin(f) => Some(f.as_str()),
        _ => None,
    })
}

fn stdout_redirect(redirects: &[Redirect]) -> Option<(&str, bool)> {
    redirects.iter().rev().find_map(|r| match r {
        Redirect::Stdout { file, append } => Some((file.as_str(), *append)),
        _ => None,
    })
}
