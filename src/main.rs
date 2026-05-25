//! rush — a small Rust shell.
//!
//! v0 scope: a REPL with persistent history, pipelines (`|`), redirections
//! (`>`, `>>`, `<`), and the builtins that must run in-process (`cd`, `exit`,
//! `pwd`). Quoting is handled by a small hand-written lexer so that
//! `echo "hello world"` is one argument.
//!
//! Not yet here (see the roadmap): variable expansion, globbing, `&&`/`||`,
//! background jobs, and signal/job control. Those come next.

mod builtins;
mod exec;
mod lexer;
mod parser;

use std::path::PathBuf;

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn history_path() -> Option<PathBuf> {
    let mut p = PathBuf::from(std::env::var_os("HOME")?);
    p.push(".rush_history");
    Some(p)
}

fn prompt() -> String {
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "?".into());
    format!("{cwd} $ ")
}

fn main() -> rustyline::Result<()> {
    let mut rl = DefaultEditor::new()?;
    let hist = history_path();
    if let Some(ref h) = hist {
        let _ = rl.load_history(h);
    }

    loop {
        match rl.readline(&prompt()) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                rl.add_history_entry(line)?;

                match parser::parse(line) {
                    Ok(pipeline) => {
                        if let Err(e) = exec::run_pipeline(&pipeline) {
                            eprintln!("rush: {e}");
                        }
                    }
                    Err(e) => eprintln!("rush: parse error: {e}"),
                }
            }
            // Ctrl-C: abandon the current line, keep the shell alive.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D on an empty line: exit.
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("rush: {e}");
                break;
            }
        }
    }

    if let Some(ref h) = hist {
        let _ = rl.save_history(h);
    }
    Ok(())
}
