//! Execute a parsed pipeline.
//!
//! Builtins only run in-process when the pipeline is a single command — a
//! builtin in the middle of a pipe (`echo hi | cd`) is a rare case we punt on
//! for now. Everything else is spawned with `std::process::Command`, wiring
//! each stage's stdout into the next stage's stdin.

use std::fs::{File, OpenOptions};
use std::process::{Child, Command, Stdio};

use crate::builtins;
use crate::parser::{Pipeline, Redirect};

pub fn run_pipeline(pipeline: &Pipeline) -> Result<(), String> {
    // Single-command fast path: lets builtins run in the shell process.
    if pipeline.commands.len() == 1 {
        let cmd = &pipeline.commands[0];
        if let Some(_code) = builtins::try_run(&cmd.argv) {
            return Ok(());
        }
    }

    let n = pipeline.commands.len();
    let mut children: Vec<Child> = Vec::with_capacity(n);
    // Stdin for the next stage: the read end of the previous stage's pipe.
    let mut prev_stdout: Option<Stdio> = None;

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        let is_last = i == n - 1;

        let program = cmd
            .argv
            .first()
            .ok_or_else(|| "empty command".to_string())?;
        let mut command = Command::new(program);
        command.args(&cmd.argv[1..]);

        // stdin: explicit `< file` wins, else the previous pipe, else inherit.
        if let Some(file) = stdin_redirect(&cmd.redirects) {
            let f = File::open(file).map_err(|e| format!("{file}: {e}"))?;
            command.stdin(Stdio::from(f));
        } else if let Some(prev) = prev_stdout.take() {
            command.stdin(prev);
        }

        // stdout: explicit `>`/`>>` wins; else pipe to next stage; else inherit.
        if let Some((file, append)) = stdout_redirect(&cmd.redirects) {
            let f = OpenOptions::new()
                .write(true)
                .create(true)
                .append(append)
                .truncate(!append)
                .open(file)
                .map_err(|e| format!("{file}: {e}"))?;
            command.stdout(Stdio::from(f));
        } else if !is_last {
            command.stdout(Stdio::piped());
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;

        // Hand this child's stdout to the next iteration as its stdin.
        if !is_last {
            prev_stdout = child.stdout.take().map(Stdio::from);
        }
        children.push(child);
    }

    // Wait for every stage; report the last command's failure if any.
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child.wait().map_err(|e| e.to_string())?;
        if i == n - 1 && !status.success() {
            if let Some(code) = status.code() {
                if code != 0 {
                    // Non-fatal: shells just record the exit status.
                    eprintln!("rush: command exited with status {code}");
                }
            }
        }
    }

    Ok(())
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
