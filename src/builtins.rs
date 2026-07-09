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
        "printf" => Some(printf_cmd(argv)),
        "shift" => Some(shift_cmd(argv)),
        "local" => Some(local_cmd(argv)),
        "getopts" => Some(getopts_cmd(argv)),
        "command" => Some(command_cmd(argv)),
        "type" => Some(type_cmd(argv)),
        "hash" => Some(hash_cmd(argv)),
        "." | "source" => Some(source_cmd(argv)),
        "eval" => Some(eval_cmd(argv)),
        "exec" => Some(crate::exec::exec_cmd(argv)),
        "umask" => Some(umask_cmd(argv)),
        _ => other_builtin(argv),
    }
}

/// Names `try_run` dispatches directly (excludes the platform-specific ones
/// in `other_builtin`, e.g. `job`'s `jobs`/`fg`/`bg`/`kill` on Unix).
pub const NAMES: &[&str] = &[
    "cd", "pwd", "echo", "export", "unset", "test", "[", "break", "continue", "return", "true",
    ":", "false", "exit", "alias", "unalias", "set", "trap", "read", "printf", "shift", "local",
    "getopts", "command", "type", "hash", ".", "source", "eval", "exec", "umask",
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

/// `printf FORMAT [args...]` — the portable, correct way to emit formatted
/// output (unlike `echo`, whose formatting is whatever the platform's
/// convention happens to be). Supports `%s`/`%b` (string, `%b` also
/// processing backslash escapes in *its* argument), `%d`/`%i`/`%o`/`%u`/`%x`/
/// `%X` (integer, decimal/octal/unsigned/hex), `%c` (first character), `%%`,
/// the `-`/`0`/`+`/` ` flags, and a width and/or `.precision` — no `*`
/// (width/precision from an argument) and no floating-point conversions
/// (`%e`/`%f`/`%g`) yet. `\n`/`\t`/`\\`/`\a`/`\b`/`\f`/`\r`/`\v`/`\NNN` (octal)
/// escapes in the *format string* are processed once, up front.
///
/// If the format has more argument-consuming conversions than there are
/// arguments, the missing ones default to `""`/`0`. If there are *more*
/// arguments than the format consumes (and it consumes at least one), the
/// whole format repeats against the rest, POSIX/bash style: `printf
/// "%s-%d\n" a 1 b 2 c` is `a-1`, `b-2`, `c-0`.
fn printf_cmd(argv: &[String]) -> i32 {
    let Some(format) = argv.get(1) else {
        eprintln!("printf: usage: printf format [arguments]");
        return 2;
    };
    let args = &argv[2..];

    let pieces = match printf::parse_format(format) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("printf: {e}");
            return 1;
        }
    };
    let consumes_args = pieces.iter().any(|p| matches!(p, printf::Piece::Conv(_)));

    let mut idx = 0;
    let mut out = String::new();
    let mut status = 0;
    loop {
        let start_idx = idx;
        for piece in &pieces {
            match piece {
                printf::Piece::Literal(s) => out.push_str(s),
                printf::Piece::Conv(conv) => {
                    let arg = args.get(idx);
                    if arg.is_some() {
                        idx += 1;
                    }
                    let (text, err) = printf::format_conv(conv, arg.map(String::as_str).unwrap_or(""));
                    out.push_str(&text);
                    if let Some(e) = err {
                        eprintln!("printf: {e}");
                        status = 1;
                    }
                }
            }
        }
        if !consumes_args || idx >= args.len() || idx == start_idx {
            break;
        }
    }

    print!("{out}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    status
}

/// `printf`'s format-string parsing and per-conversion formatting.
mod printf {
    #[derive(Debug)]
    pub enum Piece {
        Literal(String),
        Conv(Conv),
    }

    #[derive(Debug, Default)]
    pub struct Conv {
        minus: bool,
        zero: bool,
        plus: bool,
        space: bool,
        width: Option<usize>,
        precision: Option<usize>,
        spec: char,
    }

    /// Parse a format string into literal chunks (with `\`-escapes already
    /// resolved) and conversion specs, ready to be replayed once per cycle
    /// through the argument list.
    pub fn parse_format(format: &str) -> Result<Vec<Piece>, String> {
        let mut pieces = Vec::new();
        let mut literal = String::new();
        let mut chars = format.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                push_escape(&mut literal, &mut chars);
            } else if c == '%' {
                if chars.peek() == Some(&'%') {
                    chars.next();
                    literal.push('%');
                    continue;
                }
                if !literal.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Conv(parse_conv(&mut chars)?));
            } else {
                literal.push(c);
            }
        }
        if !literal.is_empty() {
            pieces.push(Piece::Literal(literal));
        }
        Ok(pieces)
    }

    /// Resolve one backslash escape (the `\` itself already consumed) into
    /// `out` — `\\`/`\a`/`\b`/`\f`/`\n`/`\r`/`\t`/`\v`, `\NNN` (one to three
    /// octal digits), or an unrecognized sequence kept literally.
    fn push_escape(out: &mut String, chars: &mut std::iter::Peekable<std::str::Chars>) {
        match chars.peek() {
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some('a') => {
                out.push('\x07');
                chars.next();
            }
            Some('b') => {
                out.push('\x08');
                chars.next();
            }
            Some('f') => {
                out.push('\x0c');
                chars.next();
            }
            Some('n') => {
                out.push('\n');
                chars.next();
            }
            Some('r') => {
                out.push('\r');
                chars.next();
            }
            Some('t') => {
                out.push('\t');
                chars.next();
            }
            Some('v') => {
                out.push('\x0b');
                chars.next();
            }
            Some('0'..='7') => {
                let mut val: u32 = 0;
                for _ in 0..3 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            val = val * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push((val as u8) as char);
            }
            _ => out.push('\\'),
        }
    }

    /// Parse `[flags][width][.precision]spec` right after the `%` that
    /// introduced it.
    fn parse_conv(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<Conv, String> {
        let mut conv = Conv::default();
        loop {
            match chars.peek() {
                Some('-') => conv.minus = true,
                Some('0') => conv.zero = true,
                Some('+') => conv.plus = true,
                Some(' ') => conv.space = true,
                _ => break,
            }
            chars.next();
        }

        conv.width = take_digits(chars);
        if chars.peek() == Some(&'.') {
            chars.next();
            conv.precision = Some(take_digits(chars).unwrap_or(0));
        }

        conv.spec = chars.next().ok_or("missing conversion specifier")?;
        if !"diouxXcsb".contains(conv.spec) {
            return Err(format!("`%{}': invalid conversion specification", conv.spec));
        }
        Ok(conv)
    }

    fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<usize> {
        let mut n = 0usize;
        let mut any = false;
        while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
            n = n * 10 + d as usize;
            any = true;
            chars.next();
        }
        any.then_some(n)
    }

    /// Format one conversion against its argument (`""` if none was left).
    /// Returns the formatted text and, if the argument couldn't be parsed as
    /// a number a numeric conversion needed, an error message (the
    /// conversion still yields `0`/`""` rather than aborting the whole
    /// `printf`, matching bash).
    pub fn format_conv(conv: &Conv, raw: &str) -> (String, Option<String>) {
        let mut error = None;
        let mut parse_int = || match raw.trim() {
            "" => 0i64,
            s => s.parse().unwrap_or_else(|_| {
                error = Some(format!("{raw}: invalid number"));
                0
            }),
        };

        let (body, is_numeric) = match conv.spec {
            's' => (truncate(raw, conv.precision), false),
            'b' => {
                let mut expanded = String::new();
                let mut chars = raw.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        push_escape(&mut expanded, &mut chars);
                    } else {
                        expanded.push(c);
                    }
                }
                (truncate(&expanded, conv.precision), false)
            }
            'c' => (raw.chars().next().map(String::from).unwrap_or_default(), false),
            'd' | 'i' => {
                let n = parse_int();
                (signed(n, conv), true)
            }
            'o' => (format!("{:o}", parse_int() as u64), true),
            'u' => (format!("{}", parse_int() as u64), true),
            'x' => (format!("{:x}", parse_int() as u64), true),
            'X' => (format!("{:X}", parse_int() as u64), true),
            _ => unreachable!("parse_conv only accepts known specifiers"),
        };

        (pad(body, conv, is_numeric), error)
    }

    fn truncate(s: &str, precision: Option<usize>) -> String {
        match precision {
            Some(p) => s.chars().take(p).collect(),
            None => s.to_string(),
        }
    }

    fn signed(n: i64, conv: &Conv) -> String {
        if n < 0 {
            n.to_string()
        } else if conv.plus {
            format!("+{n}")
        } else if conv.space {
            format!(" {n}")
        } else {
            n.to_string()
        }
    }

    /// Apply `conv`'s width, left/right-aligning with spaces (`-`) or, for a
    /// numeric conversion with no `-`, zero-padding after any sign.
    fn pad(s: String, conv: &Conv, is_numeric: bool) -> String {
        let width = conv.width.unwrap_or(0);
        let len = s.chars().count();
        if len >= width {
            return s;
        }
        let fill = width - len;
        if conv.minus {
            s + &" ".repeat(fill)
        } else if conv.zero && is_numeric {
            match s.strip_prefix(['-', '+']) {
                Some(rest) => format!("{}{}{rest}", &s[..1], "0".repeat(fill)),
                None => "0".repeat(fill) + &s,
            }
        } else {
            " ".repeat(fill) + &s
        }
    }
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

/// `shift [n]` — drop the first `n` (default 1) positional parameters,
/// connecting them to `case` into the ubiquitous `while [ $# -gt 0 ]; do
/// case $1 in …; esac; shift; done` argument-parsing loop. A negative `n`
/// is a hard usage error; `n` greater than `$#` is *not* — bash just fails
/// silently there (no message), since running past the end this way is the
/// everyday way an argument-parsing loop notices it's done.
fn shift_cmd(argv: &[String]) -> i32 {
    let n: i64 = match argv.get(1) {
        None => 1,
        Some(s) => match s.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("shift: {s}: numeric argument required");
                return 1;
            }
        },
    };
    if n < 0 {
        eprintln!("shift: {n}: shift count out of range");
        return 1;
    }
    if !crate::vars::shift(n as usize) {
        return 1;
    }
    0
}

/// `local [name[=value]]...` — declare function-scoped variables: each
/// name's prior value (or absence, for a name that didn't exist yet) is
/// restored automatically when the enclosing function call returns, so a
/// function's own `i=0` no longer permanently clobbers the caller's `i`. A
/// bare `local name` (no `=value`) leaves `name` genuinely unset within the
/// function, matching bash, not merely set to `""`. Only valid inside a
/// function call.
fn local_cmd(argv: &[String]) -> i32 {
    if argv.len() == 1 {
        eprintln!("local: usage: local name[=value] ...");
        return 1;
    }
    let mut status = 0;
    for arg in &argv[1..] {
        let declared = match arg.split_once('=') {
            Some((name, value)) => crate::vars::declare_local(name, Some(value)),
            None => crate::vars::declare_local(arg, None),
        };
        if !declared {
            eprintln!("local: can only be used in a function");
            status = 1;
        }
    }
    status
}

/// `getopts optstring name [arg...]` — POSIX option parsing: `-a`, `-b
/// value`, and combined short flags (`-ab` means `-a -b`). `optstring`
/// lists recognized letters; a letter followed by `:` requires an argument
/// (taken from the rest of the same word if there's more after the letter,
/// else the next whole word). A leading `:` in `optstring` enables
/// "silent" mode: an unknown option sets `name` to `?` and `$OPTARG` to the
/// offending character with no diagnostic printed; a missing argument sets
/// `name` to `:` (same `$OPTARG`). Without a leading `:` (the default),
/// both cases print a diagnostic, set `name` to `?`, and leave `$OPTARG`
/// unset. `$OPTIND` (1-based index of the next word to process) and
/// `$OPTARG` are ordinary shell variables — resetting `OPTIND=1` starts a
/// fresh pass. With no explicit `arg...`, parses the positional parameters.
/// A lone `--` or the first non-option word ends option processing without
/// being consumed, leaving the rest available as ordinary positional args.
fn getopts_cmd(argv: &[String]) -> i32 {
    let (Some(optstring), Some(name)) = (argv.get(1), argv.get(2)) else {
        eprintln!("getopts: usage: getopts optstring name [arg ...]");
        return 2;
    };
    let args: Vec<String> = if argv.len() > 3 { argv[3..].to_vec() } else { crate::vars::args() };

    let silent = optstring.starts_with(':');
    let opts = optstring.trim_start_matches(':');

    let optind = crate::vars::get("OPTIND").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let char_pos = crate::vars::getopts_char_pos(optind);

    // `(new_optind, new_char_pos, exit_status, name's new value, $OPTARG)`.
    let (new_optind, new_char_pos, status, name_value, optarg): (usize, usize, i32, String, Option<String>) = 'outcome: {
        if char_pos == 0 {
            match args.get(optind - 1).map(String::as_str) {
                None => break 'outcome (optind, 0, 1, "?".to_string(), None),
                Some("--") => break 'outcome (optind + 1, 0, 1, "?".to_string(), None),
                Some(w) if !w.starts_with('-') || w == "-" => {
                    break 'outcome (optind, 0, 1, "?".to_string(), None);
                }
                _ => {}
            }
        }

        let chars: Vec<char> = args[optind - 1].chars().collect();
        let pos = if char_pos == 0 { 1 } else { char_pos };
        let opt_char = chars[pos];
        let opt_idx = opts.find(opt_char);
        let takes_arg = opt_idx.is_some_and(|i| opts.as_bytes().get(i + 1) == Some(&b':'));

        if opt_idx.is_none() {
            let optarg = if silent {
                Some(opt_char.to_string())
            } else {
                eprintln!("getopts: illegal option -- {opt_char}");
                None
            };
            let (ni, np) = advance_option_pos(optind, &chars, pos);
            break 'outcome (ni, np, 0, "?".to_string(), optarg);
        }

        if takes_arg {
            let rest: String = chars[pos + 1..].iter().collect();
            if !rest.is_empty() {
                break 'outcome (optind + 1, 0, 0, opt_char.to_string(), Some(rest));
            }
            if let Some(next) = args.get(optind) {
                break 'outcome (optind + 2, 0, 0, opt_char.to_string(), Some(next.clone()));
            }
            let (name_value, optarg) = if silent {
                (":".to_string(), Some(opt_char.to_string()))
            } else {
                eprintln!("getopts: option requires an argument -- {opt_char}");
                ("?".to_string(), None)
            };
            break 'outcome (optind + 1, 0, 0, name_value, optarg);
        }

        let (ni, np) = advance_option_pos(optind, &chars, pos);
        (ni, np, 0, opt_char.to_string(), None)
    };

    crate::vars::set("OPTIND", &new_optind.to_string());
    crate::vars::set_getopts_char_pos(new_optind, new_char_pos);
    crate::vars::set(name, &name_value);
    match optarg {
        Some(v) => crate::vars::set("OPTARG", &v),
        None => crate::vars::unset("OPTARG"),
    }
    status
}

/// Move past the option character at `chars[pos]`: to the next character in
/// the same word if more remain (a combined flag cluster like `-ab`), else
/// to the start of the next word.
fn advance_option_pos(optind: usize, chars: &[char], pos: usize) -> (usize, usize) {
    if pos + 1 < chars.len() {
        (optind, pos + 1)
    } else {
        (optind + 1, 0)
    }
}

/// How a name would resolve — an alias, a reserved word, a function, a
/// builtin, or an executable found on `$PATH` — shared by `command
/// -v`/`-V` and `type`.
enum Kind {
    Alias(String),
    Keyword,
    Function,
    Builtin,
    File(std::path::PathBuf),
}

impl Kind {
    /// `type -t`'s one-word classification.
    fn label(&self) -> &'static str {
        match self {
            Kind::Alias(_) => "alias",
            Kind::Keyword => "keyword",
            Kind::Function => "function",
            Kind::Builtin => "builtin",
            Kind::File(_) => "file",
        }
    }

    /// `command -V`'s / `type`'s human-readable form.
    fn describe(&self, name: &str) -> String {
        match self {
            Kind::Alias(value) => format!("{name} is aliased to `{value}'"),
            Kind::Keyword => format!("{name} is a shell keyword"),
            Kind::Function => format!("{name} is a function"),
            Kind::Builtin => format!("{name} is a shell builtin"),
            Kind::File(path) => format!("{name} is {}", path.display()),
        }
    }

    /// `command -v`'s terser form: an alias prints its definition
    /// (`alias name='value'`); a function or builtin is just its bare
    /// name; a file is its resolved path.
    fn describe_terse(&self, name: &str) -> String {
        match self {
            Kind::Alias(value) => format!("alias {name}='{value}'"),
            Kind::Keyword | Kind::Function | Kind::Builtin => name.to_string(),
            Kind::File(path) => path.display().to_string(),
        }
    }
}

/// Classify `name` the way the shell itself would resolve it as a command —
/// alias, reserved word, function, builtin, or `$PATH` executable, in that
/// precedence order — or `None` if it wouldn't resolve to anything.
/// `keywords` is `false` for `command` (which, per POSIX, only concerns
/// itself with ordinary simple commands, not reserved words) and `true` for
/// `type`.
fn classify(name: &str, keywords: bool) -> Option<Kind> {
    if let Some(value) = crate::alias::get(name) {
        return Some(Kind::Alias(value));
    }
    if keywords && crate::parser::RESERVED.contains(&name) {
        return Some(Kind::Keyword);
    }
    if crate::func::exists(name) {
        return Some(Kind::Function);
    }
    if is_builtin(name) {
        return Some(Kind::Builtin);
    }
    resolve_in_path(name).map(Kind::File)
}

/// Search `$PATH` for an executable file named `name`. A `name` containing
/// `/` is treated as an explicit path (checked directly, not searched for)
/// rather than a `$PATH` lookup.
fn resolve_in_path(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        let p = Path::new(name);
        return is_executable_file(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

/// `command [-v|-V] name [args...]` — with `-v`/`-V`, describes how `name`
/// would resolve (`-v`: terse, the standard `command -v foo` existence
/// check; `-V`: human-readable) without running anything, exiting nonzero
/// if it wouldn't resolve at all. Without either flag, `command` actually
/// *running* `name` (bypassing a shadowing shell function, the whole point
/// of `command` in that form) is handled earlier, at the exec dispatch
/// level (`exec::run_foreground`'s `command_bypass`), before this builtin
/// is ever reached — reaching here with neither flag (e.g. `command` used
/// as one stage of a multi-command pipeline, where that interception
/// doesn't apply — the same "sole command only" limitation every builtin
/// already has) is a narrower case this falls back to reporting rather
/// than executing.
fn command_cmd(argv: &[String]) -> i32 {
    match argv.get(1).map(String::as_str) {
        Some("-v") => command_v(&argv[2..], false),
        Some("-V") => command_v(&argv[2..], true),
        None => 0,
        Some(_) => {
            eprintln!("command: running a command isn't supported here (only as the sole stage of a pipeline)");
            127
        }
    }
}

fn command_v(names: &[String], verbose: bool) -> i32 {
    if names.is_empty() {
        eprintln!("command: usage: command -{} name ...", if verbose { "V" } else { "v" });
        return 2;
    }
    let mut found_any = false;
    for name in names {
        if let Some(kind) = classify(name, false) {
            found_any = true;
            if verbose {
                println!("{}", kind.describe(name));
            } else {
                println!("{}", kind.describe_terse(name));
            }
        }
    }
    if found_any { 0 } else { 1 }
}

/// `type [-t] name ...` — describes how each `name` resolves (an alias, a
/// shell keyword, a function, a builtin, or a `$PATH` executable), printing
/// a diagnostic and reporting failure for any that don't resolve at all.
/// `-t` prints just the one-word classification instead of the full
/// sentence — useful in a script (`[ "$(type -t foo)" = function ]`).
fn type_cmd(argv: &[String]) -> i32 {
    let (just_kind, names) =
        if argv.get(1).map(String::as_str) == Some("-t") { (true, &argv[2..]) } else { (false, &argv[1..]) };
    if names.is_empty() {
        eprintln!("type: usage: type [-t] name ...");
        return 2;
    }
    let mut status = 0;
    for name in names {
        match classify(name, true) {
            Some(kind) => println!("{}", if just_kind { kind.label().to_string() } else { kind.describe(name) }),
            None => {
                eprintln!("type: {name}: not found");
                status = 1;
            }
        }
    }
    status
}

/// `hash [-r] [name...]` — rush never caches `$PATH` lookups (each spawn
/// just searches `$PATH` fresh, so there's no cache to poison), so this is
/// necessarily a narrower stub: `-r` and a bare `hash` are accepted as
/// no-ops (status 0), and `hash name...` reports whether each would
/// currently resolve on `$PATH`, without printing or caching anything.
fn hash_cmd(argv: &[String]) -> i32 {
    if argv.get(1).map(String::as_str) == Some("-r") {
        return 0;
    }
    let names = &argv[1..];
    if names.is_empty() {
        println!("hash: hash table empty");
        return 0;
    }
    let mut status = 0;
    for name in names {
        if resolve_in_path(name).is_none() {
            eprintln!("hash: {name}: not found");
            status = 1;
        }
    }
    status
}

/// `. name [args...]` / `source name [args...]` — run `name`'s commands in
/// the current shell (see `exec::source_file` for the full semantics: no
/// fork, no new variable scope, `$PATH`-searched, positional parameters
/// swapped only when `args` are given, `return` ends just the sourcing).
fn source_cmd(argv: &[String]) -> i32 {
    let Some(name) = argv.get(1) else {
        eprintln!("{}: filename argument required", argv[0]);
        return 2;
    };
    match crate::exec::source_file(name, &argv[2..]) {
        Ok(status) => status,
        Err(e) => {
            eprintln!("{}: {name}: {e}", argv[0]);
            1
        }
    }
}

/// `umask [-S] [mode]` — with no `mode`, report the process's current file
/// creation mask (plain octal, or `u=rwx,g=rx,o=rx`-style with `-S`); with
/// one, set it. A real `libc::umask()` call (Unix only), so it actually
/// affects every file/directory this process (or anything it execs/spawns)
/// creates from here on — not just a shell-internal display value.
#[cfg(unix)]
fn umask_cmd(argv: &[String]) -> i32 {
    let symbolic = argv.get(1).map(String::as_str) == Some("-S");
    let rest = &argv[if symbolic { 2 } else { 1 }..];

    let Some(mode_str) = rest.first() else {
        // `umask()` only ever *sets* the mask, returning the previous value
        // — so reading it without changing it means setting it right back.
        let current = unsafe {
            let m = libc::umask(0);
            libc::umask(m);
            m
        };
        if symbolic {
            println!("{}", symbolic_umask(current));
        } else {
            println!("{current:04o}");
        }
        return 0;
    };

    match u32::from_str_radix(mode_str, 8) {
        Ok(mode) if mode <= 0o777 => {
            unsafe {
                libc::umask(mode as libc::mode_t);
            }
            0
        }
        _ => {
            eprintln!("{}: {mode_str}: octal number out of range", argv[0]);
            1
        }
    }
}

#[cfg(unix)]
fn symbolic_umask(mask: libc::mode_t) -> String {
    let class = |shift: u32| -> String {
        let bits = (mask >> shift) & 0o7;
        let mut s = String::new();
        if bits & 0o4 == 0 {
            s.push('r');
        }
        if bits & 0o2 == 0 {
            s.push('w');
        }
        if bits & 0o1 == 0 {
            s.push('x');
        }
        s
    };
    format!("u={},g={},o={}", class(6), class(3), class(0))
}

#[cfg(not(unix))]
fn umask_cmd(argv: &[String]) -> i32 {
    eprintln!("{}: not supported on this platform", argv[0]);
    1
}

/// `eval arg...` — see `exec::eval_cmd` for the full semantics: the args are
/// joined with a space, parsed, and run in the current shell as if typed
/// inline (no scope at all — no filename, no positional-parameter swap, and
/// `return`/`break`/`continue` propagate straight through, unlike `source`).
fn eval_cmd(argv: &[String]) -> i32 {
    match crate::exec::eval_cmd(&argv[1..]) {
        Ok(status) => status,
        Err(e) => {
            eprintln!("{}: {e}", argv[0]);
            2
        }
    }
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
/// `exec::exec_list_impl`). `set -u` / `set +u` — nounset: referencing an
/// unset variable is an error (see `expand::var_lookup_checked`). `set -o
/// pipefail` / `set +o pipefail` — a pipeline's own exit status is the
/// rightmost non-zero stage rather than just its last (see
/// `exec::pipeline_status`). Other flags/`-o` names aren't implemented yet;
/// naming one is an error rather than a silently-ignored no-op.
fn set_cmd(argv: &[String]) -> i32 {
    let mut status = 0;
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-e" => crate::vars::set_errexit(true),
            "+e" => crate::vars::set_errexit(false),
            "-u" => crate::vars::set_nounset(true),
            "+u" => crate::vars::set_nounset(false),
            "-o" | "+o" => {
                let on = arg == "-o";
                match args.next().map(String::as_str) {
                    Some("pipefail") => crate::vars::set_pipefail(on),
                    Some(name) => {
                        eprintln!("set: {name}: invalid option name");
                        status = 1;
                    }
                    None => {
                        eprintln!("{}: option requires an argument -- 'o'", argv[0]);
                        status = 1;
                    }
                }
            }
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

    /// Render `format` against `args` the way `printf_cmd` does, without
    /// going through stdout — for testing the pure formatting logic
    /// directly (`printf_cmd` itself is covered black-box, against the real
    /// stdout, in `tests/exec_behavior.rs`).
    fn render(format: &str, args: &[&str]) -> String {
        let pieces = printf::parse_format(format).unwrap();
        let consumes_args = pieces.iter().any(|p| matches!(p, printf::Piece::Conv(_)));
        let mut idx = 0;
        let mut out = String::new();
        loop {
            let start_idx = idx;
            for piece in &pieces {
                match piece {
                    printf::Piece::Literal(s) => out.push_str(s),
                    printf::Piece::Conv(conv) => {
                        let arg = args.get(idx).copied();
                        if arg.is_some() {
                            idx += 1;
                        }
                        out.push_str(&printf::format_conv(conv, arg.unwrap_or("")).0);
                    }
                }
            }
            if !consumes_args || idx >= args.len() || idx == start_idx {
                break;
            }
        }
        out
    }

    #[test]
    fn printf_matches_real_bash() {
        // Format-string escapes, resolved once, up front.
        assert_eq!(render("a\\tb\\nc\\\\d", &[]), "a\tb\nc\\d");

        // Width/flags on integers.
        assert_eq!(render("%5d|%-5d|%05d", &["3", "3", "3"]), "    3|3    |00003");
        assert_eq!(render("%+d % d", &["5", "5"]), "+5  5");
        assert_eq!(render("%3d", &["-5"]), " -5");
        assert_eq!(render("%03d", &["-5"]), "-05");

        // `%o`/`%x`/`%X`/`%u`, including a negative number reinterpreted as
        // unsigned (two's complement), matching real bash.
        assert_eq!(render("%x %o %X", &["255", "8", "255"]), "ff 10 FF");
        assert_eq!(render("%x", &["-1"]), "ffffffffffffffff");

        // `%s` precision truncates; `%c` takes just the first character.
        assert_eq!(render("%.3s", &["hello"]), "hel");
        assert_eq!(render("%c", &["hello"]), "h");

        // `%b` processes backslash escapes in *its* argument (unlike `%s`).
        assert_eq!(render("%b", &["a\\tb\\nc"]), "a\tb\nc");
        assert_eq!(render("%s", &["a\\tb"]), "a\\tb");

        // `%%` is a literal percent, consuming no argument.
        assert_eq!(render("100%%", &[]), "100%");

        // No argument-consuming conversion at all: extra args are ignored,
        // format runs exactly once.
        assert_eq!(render("no conversions here", &["a", "b", "c"]), "no conversions here");

        // More arguments than the format consumes: it cycles, with the
        // final (partial) cycle defaulting whatever's missing.
        assert_eq!(render("%s-%d,", &["a", "1", "b", "2", "c"]), "a-1,b-2,c-0,");

        // Fewer arguments than conversions need: missing ones default to
        // `""`/`0`, not an error.
        assert_eq!(render("[%s][%d]", &[]), "[][0]");
    }

    #[test]
    fn printf_invalid_number_reports_error_but_still_formats() {
        let pieces = printf::parse_format("%d").unwrap();
        let printf::Piece::Conv(conv) = &pieces[0] else { panic!("expected a conversion") };
        let (text, err) = printf::format_conv(conv, "abc");
        assert_eq!(text, "0");
        assert_eq!(err.as_deref(), Some("abc: invalid number"));
    }
}
