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

mod builtins;
mod exec;
mod expand;
mod glob;
#[cfg(unix)]
mod job;
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

    // Claim the terminal and set up signal handling for job control.
    #[cfg(unix)]
    job::init();

    loop {
        // Report any background jobs that finished or stopped since last prompt.
        #[cfg(unix)]
        job::reap_background();

        match rl.readline(&prompt()) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                rl.add_history_entry(line)?;

                match parser::parse(line) {
                    Ok(list) => {
                        if let Err(e) = exec::run_list(&list) {
                            eprintln!("rush: {e}");
                        }
                    }
                    Err(e) => eprintln!("rush: {e}"),
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
