//! Builtins that must run inside the shell process.
//!
//! `cd` is the canonical example: if it ran in a child process, the child's
//! working-directory change would die with the child and the shell would
//! never move. So these are handled before we ever spawn anything.

use std::path::Path;

/// Returns `Some(exit_code)` if `argv` named a builtin (and it ran), or
/// `None` if this isn't a builtin and should be exec'd as an external command.
pub fn try_run(argv: &[String]) -> Option<i32> {
    match argv.first().map(String::as_str)? {
        "cd" => Some(cd(argv)),
        "pwd" => Some(pwd()),
        "echo" => Some(echo(argv)),
        "export" => Some(export(argv)),
        "unset" => Some(unset(argv)),
        // POSIX no-op (`:`) and the canonical true/false.
        "true" | ":" => Some(0),
        "false" => Some(1),
        "exit" => exit(argv), // diverges on success
        _ => other_builtin(argv),
    }
}

/// `echo [-n] [args...]` — join args with spaces; `-n` suppresses the newline.
/// (No `-e` escape processing, matching the bash default.)
fn echo(argv: &[String]) -> i32 {
    let mut args = &argv[1..];
    let newline = !matches!(args.first(), Some(flag) if flag == "-n");
    if !newline {
        args = &args[1..];
    }

    let line = args.join(" ");
    if newline {
        println!("{line}");
    } else {
        use std::io::Write;
        print!("{line}");
        let _ = std::io::stdout().flush();
    }
    0
}

/// Platform-specific builtins. On Unix this is where `jobs`/`fg`/`bg` live.
#[cfg(unix)]
fn other_builtin(argv: &[String]) -> Option<i32> {
    crate::job::builtin(argv)
}

#[cfg(not(unix))]
fn other_builtin(_argv: &[String]) -> Option<i32> {
    None
}

fn cd(argv: &[String]) -> i32 {
    // `cd` with no args goes home; `cd -` is not yet supported.
    let target = match argv.get(1) {
        Some(dir) => dir.clone(),
        None => match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => {
                eprintln!("cd: HOME not set");
                return 1;
            }
        },
    };

    if let Err(e) = std::env::set_current_dir(Path::new(&target)) {
        eprintln!("cd: {target}: {e}");
        return 1;
    }
    0
}

fn pwd() -> i32 {
    match std::env::current_dir() {
        Ok(p) => {
            println!("{}", p.display());
            0
        }
        Err(e) => {
            eprintln!("pwd: {e}");
            1
        }
    }
}

/// `export NAME` marks an existing variable exported; `export NAME=value` sets
/// and exports it. The `NAME=value` arg arrives already expanded.
fn export(argv: &[String]) -> i32 {
    for arg in &argv[1..] {
        match arg.split_once('=') {
            Some((name, value)) => crate::vars::set_exported(name, value),
            None => crate::vars::export(arg),
        }
    }
    0
}

/// `unset NAME...` — remove shell variables.
fn unset(argv: &[String]) -> i32 {
    for name in &argv[1..] {
        crate::vars::unset(name);
    }
    0
}

fn exit(argv: &[String]) -> Option<i32> {
    let code = argv
        .get(1)
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    std::process::exit(code);
}
