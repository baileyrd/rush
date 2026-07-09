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
        "test" => Some(test_dispatch(argv, false)),
        "[" => Some(test_dispatch(argv, true)),
        "break" => Some(loop_ctl(argv, true)),
        "continue" => Some(loop_ctl(argv, false)),
        "return" => Some(return_cmd(argv)),
        // POSIX no-op (`:`) and the canonical true/false.
        "true" | ":" => Some(0),
        "false" => Some(1),
        "exit" => exit(argv), // diverges on success
        "alias" => Some(alias_cmd(argv)),
        "unalias" => Some(unalias_cmd(argv)),
        "set" => Some(set_cmd(argv)),
        "trap" => Some(trap_cmd(argv)),
        "read" => Some(read_cmd(argv)),
        _ => other_builtin(argv),
    }
}

/// Names `try_run` dispatches directly (excludes the platform-specific ones
/// in `other_builtin`, e.g. `job`'s `jobs`/`fg`/`bg`/`kill` on Unix).
pub const NAMES: &[&str] = &[
    "cd", "pwd", "echo", "export", "unset", "test", "[", "break", "continue", "return", "true",
    ":", "false", "exit", "alias", "unalias", "set", "trap", "read",
];

/// Whether `name` is one `try_run` dispatches — so a caller can wire up
/// redirects for a builtin *before* running it, without a speculative,
/// side-effect-free call to `try_run` itself.
pub fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name) || other_is_builtin(name)
}

/// Every builtin name, for tab completion in command position.
pub fn all_names() -> Vec<&'static str> {
    let mut names = NAMES.to_vec();
    names.extend_from_slice(other_names());
    names
}

#[cfg(unix)]
fn other_is_builtin(name: &str) -> bool {
    crate::job::is_builtin(name)
}

#[cfg(not(unix))]
fn other_is_builtin(_name: &str) -> bool {
    false
}

#[cfg(unix)]
fn other_names() -> &'static [&'static str] {
    crate::job::NAMES
}

#[cfg(not(unix))]
fn other_names() -> &'static [&'static str] {
    &[]
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
    // `cd -` goes to $OLDPWD and echoes it, like POSIX `cd`.
    let going_back = argv.get(1).map(String::as_str) == Some("-");
    let target = match argv.get(1) {
        Some(_) if going_back => match crate::vars::get("OLDPWD") {
            Some(dir) => dir,
            None => {
                eprintln!("cd: OLDPWD not set");
                return 1;
            }
        },
        Some(dir) => dir.clone(),
        None => match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => {
                eprintln!("cd: HOME not set");
                return 1;
            }
        },
    };

    let previous = std::env::current_dir().ok();

    if let Err(e) = std::env::set_current_dir(Path::new(&target)) {
        eprintln!("cd: {target}: {e}");
        return 1;
    }

    if let Some(dir) = previous {
        crate::vars::set("OLDPWD", &dir.display().to_string());
    }
    if going_back {
        println!("{target}");
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

/// `read [-r] [name...]` — read one line from stdin, splitting it into
/// fields on `$IFS` and assigning them to the named variables (`REPLY` if
/// none given); a name past the last field gets the empty string, and the
/// *last* name absorbs any extra fields verbatim (original separators
/// intact), not re-split. `-r` disables backslash processing (no line
/// continuation, no escaping a separator). Reads directly off fd 0 one byte
/// at a time — this builtin's own redirects (or a whole compound's, via
/// `exec::redirect_stdio`) already point fd 0 wherever it needs to be by the
/// time this runs, and a byte-at-a-time read never over-consumes past the
/// newline, so a loop of `read` calls sharing that same fd (`while read
/// line; do …; done < file`) picks up exactly where the last call left off.
///
/// Exit status: 0 if a newline-terminated line was read, 1 on EOF — even if
/// a trailing, unterminated partial line was read first (its content is
/// still assigned, matching real bash).
fn read_cmd(argv: &[String]) -> i32 {
    let mut raw = false;
    let mut idx = 1;
    while idx < argv.len() {
        match argv[idx].as_str() {
            "-r" => {
                raw = true;
                idx += 1;
            }
            "--" => {
                idx += 1;
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("read: {s}: invalid option");
                return 2;
            }
            _ => break,
        }
    }

    let mut names: Vec<&str> = argv[idx..].iter().map(String::as_str).collect();
    if names.is_empty() {
        names.push("REPLY");
    }

    let (line, protected, hit_eof) = read_logical_line(raw);
    let fields = split_read_fields(&line, &protected);
    assign_read_fields(&names, &line, &fields);
    if hit_eof { 1 } else { 0 }
}

/// Read one logical line from stdin, byte at a time (see `read_cmd`'s doc for
/// why). In raw mode, a physical newline always ends the line. Otherwise,
/// backslash processing runs *during* the read: `\<newline>` is a line
/// continuation (both bytes dropped, reading carries on into the next
/// physical line with no field boundary inserted at the join); `\<char>`
/// drops the backslash and keeps `<char>` marked "protected" in the returned
/// mask, so field-splitting never treats it as a separator even if it's an
/// `$IFS` character. A lone trailing backslash right at EOF is just dropped.
///
/// Returns `(line, protected, hit_eof)`: `hit_eof` is true iff the line
/// ended by exhausting stdin rather than a real newline.
fn read_logical_line(raw: bool) -> (Vec<u8>, Vec<bool>, bool) {
    use std::io::Read;

    let mut stdin = std::io::stdin();
    let mut line = Vec::new();
    let mut protected = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match stdin.read(&mut byte) {
            Ok(0) => return (line, protected, true),
            Err(_) => return (line, protected, true),
            Ok(_) => {}
        }
        let b = byte[0];
        if b == b'\n' {
            return (line, protected, false);
        }
        if !raw && b == b'\\' {
            match stdin.read(&mut byte) {
                Ok(0) | Err(_) => return (line, protected, true),
                Ok(_) => {
                    let next = byte[0];
                    if next != b'\n' {
                        line.push(next);
                        protected.push(true);
                    }
                    // else: line continuation — both bytes dropped, keep reading.
                }
            }
        } else {
            line.push(b);
            protected.push(false);
        }
    }
}

/// One field's byte range within the line read by `read_cmd`.
struct ReadField {
    start: usize,
    end: usize,
}

/// `$IFS`'s whitespace/non-whitespace split, as bytes — the same
/// classification `expand.rs`'s `Ifs` uses for word-splitting (see there for
/// the full rationale), just not sharing code since `read`'s "last name
/// absorbs the raw remainder" behavior needs field *positions* into the
/// original bytes, which word-splitting's model has no reason to track.
struct ReadIfs {
    whitespace: Vec<u8>,
    other: Vec<u8>,
    disabled: bool,
}

impl ReadIfs {
    fn current() -> Self {
        match crate::vars::get("IFS").or_else(|| std::env::var("IFS").ok()) {
            None => ReadIfs {
                whitespace: vec![b' ', b'\t', b'\n'],
                other: Vec::new(),
                disabled: false,
            },
            Some(s) if s.is_empty() => {
                ReadIfs { whitespace: Vec::new(), other: Vec::new(), disabled: true }
            }
            Some(s) => {
                let mut whitespace = Vec::new();
                let mut other = Vec::new();
                for &b in s.as_bytes() {
                    let bucket = if matches!(b, b' ' | b'\t' | b'\n') { &mut whitespace } else { &mut other };
                    if !bucket.contains(&b) {
                        bucket.push(b);
                    }
                }
                ReadIfs { whitespace, other, disabled: false }
            }
        }
    }

    fn is_whitespace(&self, b: u8) -> bool {
        self.whitespace.contains(&b)
    }

    fn is_delim(&self, b: u8) -> bool {
        self.whitespace.contains(&b) || self.other.contains(&b)
    }
}

/// Split `line` into fields on `$IFS`, treating any byte marked `protected`
/// (backslash-escaped — see `read_logical_line`) as never a delimiter. Same
/// rules as word-splitting: whitespace runs collapse (no empty fields); each
/// non-whitespace `$IFS` byte delimits a field on its own, even empty, except
/// a single trailing one right at the end of the line, which — matching a
/// real asymmetry in bash's own behavior — produces no trailing empty field.
///
/// The trailing-elision falls out for free from *not* eagerly reopening a
/// field right after a hard (non-whitespace) delimiter fires: if nothing
/// real follows before EOF, `open_start` simply stays `None` and nothing
/// more gets pushed. If another hard delimiter follows immediately after
/// (`,,`), *that* firing computes its own start as its own position (via
/// `unwrap_or(k)`), correctly producing the empty field between them — so a
/// *repeated* trailing delimiter still leaves one behind, just not the
/// final one.
fn split_read_fields(line: &[u8], protected: &[bool]) -> Vec<ReadField> {
    let ifs = ReadIfs::current();
    if ifs.disabled {
        return if line.is_empty() { Vec::new() } else { vec![ReadField { start: 0, end: line.len() }] };
    }

    let is_delim = |i: usize| !protected[i] && ifs.is_delim(line[i]);
    let is_ws = |i: usize| !protected[i] && ifs.is_whitespace(line[i]);

    let mut fields = Vec::new();
    let mut open_start: Option<usize> = None;
    let n = line.len();
    let mut i = 0;

    while i < n {
        if is_delim(i) {
            let mut j = i;
            let mut hard = 0usize;
            while j < n && is_delim(j) {
                if !is_ws(j) {
                    hard += 1;
                }
                j += 1;
            }
            if hard > 0 {
                let mut k = i;
                while k < j {
                    if !is_ws(k) {
                        let start = open_start.take().unwrap_or(k);
                        fields.push(ReadField { start, end: k });
                    }
                    k += 1;
                }
            } else if let Some(start) = open_start.take() {
                // A pure-whitespace run: close whatever field was open (if
                // any) — the next real content, if there is any, starts a
                // fresh one.
                fields.push(ReadField { start, end: i });
            }
            i = j;
        } else {
            if open_start.is_none() {
                open_start = Some(i);
            }
            i += 1;
        }
    }

    if let Some(start) = open_start {
        fields.push(ReadField { start, end: n });
    }

    fields
}

/// Assign split fields to `names`: each name gets its own field in order: a
/// name past the last field gets `""`; if there are *more* fields than
/// names, the last name absorbs everything from its field's start to the end
/// of the line verbatim (original separators intact), not re-split.
fn assign_read_fields(names: &[&str], line: &[u8], fields: &[ReadField]) {
    let n_names = names.len();
    if fields.len() <= n_names {
        for (i, name) in names.iter().enumerate() {
            let value = match fields.get(i) {
                Some(f) => String::from_utf8_lossy(&line[f.start..f.end]).into_owned(),
                None => String::new(),
            };
            crate::vars::set(name, &value);
        }
    } else {
        for (name, f) in names[..n_names - 1].iter().zip(fields) {
            crate::vars::set(name, &String::from_utf8_lossy(&line[f.start..f.end]));
        }
        let overflow_start = fields[n_names - 1].start;
        let value = String::from_utf8_lossy(&line[overflow_start..]).into_owned();
        crate::vars::set(names[n_names - 1], &value);
    }
}

/// `test EXPR` / `[ EXPR ]` — evaluate a conditional, returning 0 (true),
/// 1 (false), or 2 (usage error). Supports the common unary file/string tests,
/// string `=`/`!=`, integer `-eq`/`-ne`/`-lt`/`-le`/`-gt`/`-ge`, a leading
/// `!`, and the `-a`/`-o` logical combinators. (No parentheses yet.)
fn test_dispatch(argv: &[String], bracket: bool) -> i32 {
    let args: &[String] = if bracket {
        if argv.last().map(String::as_str) != Some("]") {
            eprintln!("[: missing `]'");
            return 2;
        }
        &argv[1..argv.len() - 1]
    } else {
        &argv[1..]
    };
    match test_eval(args) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(msg) => {
            eprintln!("test: {msg}");
            2
        }
    }
}

const UNARY_OPS: &[&str] = &["-z", "-n", "-e", "-f", "-d", "-s", "-r", "-w", "-x"];
const BINARY_OPS: &[&str] =
    &["=", "==", "!=", "-eq", "-ne", "-lt", "-le", "-gt", "-ge"];

/// `EXPR1 -o EXPR2` (lowest precedence, left-assoc): true if either side is.
fn test_eval(args: &[String]) -> Result<bool, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut pos = 0;
    let result = test_or(&words, &mut pos)?;
    if pos != words.len() {
        return Err("too many arguments".into());
    }
    Ok(result)
}

fn test_or(a: &[&str], pos: &mut usize) -> Result<bool, String> {
    let mut result = test_and(a, pos)?;
    while *pos < a.len() && a[*pos] == "-o" {
        *pos += 1;
        result = test_and(a, pos)? || result;
    }
    Ok(result)
}

/// `EXPR1 -a EXPR2` (binds tighter than `-o`, left-assoc): true if both are.
fn test_and(a: &[&str], pos: &mut usize) -> Result<bool, String> {
    let mut result = test_not(a, pos)?;
    while *pos < a.len() && a[*pos] == "-a" {
        *pos += 1;
        result = test_not(a, pos)? && result;
    }
    Ok(result)
}

/// `! EXPR` negates only the next primary (bash's actual behavior — it does
/// *not* negate a whole trailing `-a`/`-o` chain).
fn test_not(a: &[&str], pos: &mut usize) -> Result<bool, String> {
    if *pos < a.len() && a[*pos] == "!" {
        *pos += 1;
        return test_not(a, pos).map(|b| !b);
    }
    test_primary(a, pos)
}

fn test_primary(a: &[&str], pos: &mut usize) -> Result<bool, String> {
    if *pos >= a.len() {
        return Ok(false); // an empty expression is false
    }
    if UNARY_OPS.contains(&a[*pos]) && *pos + 1 < a.len() {
        let (op, operand) = (a[*pos], a[*pos + 1]);
        *pos += 2;
        return test_unary(op, operand);
    }
    if *pos + 1 < a.len() && BINARY_OPS.contains(&a[*pos + 1]) {
        if *pos + 2 >= a.len() {
            return Err(format!("`{}': argument expected", a[*pos + 1]));
        }
        let (x, op, y) = (a[*pos], a[*pos + 1], a[*pos + 2]);
        *pos += 3;
        return test_binary(x, op, y);
    }
    // A lone string: true when non-empty.
    let s = a[*pos];
    *pos += 1;
    Ok(!s.is_empty())
}

fn test_unary(op: &str, s: &str) -> Result<bool, String> {
    use std::path::Path;
    Ok(match op {
        "-z" => s.is_empty(),
        "-n" => !s.is_empty(),
        "-e" => Path::new(s).exists(),
        "-f" => Path::new(s).is_file(),
        "-d" => Path::new(s).is_dir(),
        "-s" => Path::new(s).metadata().map(|m| m.len() > 0).unwrap_or(false),
        // Permission bits aren't portable; approximate with existence.
        "-r" | "-w" | "-x" => Path::new(s).exists(),
        _ => return Err(format!("unknown unary operator `{op}`")),
    })
}

fn test_binary(a: &str, op: &str, b: &str) -> Result<bool, String> {
    let int = |s: &str| s.parse::<i64>().map_err(|_| format!("integer expected: `{s}`"));
    Ok(match op {
        "=" | "==" => a == b,
        "!=" => a != b,
        "-eq" => int(a)? == int(b)?,
        "-ne" => int(a)? != int(b)?,
        "-lt" => int(a)? < int(b)?,
        "-le" => int(a)? <= int(b)?,
        "-gt" => int(a)? > int(b)?,
        "-ge" => int(a)? >= int(b)?,
        _ => return Err(format!("unknown operator `{op}`")),
    })
}

/// `break [n]` / `continue [n]` — request loop control; the executor acts on it.
fn loop_ctl(argv: &[String], is_break: bool) -> i32 {
    let n: u32 = argv.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    if n == 0 {
        eprintln!("{}: loop count must be positive", argv[0]);
        return 1;
    }
    let ctl = if is_break {
        crate::vars::LoopCtl::Break(n)
    } else {
        crate::vars::LoopCtl::Continue(n)
    };
    crate::vars::set_loop_ctl(Some(ctl));
    0
}

/// `return [n]` — unwind the current function with status `n` (default `$?`).
/// The executor's `call_function` consumes the request.
fn return_cmd(argv: &[String]) -> i32 {
    let code = argv
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(crate::vars::last_status);
    crate::vars::set_returning(Some(code));
    code
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
    crate::trap::exit_shell(code);
}

/// `alias` (list all) / `alias NAME` (show one) / `alias NAME=value` (define).
fn alias_cmd(argv: &[String]) -> i32 {
    if argv.len() == 1 {
        for (name, value) in crate::alias::all() {
            println!("alias {name}='{value}'");
        }
        return 0;
    }
    let mut status = 0;
    for arg in &argv[1..] {
        match arg.split_once('=') {
            Some((name, value)) => crate::alias::set(name, value),
            None => match crate::alias::get(arg) {
                Some(value) => println!("alias {arg}='{value}'"),
                None => {
                    eprintln!("alias: {arg}: not found");
                    status = 1;
                }
            },
        }
    }
    status
}

/// `unalias NAME...` / `unalias -a` (remove all).
fn unalias_cmd(argv: &[String]) -> i32 {
    if argv.get(1).map(String::as_str) == Some("-a") {
        crate::alias::unset_all();
        return 0;
    }
    let mut status = 0;
    for name in &argv[1..] {
        if !crate::alias::unset(name) {
            eprintln!("unalias: {name}: not found");
            status = 1;
        }
    }
    status
}

/// `set -e` / `set +e` — errexit: a failing command exits the shell (see
/// `exec::exec_list_impl`). Other flags aren't implemented yet; naming one is
/// an error rather than a silently-ignored no-op.
fn set_cmd(argv: &[String]) -> i32 {
    let mut status = 0;
    for arg in &argv[1..] {
        match arg.as_str() {
            "-e" => crate::vars::set_errexit(true),
            "+e" => crate::vars::set_errexit(false),
            other => {
                eprintln!("set: {other}: not supported");
                status = 1;
            }
        }
    }
    status
}

/// `trap` (list) / `trap 'command' NAME...` (register) / `trap - NAME...`
/// (reset to default). Only `EXIT` and `INT` are ever fired (see `trap.rs`).
fn trap_cmd(argv: &[String]) -> i32 {
    if argv.len() == 1 {
        for (name, command) in crate::trap::all() {
            println!("trap -- '{command}' {name}");
        }
        return 0;
    }
    let command = &argv[1];
    if command == "-" {
        for name in &argv[2..] {
            crate::trap::unset(name);
        }
        return 0;
    }
    for name in &argv[2..] {
        crate::trap::set(name, command);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(args: &[&str]) -> Result<bool, String> {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        test_eval(&v)
    }

    #[test]
    fn test_strings_and_negation() {
        assert_eq!(ev(&[]), Ok(false));
        assert_eq!(ev(&["abc"]), Ok(true));
        assert_eq!(ev(&[""]), Ok(false));
        assert_eq!(ev(&["-z", ""]), Ok(true));
        assert_eq!(ev(&["-n", "x"]), Ok(true));
        assert_eq!(ev(&["a", "=", "a"]), Ok(true));
        assert_eq!(ev(&["a", "!=", "b"]), Ok(true));
        assert_eq!(ev(&["!", "-z", "x"]), Ok(true));
    }

    #[test]
    fn test_integers() {
        assert_eq!(ev(&["3", "-lt", "5"]), Ok(true));
        assert_eq!(ev(&["5", "-eq", "5"]), Ok(true));
        assert_eq!(ev(&["5", "-ge", "9"]), Ok(false));
        assert!(ev(&["x", "-eq", "5"]).is_err());
    }

    #[test]
    fn test_files() {
        assert_eq!(ev(&["-f", "Cargo.toml"]), Ok(true));
        assert_eq!(ev(&["-d", "src"]), Ok(true));
        assert_eq!(ev(&["-e", "no-such-file-xyz"]), Ok(false));
    }

    #[test]
    fn test_logical_combinators() {
        // `-a` (AND) / `-o` (OR).
        assert_eq!(ev(&["x", "-a", "y"]), Ok(true));
        assert_eq!(ev(&["x", "-a", ""]), Ok(false));
        assert_eq!(ev(&["", "-o", "y"]), Ok(true));
        assert_eq!(ev(&["", "-o", ""]), Ok(false));

        // `-a` binds tighter than `-o`, matching bash: `1 = 2 -o 1 = 1 -a 1 =
        // 2` groups as `(1 = 2) -o ((1 = 1) -a (1 = 2))` = F -o F = false.
        assert_eq!(ev(&["1", "=", "2", "-o", "1", "=", "1", "-a", "1", "=", "2"]), Ok(false));
        assert_eq!(ev(&["1", "=", "1", "-o", "1", "=", "1", "-a", "1", "=", "2"]), Ok(true));

        // `!` negates only the next primary, not a whole trailing `-a`/`-o`
        // chain — matches real bash (verified against it directly): `! F -a
        // F` is `(!F) -a F` = `T -a F` = false, not `!(F -a F)` = true.
        assert_eq!(ev(&["!", "", "-a", ""]), Ok(false));
        assert_eq!(ev(&["!", "", "-a", "!", ""]), Ok(true));

        // Unary/binary operators still combine with `-a`/`-o`.
        assert_eq!(ev(&["-f", "Cargo.toml", "-a", "-d", "src"]), Ok(true));
        assert_eq!(ev(&["-f", "Cargo.toml", "-a", "-f", "no-such-file-xyz"]), Ok(false));
    }

    /// Split `line` (no backslash-escaping) on `ifs` (setting `$IFS`, or
    /// leaving it unset for the default), returning each field's text.
    fn split(line: &str, ifs: Option<&str>) -> Vec<String> {
        match ifs {
            Some(v) => crate::vars::set("IFS", v),
            None => crate::vars::unset("IFS"),
        }
        let bytes = line.as_bytes();
        let protected = vec![false; bytes.len()];
        split_read_fields(bytes, &protected)
            .iter()
            .map(|f| String::from_utf8_lossy(&bytes[f.start..f.end]).into_owned())
            .collect()
    }

    #[test]
    fn read_field_splitting_matches_real_bash() {
        // Default IFS: whitespace runs collapse, no empty fields.
        assert_eq!(split("a   b     c    d", None), vec!["a", "b", "c", "d"]);
        assert_eq!(split("  leading", None), vec!["leading"]);
        assert_eq!(split("trailing  ", None), vec!["trailing"]);
        assert_eq!(split("   ", None), Vec::<String>::new());

        // Custom non-whitespace IFS: each occurrence delimits its own field,
        // even empty — `a,,b` is three fields, not two.
        assert_eq!(split("a,,b,c", Some(",")), vec!["a", "", "b", "c"]);
        // Leading delimiter keeps an empty field; a single *trailing* one at
        // the very end doesn't — matching bash's own asymmetry there.
        assert_eq!(split(",a,", Some(",")), vec!["", "a"]);
        // A *repeated* trailing delimiter still leaves one behind (only the
        // final one is elided).
        assert_eq!(split("a,,b,,", Some(",")), vec!["a", "", "b", ""]);

        // Mixed whitespace + non-whitespace IFS: whitespace immediately
        // adjacent to a hard delimiter is absorbed into it, not an extra
        // boundary of its own.
        assert_eq!(split("a, b,, c", Some(" ,")), vec!["a", "b", "", "c"]);

        // `IFS=` (explicitly empty) disables splitting entirely.
        assert_eq!(split("a  b", Some("")), vec!["a  b"]);

        crate::vars::unset("IFS");
    }
}
