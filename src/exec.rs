//! Execute a parsed command list.
//!
//! A [`CommandList`] is a sequence of pipelines joined by `;`/`&&`/`||`. Each
//! pipeline is expanded (variables, globs, …) *just before it runs*, left to
//! right, so a `cd` or future assignment takes effect for later pipelines. The
//! connector and the previous pipeline's exit status decide what runs next.
//!
//! Within a pipeline, builtins only run in-process when the pipeline is a
//! single command — a builtin in the middle of a pipe (`echo hi | cd`) is a
//! rare case we punt on for now. Everything else is spawned with
//! `std::process::Command`, wiring each stage's stdout into the next's stdin.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::process::{Child, Command as OsCommand, Stdio};

use crate::builtins;
use crate::parser::{CommandList, Connector, RawPipeline};

#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
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

/// Run a command list against the shell's own stdio, returning the exit status
/// of the last pipeline that actually ran.
pub fn run_list(list: &CommandList) -> Result<i32, String> {
    let mut status = run_one(&list.first, false)?.0;
    for (connector, raw) in &list.rest {
        if should_run(*connector, status) {
            status = run_one(raw, false)?.0;
        }
    }
    Ok(status)
}

/// Run a command list and return its stdout as a string — the engine behind
/// `$(...)` command substitution. The connector logic mirrors [`run_list`]; the
/// stdout of every pipeline that runs is concatenated.
pub fn capture_list(list: &CommandList) -> Result<String, String> {
    let mut out = String::new();
    let (mut status, s) = run_one(&list.first, true)?;
    out.push_str(&s);
    for (connector, raw) in &list.rest {
        if should_run(*connector, status) {
            let (st, s) = run_one(raw, true)?;
            status = st;
            out.push_str(&s);
        }
    }
    Ok(out)
}

fn should_run(connector: Connector, prev_status: i32) -> bool {
    match connector {
        Connector::Seq => true,
        Connector::And => prev_status == 0,
        Connector::Or => prev_status != 0,
    }
}

/// Expand a raw pipeline and run it, returning `(exit status, captured stdout)`.
/// The captured string is empty unless `capture` is set.
fn run_one(raw: &RawPipeline, capture: bool) -> Result<(i32, String), String> {
    let pipeline = crate::expand::expand(raw)?;

    // Single-command fast path: lets builtins run in the shell process. Builtins
    // are not specialised for capture, so a substitution sees external commands.
    if !capture && pipeline.commands.len() == 1 {
        if let Some(code) = builtins::try_run(&pipeline.commands[0].argv) {
            return Ok((code, String::new()));
        }
    }

    run(&pipeline, capture)
}

fn run(pipeline: &Pipeline, capture: bool) -> Result<(i32, String), String> {
    let n = pipeline.commands.len();
    let mut children: Vec<Child> = Vec::with_capacity(n);
    // Stdin for the next stage: the read end of the previous stage's pipe.
    let mut prev_stdout: Option<Stdio> = None;
    let mut captured = String::new();

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        let is_last = i == n - 1;

        let program = cmd
            .argv
            .first()
            .ok_or_else(|| "empty command".to_string())?;
        let mut command = OsCommand::new(program);
        command.args(&cmd.argv[1..]);

        // stdin: explicit `< file` wins, else the previous pipe, else inherit.
        if let Some(file) = stdin_redirect(&cmd.redirects) {
            let f = File::open(file).map_err(|e| format!("{file}: {e}"))?;
            command.stdin(Stdio::from(f));
        } else if let Some(prev) = prev_stdout.take() {
            command.stdin(prev);
        }

        // stdout: explicit `>`/`>>` wins; else pipe to the next stage; else, for
        // the last stage, pipe when capturing and otherwise inherit.
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

        let mut child = command
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;

        if !is_last {
            // Hand this child's stdout to the next iteration as its stdin.
            prev_stdout = child.stdout.take().map(Stdio::from);
        } else if capture {
            // Drain the final stage before waiting, so a full pipe can't wedge.
            if let Some(mut out) = child.stdout.take() {
                out.read_to_string(&mut captured).map_err(|e| e.to_string())?;
            }
        }
        children.push(child);
    }

    // Wait for every stage; the pipeline's status is the last stage's.
    let mut status = 0;
    for (i, mut child) in children.into_iter().enumerate() {
        let exit = child.wait().map_err(|e| e.to_string())?;
        if i == n - 1 {
            status = exit.code().unwrap_or(1);
        }
    }

    Ok((status, captured))
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
