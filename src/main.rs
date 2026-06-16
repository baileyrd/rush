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

mod arith;
mod builtins;
mod exec;
mod expand;
mod func;
mod glob;
#[cfg(unix)]
mod job;
mod lexer;
mod parser;
mod vars;

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
    let args: Vec<String> = std::env::args().collect();

    // Non-interactive modes: `rush -c "cmd" [name args…]` and `rush FILE [args…]`.
    match args.get(1).map(String::as_str) {
        Some("-c") => {
            let cmd = args.get(2).cloned().unwrap_or_default();
            let name = args.get(3).cloned().unwrap_or_else(|| "rush".to_string());
            vars::set_args(name, args.get(4..).unwrap_or(&[]).to_vec());
            std::process::exit(run_source(&cmd));
        }
        Some(file) => {
            vars::set_args(file.to_string(), args.get(2..).unwrap_or(&[]).to_vec());
            match std::fs::read_to_string(file) {
                Ok(src) => std::process::exit(run_source(&src)),
                Err(e) => {
                    eprintln!("rush: {file}: {e}");
                    std::process::exit(1);
                }
            }
        }
        None => interactive(),
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

fn interactive() -> rustyline::Result<()> {
    let mut rl = DefaultEditor::new()?;
    let hist = history_path();
    if let Some(ref h) = hist {
        let _ = rl.load_history(h);
    }

    // Claim the terminal and set up signal handling for job control.
    #[cfg(unix)]
    job::init();

    // Accumulates lines until a complete command is parsed — so an `if`/`while`
    // can span several lines, with a `> ` continuation prompt.
    let mut buffer = String::new();

    loop {
        // Report any background jobs that finished or stopped since last prompt.
        #[cfg(unix)]
        job::reap_background();

        let prompt = if buffer.is_empty() { prompt() } else { "> ".to_string() };
        match rl.readline(&prompt) {
            Ok(line) => {
                if buffer.is_empty() && line.trim().is_empty() {
                    continue;
                }
                rl.add_history_entry(&line)?;
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
            // Ctrl-C: abandon the current (possibly multi-line) input.
            Err(ReadlineError::Interrupted) => {
                buffer.clear();
                continue;
            }
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
