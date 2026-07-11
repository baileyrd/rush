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
        // `local` is dispatched earlier, straight off the expanded `Command`
        // (`exec::dispatch_builtin`), never through here — see
        // `local_from_decls`'s own doc comment for why.
        "getopts" => Some(getopts_cmd(argv)),
        "command" => Some(command_cmd(argv)),
        "type" => Some(type_cmd(argv)),
        "hash" => Some(hash_cmd(argv)),
        "." | "source" => Some(source_cmd(argv)),
        "eval" => Some(eval_cmd(argv)),
        "exec" => Some(crate::exec::exec_cmd(argv)),
        "umask" => Some(umask_cmd(argv)),
        "ulimit" => Some(ulimit_cmd(argv)),
        "shopt" => Some(shopt_cmd(argv)),
        "mapfile" | "readarray" => Some(mapfile_cmd(argv)),
        "abbr" => Some(abbr_cmd(argv)),
        "pushd" => Some(pushd_cmd(argv)),
        "popd" => Some(popd_cmd(argv)),
        "dirs" => Some(dirs_cmd(argv)),
        "unabbr" => Some(unabbr_cmd(argv)),
        "let" => Some(let_cmd(argv)),
        "history" => Some(history_cmd(argv)),
        // `builtin name [args...]` (C92): run the builtin directly, so a
        // wrapper function can call the thing it shadows without recursing.
        "builtin" => Some(match argv.get(1) {
            None => 0,
            Some(name) if is_builtin(name) => try_run(&argv[1..]).unwrap_or(1),
            Some(name) => {
                eprintln!("builtin: {name}: not a shell builtin");
                1
            }
        }),
        _ => other_builtin(argv),
    }
}

/// Names `try_run` dispatches directly (excludes the platform-specific ones
/// in `other_builtin`, e.g. `job`'s `jobs`/`fg`/`bg`/`kill` on Unix).
pub const NAMES: &[&str] = &[
    "cd", "pwd", "echo", "export", "unset", "test", "[", "break", "continue", "return", "true",
    ":", "false", "exit", "alias", "unalias", "set", "trap", "read", "printf", "shift", "local",
    "getopts", "command", "type", "hash", ".", "source", "eval", "exec", "umask", "ulimit", "shopt",
    "mapfile", "readarray", "abbr", "unabbr", "pushd", "popd", "dirs", "declare", "typeset",
    "readonly", "let", "builtin", "history",
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
    // Leading flags: `-n` (no newline), `-e` (interpret escapes), `-E`
    // (don't — the default), clustered in any combination (`-en`, `-nE`)
    // (C90). The first argument that isn't purely those letters ends flag
    // parsing and prints literally, matching bash (`echo -x` prints `-x`).
    let mut newline = true;
    // `xpg_echo` (C108): escapes are interpreted by default.
    let mut escapes = crate::vars::shopt("xpg_echo");
    let mut idx = 1;
    while idx < argv.len() {
        let Some(flags) = argv[idx]
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 'n' | 'e' | 'E')))
        else {
            break;
        };
        for c in flags.chars() {
            match c {
                'n' => newline = false,
                'e' => escapes = true,
                'E' => escapes = false,
                _ => unreachable!(),
            }
        }
        idx += 1;
    }

    let mut line = argv[idx..].join(" ");
    if escapes {
        // `\c` means "stop output here, no newline" in echo (unlike
        // `$'...'`, where `\cX` is a control character) — find the first
        // unescaped one before handing off to the shared unescaper.
        let mut cut = None;
        let mut chars = line.char_indices();
        while let Some((i, c)) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some((_, 'c')) => {
                        cut = Some(i);
                        break;
                    }
                    _ => continue,
                }
            }
        }
        if let Some(i) = cut {
            line.truncate(i);
            newline = false;
        }
        line = crate::expand::ansi_unescape(&line);
    }
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
/// The `%(fmt)T` strftime subset, exposed for the prompt's `\t`-family
/// escapes (C125).
pub(crate) fn strftime_utc(fmt: &str, epoch: i64) -> String {
    printf::strftime(fmt, epoch)
}

fn printf_cmd(argv: &[String]) -> i32 {
    // `printf -v var format args...` (C99): format into `var` (or
    // `arr[i]`) instead of stdout — the standard no-subshell formatter.
    let (target, fmt_idx) = if argv.get(1).map(String::as_str) == Some("-v") {
        match argv.get(2) {
            Some(name) => (Some(name.clone()), 3),
            None => {
                eprintln!("printf: -v: option requires an argument");
                return 2;
            }
        }
    } else {
        (None, 1)
    };
    let Some(format) = argv.get(fmt_idx) else {
        eprintln!("printf: usage: printf [-v var] format [arguments]");
        return 2;
    };
    let args = &argv[fmt_idx + 1..];

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

    match target {
        Some(name) => match name.split_once('[') {
            Some((base, rest)) if rest.ends_with(']') => {
                crate::vars::key_set(base, &rest[..rest.len() - 1], &out);
            }
            _ => crate::vars::set(&name, &out),
        },
        None => {
            print!("{out}");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
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
        /// `%(fmt)T`'s strftime-style format (C99).
        time_fmt: Option<String>,
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

        // `%(fmt)T` (C99): a strftime-style date conversion whose format
        // rides inside the parens.
        if chars.peek() == Some(&'(') {
            chars.next();
            let mut fmt = String::new();
            loop {
                match chars.next() {
                    Some(')') => break,
                    Some(c) => fmt.push(c),
                    None => return Err("`%(': missing `)T'".to_string()),
                }
            }
            if chars.next() != Some('T') {
                return Err("`%(': expected `T' after `)'".to_string());
            }
            conv.spec = 'T';
            conv.time_fmt = Some(fmt);
            return Ok(conv);
        }
        conv.spec = chars.next().ok_or("missing conversion specifier")?;
        if !"diouxXcsbq".contains(conv.spec) {
            return Err(format!("`%{}': invalid conversion specification", conv.spec));
        }
        Ok(conv)
    }

    /// A strftime subset for `%(fmt)T` (C99), UTC: civil date via the
    /// days-from-epoch algorithm, no timezone database.
    pub(crate) fn strftime(fmt: &str, epoch: i64) -> String {
        const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June",
            "July", "August", "September", "October", "November", "December"];
        const DAYS: [&str; 7] =
            ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
        let days = epoch.div_euclid(86400);
        let secs_of_day = epoch.rem_euclid(86400);
        let (hour, min, sec) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);
        // Howard Hinnant's civil-from-days.
        let z = days + 719468;
        let era = z.div_euclid(146097);
        let doe = z.rem_euclid(146097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };
        let weekday = (days + 4).rem_euclid(7); // epoch day 0 = Thursday
        let jan1 = {
            // day-of-year: days since Jan 1 of `year`
            let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            let cum = [0i64, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
            cum[(m - 1) as usize] + d + i64::from(is_leap && m > 2)
        };
        let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };

        let mut out = String::new();
        let mut chars = fmt.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('Y') => out.push_str(&year.to_string()),
                Some('y') => out.push_str(&format!("{:02}", year.rem_euclid(100))),
                Some('m') => out.push_str(&format!("{m:02}")),
                Some('d') => out.push_str(&format!("{d:02}")),
                Some('e') => out.push_str(&format!("{d:2}")),
                Some('j') => out.push_str(&format!("{jan1:03}")),
                Some('H') => out.push_str(&format!("{hour:02}")),
                Some('I') => out.push_str(&format!("{hour12:02}")),
                Some('M') => out.push_str(&format!("{min:02}")),
                Some('S') => out.push_str(&format!("{sec:02}")),
                Some('s') => out.push_str(&epoch.to_string()),
                Some('p') => out.push_str(if hour < 12 { "AM" } else { "PM" }),
                Some('a') => out.push_str(&DAYS[weekday as usize][..3]),
                Some('A') => out.push_str(DAYS[weekday as usize]),
                Some('b') | Some('h') => out.push_str(&MONTHS[(m - 1) as usize][..3]),
                Some('B') => out.push_str(MONTHS[(m - 1) as usize]),
                Some('u') => out.push_str(&(if weekday == 0 { 7 } else { weekday }).to_string()),
                Some('w') => out.push_str(&weekday.to_string()),
                Some('T') | Some('X') => out.push_str(&format!("{hour:02}:{min:02}:{sec:02}")),
                Some('R') => out.push_str(&format!("{hour:02}:{min:02}")),
                Some('D') => out.push_str(&format!("{m:02}/{d:02}/{:02}", year.rem_euclid(100))),
                Some('F') => out.push_str(&format!("{year}-{m:02}-{d:02}")),
                Some('Z') => out.push_str("UTC"),
                Some('z') => out.push_str("+0000"),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    }

    /// `%q`'s quoting: backslash-escape shell-special characters
    /// (bash/zsh's own style; ksh93 prefers single quotes — a cosmetic,
    /// documented difference), except control characters, which force the
    /// `$'...'` form.
    fn shell_quote_q(raw: &str) -> String {
        if raw.is_empty() {
            return "''".to_string();
        }
        if raw.chars().any(|c| c.is_control()) {
            let mut out = String::from("$'");
            for c in raw.chars() {
                match c {
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    '\\' => out.push_str("\\\\"),
                    '\'' => out.push_str("\\'"),
                    c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('\'');
            return out;
        }
        let mut out = String::new();
        for c in raw.chars() {
            if "|&;<>()$`\\\"' *?[]#~=%!{}".contains(c) || c == ' ' {
                out.push('\\');
            }
            out.push(c);
        }
        out
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
            // A leading `'` or `"` means the next character's codepoint —
            // the standard char-to-number idiom (`printf %d "'A"` → 65)
            // (C99).
            s if s.starts_with('\'') || s.starts_with('"') => {
                s.chars().nth(1).map(|c| c as i64).unwrap_or(0)
            }
            s => s.parse().unwrap_or_else(|_| {
                error = Some(format!("{raw}: invalid number"));
                0
            }),
        };

        let (body, is_numeric) = match conv.spec {
            's' => (truncate(raw, conv.precision), false),
            // `%q` (C63): quote for reuse as shell input — bash/zsh's
            // backslash style for special characters, `''` for an empty
            // argument, `$'...'` when control characters are present
            // (each verified against bash).
            'q' => (shell_quote_q(raw), false),
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
            // `%(fmt)T` (C99): the argument is epoch seconds (`-1`/empty =
            // now; `-2` approximated as now too — rush doesn't record its
            // start instant). Rendered in UTC — rush carries no timezone
            // database, a documented narrowing.
            'T' => {
                let secs = match raw.trim() {
                    "" | "-1" | "-2" => std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0),
                    _ => parse_int(),
                };
                (strftime(conv.time_fmt.as_deref().unwrap_or("%X"), secs), false)
            }
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

thread_local! {
    // The directory stack (`pushd`/`popd`/`dirs`, C72) — entries *below*
    // the current directory (which bash treats as the implicit top).
    static DIR_STACK: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The stack as bash's `dirs` shows it, one entry per element: the current
/// directory first, then the pushed entries, `$HOME` abbreviated to `~`.
fn dirs_entries() -> Vec<String> {
    let mut entries = vec![std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()];
    DIR_STACK.with(|s| entries.extend(s.borrow().iter().rev().cloned()));
    let home = crate::vars::get("HOME").unwrap_or_default();
    entries
        .iter()
        .map(|e| {
            if !home.is_empty() && e.starts_with(&home) {
                format!("~{}", &e[home.len()..])
            } else {
                e.clone()
            }
        })
        .collect()
}

fn dirs_display() -> String {
    dirs_entries().join(" ")
}

fn current_dir_string() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.display().to_string())
}

/// `pushd [dir]` (C72): with a dir, cd there and push the old cwd; with
/// none, swap the top two entries — printing the stack either way, like
/// bash.
fn pushd_cmd(argv: &[String]) -> i32 {
    let prev = current_dir_string();
    match argv.get(1) {
        Some(dir) => {
            if cd(&["cd".to_string(), dir.clone()]) != 0 {
                return 1;
            }
            if let Some(p) = prev {
                DIR_STACK.with(|s| s.borrow_mut().push(p));
            }
        }
        None => {
            let Some(top) = DIR_STACK.with(|s| s.borrow_mut().pop()) else {
                eprintln!("pushd: no other directory");
                return 1;
            };
            if cd(&["cd".to_string(), top]) != 0 {
                return 1;
            }
            if let Some(p) = prev {
                DIR_STACK.with(|s| s.borrow_mut().push(p));
            }
        }
    }
    println!("{}", dirs_display());
    0
}

/// `popd [+N]` (C72): drop the current directory and cd to the next stack
/// entry — or, with `+N` (C101), remove the Nth entry of `dirs`' listing
/// (0 = the current directory, so `+0` is the plain form; higher indices
/// remove without changing directory, bash's own rule).
fn popd_cmd(argv: &[String]) -> i32 {
    if let Some(n) = argv.get(1).and_then(|a| a.strip_prefix('+')).and_then(|r| r.parse::<usize>().ok())
        && n >= 1
    {
        let removed = DIR_STACK.with(|s| {
            let mut s = s.borrow_mut();
            let len = s.len();
            (n <= len).then(|| s.remove(len - n))
        });
        if removed.is_none() {
            eprintln!("popd: +{n}: directory stack index out of range");
            return 1;
        }
        println!("{}", dirs_display());
        return 0;
    }
    let Some(next) = DIR_STACK.with(|s| s.borrow_mut().pop()) else {
        eprintln!("popd: directory stack empty");
        return 1;
    };
    if cd(&["cd".to_string(), next]) != 0 {
        return 1;
    }
    println!("{}", dirs_display());
    0
}

/// `dirs [-c] [-p] [-v]` (C72): print the stack — `-c` clears it, `-p`
/// one per line, `-v` one per line with the index column (C101).
fn dirs_cmd(argv: &[String]) -> i32 {
    match argv.get(1).map(String::as_str) {
        Some("-c") => {
            DIR_STACK.with(|s| s.borrow_mut().clear());
            0
        }
        Some("-v") => {
            for (i, e) in dirs_entries().iter().enumerate() {
                println!(" {i}  {e}");
            }
            0
        }
        Some("-p") => {
            for e in dirs_entries() {
                println!("{e}");
            }
            0
        }
        _ => {
            println!("{}", dirs_display());
            0
        }
    }
}

/// Levenshtein distance, for `cd`'s interactive spelling correction (C72).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `cd`'s not-found fallbacks (C72): `$CDPATH` for a bare relative name
/// (POSIX — the resulting directory is printed, as POSIX requires), then
/// — interactively only, like fish — a unique close-spelling sibling
/// (edit distance ≤ 2 with at most one candidate).
fn cd_fallback_target(target: &str) -> Option<String> {
    let relative_bare = !target.starts_with(['/', '.']);
    if relative_bare && let Some(cdpath) = crate::vars::get("CDPATH") {
        for dir in cdpath.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = format!("{dir}/{target}");
            if Path::new(&candidate).is_dir() {
                println!("{candidate}");
                return Some(candidate);
            }
        }
    }
    if crate::vars::interactive() {
        let path = Path::new(target);
        let (parent, name) = (path.parent().filter(|p| !p.as_os_str().is_empty()), path.file_name()?);
        let parent = parent.unwrap_or(Path::new("."));
        let name = name.to_string_lossy();
        let mut candidates = Vec::new();
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let entry_name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir()
                    && (entry_name.eq_ignore_ascii_case(&name) || edit_distance(&entry_name, &name) <= 2)
                {
                    candidates.push(entry.path().display().to_string());
                }
            }
        }
        if let [only] = candidates.as_slice() {
            eprintln!("cd: corrected to {only}");
            return Some(only.clone());
        }
    }
    None
}

fn cd(argv: &[String]) -> i32 {
    // `-L`/`-P`/`-e` are accepted and skipped (C101): rush always tracks
    // the physical directory (`std::env` has no logical-path notion), so
    // `-P` is the native behavior and `-L` a documented approximation.
    let mut argv = argv.to_vec();
    while argv.len() > 1 && matches!(argv[1].as_str(), "-L" | "-P" | "-e") {
        argv.remove(1);
    }
    let argv = &argv[..];
    // `cd -` goes to $OLDPWD and echoes it, like POSIX `cd`. `cd -N`
    // (C72) jumps to the Nth directory-stack entry (1-based, `dirs`'
    // order), like zsh.
    let going_back = argv.get(1).map(String::as_str) == Some("-");
    if let Some(n) = argv
        .get(1)
        .and_then(|a| a.strip_prefix('-'))
        .filter(|r| !r.is_empty())
        .and_then(|r| r.parse::<usize>().ok())
    {
        let Some(target) = DIR_STACK.with(|s| {
            let s = s.borrow();
            s.iter().rev().nth(n.saturating_sub(1)).cloned()
        }) else {
            eprintln!("cd: -{n}: directory stack index out of range");
            return 1;
        };
        return cd(&["cd".to_string(), target]);
    }
    let target = match argv.get(1) {
        Some(_) if going_back => match crate::vars::get("OLDPWD") {
            Some(dir) => dir,
            None => {
                eprintln!("cd: OLDPWD not set");
                return 1;
            }
        },
        Some(dir) => dir.clone(),
        // `vars::get` alone: seeded from the inherited environment at
        // startup (C36), so this already sees a real, unexported `HOME`
        // too (verified directly against real bash: `HOME=/tmp; cd` does
        // change directory) — and correctly stops seeing it after `unset`
        // (C40), instead of a `std::env` fallback resurrecting it.
        None => match crate::vars::get("HOME") {
            Some(h) => h,
            None => {
                eprintln!("cd: HOME not set");
                return 1;
            }
        },
    };

    let previous = std::env::current_dir().ok();

    if let Err(e) = std::env::set_current_dir(Path::new(&target)) {
        // Not found: try `$CDPATH`, then (interactively) spelling
        // correction (C72), before giving up.
        match cd_fallback_target(&target) {
            Some(fallback) if std::env::set_current_dir(Path::new(&fallback)).is_ok() => {}
            _ => {
                eprintln!("cd: {target}: {e}");
                return 1;
            }
        }
    }

    if let Some(dir) = previous {
        crate::vars::set("OLDPWD", &dir.display().to_string());
    }
    // Keep `$PWD` current too — it arrives exported from the parent
    // process, so without this every `cd` leaves it stale (found while
    // wiring `~+`, which reads it).
    if let Ok(dir) = std::env::current_dir() {
        crate::vars::set("PWD", &dir.display().to_string());
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
    let mut status = 0;
    // `-n` un-exports (C98); `-f` would export functions, which needs
    // function-source serialization rush doesn't have yet — warn loudly
    // instead of silently pretending (a child genuinely won't see it).
    match argv.get(1).map(String::as_str) {
        Some("-n") => {
            for name in &argv[2..] {
                crate::vars::unexport(name);
            }
            return 0;
        }
        Some("-f") => {
            // Function export (C98): store the body in the environment as
            // `BASH_FUNC_name%%=() { ... }` — bash's own encoding, so an
            // exported function crosses into child rush *and* bash shells
            // alike (and `import_functions` reads it back at startup).
            let mut status = 0;
            for name in &argv[2..] {
                match crate::func::get(name) {
                    Some(body) => crate::vars::set_exported(
                        &format!("BASH_FUNC_{name}%%"),
                        &crate::unparse::function_export_value(&body),
                    ),
                    None => {
                        eprintln!("export: {name}: not a function");
                        status = 1;
                    }
                }
            }
            return status;
        }
        _ => {}
    }
    for arg in &argv[1..] {
        match arg.split_once('=') {
            // `export x=2` on a readonly `x` fails with status 1 but does
            // NOT abort the script (unlike a bare assignment) — verified
            // against real bash. A bare `export x` on a readonly name is
            // fine (it only adds the export flag).
            Some((name, _)) if crate::vars::is_readonly(name) => {
                eprintln!("export: {name}: readonly variable");
                status = 1;
            }
            Some((name, value)) => crate::vars::set_exported(name, value),
            None => crate::vars::export(arg),
        }
    }
    status
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
    // Full flag set (C89): -r raw, -p prompt, -s silent, -t timeout,
    // -n/-N char counts, -d delimiter, -a array, -u fd, -e (accepted,
    // no readline editing — a documented approximation). Values may be
    // attached (`-t5`) or separate (`-t 5`), same as bash.
    let mut opts = ReadOpts::default();
    let mut idx = 1;
    while idx < argv.len() {
        let arg = argv[idx].as_str();
        if arg == "--" {
            idx += 1;
            break;
        }
        if !arg.starts_with('-') || arg.len() < 2 {
            break;
        }
        let mut value_for = |flag: char, rest: &str, idx: &mut usize| -> Option<String> {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
            *idx += 1;
            let v = argv.get(*idx).cloned();
            if v.is_none() {
                eprintln!("read: -{flag}: option requires an argument");
            }
            v
        };
        match (&arg[1..2], &arg[2..]) {
            ("r", "") => opts.raw = true,
            ("s", "") => opts.silent = true,
            ("e", "") => {} // readline editing: plain read, documented
            ("p", rest) => match value_for('p', rest, &mut idx) {
                Some(v) => opts.prompt = Some(v),
                None => return 2,
            },
            ("t", rest) => match value_for('t', rest, &mut idx).map(|v| v.parse::<f64>()) {
                Some(Ok(v)) if v >= 0.0 => opts.timeout = Some(v),
                _ => {
                    eprintln!("read: invalid timeout specification");
                    return 2;
                }
            },
            ("n", rest) | ("N", rest) => {
                let ignore_delim = &arg[1..2] == "N";
                match value_for('n', rest, &mut idx).map(|v| v.parse::<usize>()) {
                    Some(Ok(n)) => opts.nchars = Some((n, ignore_delim)),
                    _ => {
                        eprintln!("read: invalid number");
                        return 2;
                    }
                }
            }
            ("d", rest) => match value_for('d', rest, &mut idx) {
                // An empty delimiter means NUL, same as bash.
                Some(v) => opts.delim = v.bytes().next().unwrap_or(0),
                None => return 2,
            },
            ("a", rest) => match value_for('a', rest, &mut idx) {
                Some(v) => opts.array = Some(v),
                None => return 2,
            },
            ("u", rest) => match value_for('u', rest, &mut idx).map(|v| v.parse::<i32>()) {
                Some(Ok(fd)) if fd >= 0 => opts.fd = fd,
                _ => {
                    eprintln!("read: invalid file descriptor specification");
                    return 2;
                }
            },
            _ => {
                eprintln!("read: {arg}: invalid option");
                return 2;
            }
        }
        idx += 1;
    }

    let mut names: Vec<&str> = argv[idx..].iter().map(String::as_str).collect();
    if names.is_empty() {
        names.push("REPLY");
    }

    // Prompt only when the input is a terminal, like bash.
    let tty = unsafe { crate::sys::isatty(opts.fd) } == 1;
    if let Some(prompt) = &opts.prompt
        && tty
    {
        eprint!("{prompt}");
    }
    // `-s` on a terminal: shell out to `stty` to stop echo — rush carries
    // no termios binding of its own (documented approximation).
    let silenced = opts.silent && tty && opts.fd == 0
        && std::process::Command::new("stty").arg("-echo").status().is_ok_and(|s| s.success());

    let (line, protected, status) = read_logical_line(&opts);

    if silenced {
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
    }

    if let Some(array) = &opts.array {
        let fields = split_read_fields(&line, &protected);
        let values: Vec<String> = fields
            .iter()
            .map(|f| String::from_utf8_lossy(&line[f.start..f.end]).into_owned())
            .collect();
        crate::vars::set_array(array, values);
        return status;
    }
    let fields = split_read_fields(&line, &protected);
    assign_read_fields(&names, &line, &fields);
    status
}

/// `read`'s parsed flags (C89) — see `read_cmd`.
struct ReadOpts {
    raw: bool,
    silent: bool,
    prompt: Option<String>,
    timeout: Option<f64>,
    /// `(n, ignore_delim)`: `-n` stops at the delimiter too, `-N` doesn't.
    nchars: Option<(usize, bool)>,
    delim: u8,
    array: Option<String>,
    fd: i32,
}

impl Default for ReadOpts {
    fn default() -> Self {
        ReadOpts {
            raw: false,
            silent: false,
            prompt: None,
            timeout: None,
            nchars: None,
            delim: b'\n',
            array: None,
            fd: 0,
        }
    }
}

/// `select`'s own line read: same backslash-continuation processing as
/// plain `read` (`read_logical_line(false)`, verified directly — `a\<NL>b`
/// still joins into `ab`), but the raw line becomes `$REPLY` *unsplit* and
/// *untrimmed* — unlike ordinary `read`'s "last name absorbs the remainder,
/// trimmed of surrounding `$IFS` whitespace" rule (verified directly: three
/// bare spaces as the whole line come back in `$REPLY` as three spaces,
/// where `read reply` on the same input would trim it to empty). "Blank"
/// (for the menu-redisplay rule) is the caller's job: it means this
/// returned line is zero-length, not merely all-whitespace.
/// `mapfile [-t] [array]` / `readarray [-t] [array]` (C61) — read every
/// line from stdin into an indexed array (`MAPFILE` when no name is
/// given). Without `-t` each element keeps its trailing newline; with it
/// the delimiter is stripped — the one flag that matters in real usage
/// (`-d`/`-n`/`-s`/`-O`/`-c`/`-C` are documented waits, per the item's
/// own scoping). Reads raw (no backslash processing), one byte at a time
/// off fd 0 via the same primitive `read` uses, so it never over-consumes.
fn mapfile_cmd(argv: &[String]) -> i32 {
    let mut strip = false;
    let mut delim = b'\n';
    let mut name: Option<&str> = None;
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-t" => strip = true,
            // `-d DELIM` (C89): split on DELIM instead of newline; an
            // empty argument means NUL, same as bash.
            "-d" => match args.next() {
                Some(d) => delim = d.bytes().next().unwrap_or(0),
                None => {
                    eprintln!("{}: -d: option requires an argument", argv[0]);
                    return 2;
                }
            },
            other if other.starts_with("-d") => delim = other[2..].bytes().next().unwrap_or(0),
            other if other.starts_with('-') => {
                eprintln!("{}: {other}: only -t and -d are supported", argv[0]);
                return 2;
            }
            other if name.is_none() => name = Some(other),
            other => {
                eprintln!("{}: {other}: too many arguments", argv[0]);
                return 2;
            }
        }
    }
    let name = name.unwrap_or("MAPFILE");
    let mut valid = name.chars().next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
    valid &= name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
    if !valid {
        eprintln!("{}: {name}: not a valid identifier", argv[0]);
        return 1;
    }
    let mut lines = Vec::new();
    let raw_opts = ReadOpts { raw: true, delim, ..ReadOpts::default() };
    loop {
        let (bytes, _, status) = read_logical_line(&raw_opts);
        let hit_eof = status != 0;
        let mut line = String::from_utf8_lossy(&bytes).into_owned();
        if line.is_empty() && hit_eof {
            break;
        }
        // `hit_eof` means this line was unterminated — it has no trailing
        // delimiter to keep even without `-t` (matching bash).
        if !strip && !hit_eof {
            line.push(delim as char);
        }
        lines.push(line);
        if hit_eof {
            break;
        }
    }
    crate::vars::set_array(name, lines);
    0
}

pub(crate) fn read_reply_line() -> (String, bool) {
    let (line, _protected, status) = read_logical_line(&ReadOpts::default());
    (String::from_utf8_lossy(&line).into_owned(), status != 0)
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
/// Returns `(bytes, protected, exit_status)`: 0 = delimiter reached (or
/// the `-n`/`-N` count filled), 1 = EOF, 142 (128+SIGALRM) = `-t` timeout
/// — matching bash's statuses. Reads byte-at-a-time off `opts.fd` so a
/// loop of `read` calls never over-consumes.
fn read_logical_line(opts: &ReadOpts) -> (Vec<u8>, Vec<bool>, i32) {
    let mut line = Vec::new();
    let mut protected = Vec::new();

    // `-t`: a whole-call deadline, enforced by putting the fd in
    // non-blocking mode and polling (Linux constants; the default
    // backend is Linux-only — elsewhere `-t` degrades to blocking).
    let deadline = opts.timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs_f64(t));
    #[cfg(target_os = "linux")]
    let restore_flags = deadline.and_then(|_| {
        const F_GETFL: i32 = 3;
        const F_SETFL: i32 = 4;
        const O_NONBLOCK: i32 = 0o4000;
        let flags = unsafe { crate::sys::fcntl(opts.fd, F_GETFL, 0) };
        (flags != -1
            && unsafe { crate::sys::fcntl(opts.fd, F_SETFL, flags | O_NONBLOCK) } != -1)
            .then_some((F_SETFL, flags))
    });
    #[cfg(not(target_os = "linux"))]
    let restore_flags: Option<(i32, i32)> = None;

    let finish = |line: Vec<u8>, protected: Vec<bool>, status: i32| {
        if let Some((setfl, flags)) = restore_flags {
            unsafe {
                crate::sys::fcntl(opts.fd, setfl, flags);
            }
        }
        (line, protected, status)
    };

    let read_byte = |deadline: Option<std::time::Instant>| -> Result<Option<u8>, i32> {
        let mut byte = [0u8; 1];
        loop {
            let n = unsafe {
                let mut f = std::mem::ManuallyDrop::new(<std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(opts.fd));
                std::io::Read::read(&mut *f, &mut byte)
            };
            match n {
                Ok(0) => return Ok(None),
                Ok(_) => return Ok(Some(byte[0])),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    match deadline {
                        Some(d) if std::time::Instant::now() >= d => return Err(142),
                        _ => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(None),
            }
        }
    };

    loop {
        if let Some((n, _)) = opts.nchars
            && line.len() >= n
        {
            return finish(line, protected, 0);
        }
        let b = match read_byte(deadline) {
            Ok(Some(b)) => b,
            Ok(None) => return finish(line, protected, 1),
            Err(status) => return finish(line, protected, status),
        };
        let ignore_delim = matches!(opts.nchars, Some((_, true)));
        if b == opts.delim && !ignore_delim {
            return finish(line, protected, 0);
        }
        if !opts.raw && b == b'\\' {
            match read_byte(deadline) {
                Ok(None) => return finish(line, protected, 1),
                Err(status) => return finish(line, protected, status),
                Ok(Some(next)) => {
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
        // `vars::get` alone (no `std::env` fallback, see `expand.rs`'s
        // `var_raw` for why — C36/C40) — `IFS` is essentially never a real
        // environment variable anyway, so this was always closer to
        // vestigial than load-bearing.
        match crate::vars::get("IFS") {
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

const UNARY_OPS: &[&str] = &["-z", "-n", "-e", "-f", "-d", "-s", "-r", "-w", "-x", "-v", "-o", "-R"];
const BINARY_OPS: &[&str] =
    &["=", "==", "!=", "-eq", "-ne", "-lt", "-le", "-gt", "-ge", "<", ">"];

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

/// `[[ ]]`'s unary operators (C55) — `test`'s set plus `-a` (exists;
/// unary-only inside `[[`, where the flat `-a` combinator doesn't exist)
/// and `-h`/`-L` (symlink — `symlink_metadata`, so a dangling link still
/// reports true, same as bash).
pub(crate) fn cond_unary(op: &str, s: &str) -> Result<bool, String> {
    use std::path::Path;
    match op {
        "-a" => Ok(Path::new(s).exists()),
        "-h" | "-L" => Ok(std::fs::symlink_metadata(s).is_ok_and(|m| m.file_type().is_symlink())),
        _ => test_unary(op, s),
    }
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
        // `-v name` — is the variable set (C95)? An `arr[i]` form checks
        // that one element, same as bash.
        "-v" => match crate::expand::parse_array_unset_index(s) {
            Ok(Some(crate::expand::UnsetTarget::Index(a, i))) => {
                crate::vars::array_get(&a, i).is_some()
            }
            Ok(Some(crate::expand::UnsetTarget::Key(a, k))) => {
                crate::vars::assoc_get(&a, &k).is_some()
            }
            _ => crate::vars::get(s).is_some() || crate::vars::array_len(s) > 0,
        },
        // `-o name` — is the named shell option on (C95)? Unknown names
        // are simply false, same as bash.
        "-o" => named_option_state(s).unwrap_or(false),
        // `-R name` — set *and* a nameref (C95).
        "-R" => crate::vars::nameref_target(s).is_some() && crate::vars::get(s).is_some(),
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
        // Lexicographic string comparison (C95) — reached as `\<`/`\>` or
        // quoted, since an unquoted `<`/`>` is a redirection first.
        "<" => a < b,
        ">" => a > b,
        _ => return Err(format!("unknown operator `{op}`")),
    })
}

/// The state of a `set -o`-style long-named option, `None` for an unknown
/// name — shared by `test -o` (C95) and `list_options`' own table.
fn named_option_state(name: &str) -> Option<bool> {
    Some(match name {
        "allexport" => crate::vars::allexport(),
        "noglob" => crate::vars::noglob(),
        "errexit" => crate::vars::errexit(),
        "noclobber" => crate::vars::noclobber(),
        "noexec" => crate::vars::noexec(),
        "nounset" => crate::vars::nounset(),
        "pipefail" => crate::vars::pipefail(),
        "vi" => crate::vars::edit_mode_vi(),
        "emacs" => !crate::vars::edit_mode_vi(),
        "xtrace" => crate::vars::xtrace(),
        _ => return None,
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
/// `let expr...` (C91) — each argument is a full arithmetic expression
/// (assignments included, so `let i++` and `let "n = n + 1"` mutate the
/// variable); status is 1 when the *last* expression evaluates to 0,
/// 0 otherwise — the exact inverse-truth rule `(( ))` uses.
fn let_cmd(argv: &[String]) -> i32 {
    let exprs = &argv[1..];
    if exprs.is_empty() {
        eprintln!("let: expression expected");
        return 1;
    }
    let mut last = 0;
    for expr in exprs {
        match crate::arith::eval(expr) {
            Ok(n) => last = n,
            Err(e) => {
                eprintln!("let: {e}");
                return 1;
            }
        }
    }
    i32::from(last == 0)
}

fn return_cmd(argv: &[String]) -> i32 {
    // Outside any function or sourced file, `return` is an error that the
    // script survives — it used to silently exit the whole shell (C88).
    if crate::vars::function_depth() == 0 && crate::vars::sourcing_depth() == 0 {
        eprintln!("return: can only `return' from a function or sourced script");
        return 1;
    }
    let code = argv
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(crate::vars::last_status);
    crate::vars::set_returning(Some(code));
    code
}

/// `unset NAME...` — remove shell variables (scalar or array, the whole
/// thing) or, for `unset 'arr[i]'`/`unset 'arr[key]'` specifically, just
/// that one element (a real gap in a sparse indexed array, not merely
/// emptying it — see `vars::array_unset_index`/`vars::assoc_unset_key`).
fn unset(argv: &[String]) -> i32 {
    use crate::expand::UnsetTarget;
    let mut status = 0;
    // `-f` targets functions, `-v` variables (the default) (C97). bash
    // with neither flag also falls back to a function when no variable by
    // that name exists — matched below.
    let (mode, start) = match argv.get(1).map(String::as_str) {
        Some("-f") => ('f', 2),
        Some("-v") => ('v', 2),
        _ => (' ', 1),
    };
    if mode == 'f' {
        for name in &argv[start..] {
            crate::func::remove(name);
        }
        return 0;
    }
    for name in &argv[start..] {
        if mode == ' '
            && crate::vars::get(name).is_none()
            && crate::vars::array_len(name) == 0
            && crate::func::exists(name)
        {
            crate::func::remove(name);
            continue;
        }
        match crate::expand::parse_array_unset_index(name) {
            Ok(target) => {
                // Readonly names can't be unset — whole variable or one
                // element alike. Status 1 and continue (not fatal),
                // message matching real bash's own.
                let base = match &target {
                    Some(UnsetTarget::Index(array, _)) => array.as_str(),
                    Some(UnsetTarget::Key(array, _)) => array.as_str(),
                    None => name.as_str(),
                };
                if crate::vars::is_readonly(base) {
                    eprintln!("unset: {base}: cannot unset: readonly variable");
                    status = 1;
                    continue;
                }
                match target {
                    Some(UnsetTarget::Index(array, index)) => crate::vars::array_unset_index(&array, index),
                    Some(UnsetTarget::Key(array, key)) => crate::vars::assoc_unset_key(&array, &key),
                    None => crate::vars::unset(name),
                }
            }
            Err(e) => eprintln!("unset: {name}: {e}"),
        }
    }
    status
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
///
/// Dispatched straight from the expanded `Command` (see `exec::
/// dispatch_builtin`) rather than through `try_run`'s ordinary `argv:
/// &[String]` path: `local arr=(a b c)`'s array literal can't survive being
/// flattened into a plain string, so `expand::expand_simple` parses each
/// declaration itself into `decls` ahead of time (see `Command::
/// local_decls`'s own doc comment).
pub(crate) fn local_from_decls(
    decls: &[(String, Option<crate::vars::AssignOp>)],
    attrs: crate::vars::Attrs,
) -> i32 {
    if decls.is_empty() {
        eprintln!("local: usage: local name[=value] ...");
        return 1;
    }
    let mut status = 0;
    for (name, op) in decls {
        // Shadowing a readonly name with a local fails with status 1 and
        // continues — verified against real bash ("local: x: readonly
        // variable", C45).
        if crate::vars::is_readonly(name) {
            eprintln!("local: {name}: readonly variable");
            status = 1;
            continue;
        }
        // `local -n out=$1` (C62) — the standard "return through a
        // caller-named variable" mechanism. The nameref mapping is
        // frame-scoped: captured and restored by the local machinery.
        if attrs.nameref {
            let target = match op {
                Some(crate::vars::AssignOp::Set(crate::vars::AssignValue::Scalar(t))) => t.clone(),
                None => String::new(),
                _ => {
                    eprintln!("local: {name}: invalid nameref declaration");
                    status = 1;
                    continue;
                }
            };
            if !crate::vars::declare_local_nameref(name, &target) {
                eprintln!("local: can only be used in a function");
                status = 1;
            }
            continue;
        }
        let declared = match op {
            Some(crate::vars::AssignOp::Set(crate::vars::AssignValue::Array(elements))) => {
                crate::vars::declare_local_array_attrs(name, elements.clone(), attrs)
            }
            Some(crate::vars::AssignOp::Set(crate::vars::AssignValue::Assoc(pairs))) => {
                crate::vars::declare_local_assoc_attrs(name, pairs.clone(), attrs)
            }
            Some(crate::vars::AssignOp::Set(crate::vars::AssignValue::Scalar(value))) => {
                crate::vars::declare_local_attrs(name, Some(value), attrs)
            }
            // `+=`/indexed forms aren't meaningful for a name that isn't
            // local yet — treated the same as a bare `local name`.
            _ => crate::vars::declare_local_attrs(name, None, attrs),
        };
        if !declared {
            eprintln!("local: can only be used in a function");
            status = 1;
        }
    }
    status
}

/// `declare [-a|-A] [name[=value]]...` — a deliberately narrow subset: just
/// enough to unlock indexed (`-a`) and associative (`-A`) arrays, since
/// `arr[key]=v` needs `arr` to already be declared `-A` to treat `key` as a
/// literal string instead of an arithmetic index (see `vars::key_set`'s own
/// doc comment). Always applies to the current/global scope — unlike real
/// bash, where `declare` quietly acts like `local` inside a function; an
/// accepted, documented simplification (use `local -A`/`local -a` for a
/// function-scoped declaration). No `-p` (print), `-x` (export), `-r`
/// (readonly), `-i` (integer), or `-f` (functions) — none of those exist
/// here at all yet.
/// `declare -p [name...]` / `declare -F [name...]` / `declare -f
/// [name...]` — the introspection forms (C96). `-p` round-trips variables
/// in bash's own `declare -flags name="value"` format; `-F` lists
/// function names (`declare -f name` lines bare, just the name when
/// filtered — bash's formats). `-f` reports existence by status; printing
/// a function's *source* needs an AST unparser rush doesn't have yet, a
/// documented narrowing (existence checks — its most common use — work).
pub(crate) fn declare_print(argv: &[String]) -> i32 {
    let mode = argv[1].as_str();
    let names = &argv[2..];
    match mode {
        "-p" => {
            if names.is_empty() {
                let mut all = crate::vars::names();
                all.sort();
                for name in all {
                    println!("{}", crate::expand::declare_p_line(&name));
                }
                return 0;
            }
            let mut status = 0;
            for name in names {
                if crate::vars::get(name).is_some() || crate::vars::array_len(name) > 0 {
                    println!("{}", crate::expand::declare_p_line(name));
                } else {
                    eprintln!("declare: {name}: not found");
                    status = 1;
                }
            }
            status
        }
        _ => {
            // `-F` and `-f` share the status rules; only output differs.
            if names.is_empty() {
                for name in crate::func::names() {
                    if mode == "-F" {
                        println!("declare -f {name}");
                    } else if let Some(body) = crate::func::get(&name) {
                        print!("{}", crate::unparse::function_source(&name, &body));
                    }
                }
                return 0;
            }
            let mut status = 0;
            for name in names {
                match crate::func::get(name) {
                    Some(body) => {
                        if mode == "-F" {
                            println!("{name}");
                        } else {
                            print!("{}", crate::unparse::function_source(name, &body));
                        }
                    }
                    None => status = 1,
                }
            }
            status
        }
    }
}

pub(crate) fn declare_from_decls(
    decls: &[(String, Option<crate::vars::AssignOp>)],
    attrs: crate::vars::Attrs,
) -> i32 {
    let mut status = 0;
    for (name, op) in decls {
        // `declare -n ref[=target]` (C62): record the nameref mapping
        // instead of assigning a value — `=target` names the referent, a
        // bare declaration leaves the target for the next plain
        // assignment to choose (both verified against bash).
        if attrs.nameref {
            match op {
                Some(crate::vars::AssignOp::Set(crate::vars::AssignValue::Scalar(target))) => {
                    crate::vars::set_nameref(name, target);
                }
                None => crate::vars::set_nameref(name, ""),
                _ => {
                    eprintln!("rush: {name}: invalid nameref declaration");
                    status = 1;
                }
            }
            continue;
        }
        // Re-assigning an already-readonly name fails with status 1 and
        // continues (not fatal) — verified against real bash (C45).
        if op.is_some() && crate::vars::is_readonly(name) {
            eprintln!("rush: {name}: readonly variable");
            status = 1;
            continue;
        }
        // Attributes install *before* the initializer applies, so
        // `declare -u u=hello` uppercases (C43) — except `-r`, which
        // installs *after* it, so `declare -r x=5` can still set its own
        // value (C45). Separate invocations merge (`declare -i x` after
        // `declare -u x` keeps both), same as real bash — see
        // `vars::set_attrs`.
        if attrs.any() {
            crate::vars::set_attrs(name, crate::vars::Attrs { readonly: false, ..attrs });
        }
        if let Some(op) = op {
            crate::vars::assign(name, op);
        }
        if attrs.readonly {
            crate::vars::set_attrs(name, crate::vars::Attrs { readonly: true, ..Default::default() });
        }
        // A bare `declare name` (no flags, no initializer) is a
        // documented no-op here — bash pre-declares it unset, which is
        // unobservable without a `declare -p` this codebase doesn't have.
    }
    status
}

/// `readonly [name[=value]]...` — POSIX special builtin (C45): assigns
/// (when a value is given) and marks each name read-only; every later
/// mutation is rejected (see `vars::is_readonly` and its call sites).
/// `readonly` with no operands, or `readonly -p`, lists every read-only
/// variable in bash's own `declare -r name="value"` format. Routed through
/// the same decl path as `local`/`declare` so array literals
/// (`readonly arr=(a b)`) survive, and so `-a`/`-A` work.
pub(crate) fn readonly_from_decls(
    decls: &[(String, Option<crate::vars::AssignOp>)],
    attrs: crate::vars::Attrs,
) -> i32 {
    // `-p` isn't in the decl flag-cluster set (it's print, not an
    // attribute), so it arrives here as an ordinary operand.
    let names_only: Vec<_> = decls.iter().filter(|(n, _)| n != "-p").collect();
    if names_only.is_empty() {
        for line in crate::vars::readonly_listing() {
            println!("{line}");
        }
        return 0;
    }
    let mut status = 0;
    for (name, op) in names_only {
        // Re-assigning an already-readonly name fails with status 1 and
        // continues — verified against real bash (`readonly x=9` on a
        // readonly `x` errors, keeps the old value, keeps going).
        if op.is_some() && crate::vars::is_readonly(name) {
            eprintln!("rush: {name}: readonly variable");
            status = 1;
            continue;
        }
        if attrs.any() {
            crate::vars::set_attrs(name, crate::vars::Attrs { readonly: false, ..attrs });
        }
        if let Some(op) = op {
            crate::vars::assign(name, op);
        }
        crate::vars::set_attrs(name, crate::vars::Attrs { readonly: true, ..Default::default() });
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

/// Every way `name` resolves, highest-precedence first — `type -a` (C48):
/// alias, keyword, function, builtin, then *every* `$PATH` match, one per
/// directory in order (duplicate directories deliberately not deduped —
/// real bash lists `ls` twice for `PATH=/bin:/usr/bin:/bin`, verified
/// directly).
fn classify_all(name: &str) -> Vec<Kind> {
    let mut kinds = Vec::new();
    if let Some(value) = crate::alias::get(name) {
        kinds.push(Kind::Alias(value));
    }
    if crate::parser::RESERVED.contains(&name) {
        kinds.push(Kind::Keyword);
    }
    if crate::func::exists(name) {
        kinds.push(Kind::Function);
    }
    if is_builtin(name) {
        kinds.push(Kind::Builtin);
    }
    if name.contains('/') {
        kinds.extend(resolve_in_path(name).map(Kind::File));
    } else if let Some(path) = crate::vars::get("PATH") {
        kinds.extend(
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .filter(|c| is_executable_file(c))
                .map(Kind::File),
        );
    }
    kinds
}

/// Search `$PATH` for an executable file named `name`. A `name` containing
/// `/` is treated as an explicit path (checked directly, not searched for)
/// rather than a `$PATH` lookup.
pub(crate) fn resolve_in_path(name: &str) -> Option<std::path::PathBuf> {
    // `vars::get` alone — no `std::env` fallback (C36/C40): falling back
    // would resurrect `PATH`'s original, inherited value after `unset`.
    resolve_in_path_string(name, crate::vars::get("PATH"))
}

/// As [`resolve_in_path`], but against the fixed default system path —
/// `command -p`'s search (C47), which deliberately ignores the shell's own
/// (possibly compromised or customized) `$PATH`. The same "/bin:/usr/bin"
/// real bash's `command -p` uses here (its `confstr(_CS_PATH)` value on
/// Linux, verified via `getconf PATH`).
pub(crate) fn resolve_in_default_path(name: &str) -> Option<std::path::PathBuf> {
    resolve_in_path_string(name, Some("/bin:/usr/bin".to_string()))
}

fn resolve_in_path_string(name: &str, path: Option<String>) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        let p = Path::new(name);
        return is_executable_file(p).then(|| p.to_path_buf());
    }
    let path = path?;
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
    // Flags may cluster (`command -pv ls`, same as bash). `-p` (C47)
    // switches the lookup to the fixed default system path. The
    // *execution* forms (`command name …`, `command -p name …`) are
    // intercepted earlier by `exec::command_bypass` and never reach here.
    let mut default_path = false;
    let mut verbose: Option<bool> = None;
    let mut idx = 1;
    while idx < argv.len() {
        let Some(flags) = argv[idx].strip_prefix('-').filter(|f| !f.is_empty()) else {
            break;
        };
        if !flags.chars().all(|c| matches!(c, 'p' | 'v' | 'V')) {
            break;
        }
        for c in flags.chars() {
            match c {
                'p' => default_path = true,
                'v' => verbose = Some(false),
                'V' => verbose = Some(true),
                _ => unreachable!(),
            }
        }
        idx += 1;
    }
    match verbose {
        Some(v) => command_v(&argv[idx..], v, default_path),
        None if idx >= argv.len() => 0,
        None => {
            eprintln!("command: running a command isn't supported here (only as the sole stage of a pipeline)");
            127
        }
    }
}

fn command_v(names: &[String], verbose: bool, default_path: bool) -> i32 {
    if names.is_empty() {
        eprintln!("command: usage: command -{} name ...", if verbose { "V" } else { "v" });
        return 2;
    }
    let mut found_any = false;
    for name in names {
        // `-p`: only the default system path is consulted — a name that
        // resolves solely through the shell's own `$PATH` (or an alias/
        // function/builtin: bash still reports those) keeps bash's own
        // precedence, but the file lookup itself uses the fixed path.
        let kind = if default_path {
            match classify(name, false) {
                Some(Kind::File(_)) | None => resolve_in_default_path(name).map(Kind::File),
                other => other,
            }
        } else {
            classify(name, false)
        };
        if let Some(kind) = kind {
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
    // `-t` (one-word label), `-a` (every match, C48), and the path forms
    // `-p` (path only if it'd run the disk file) / `-P` (force a `$PATH`
    // search, C100) — alone or clustered, same as bash.
    let mut just_kind = false;
    let mut all = false;
    let mut path_only = false;
    let mut force_path = false;
    let mut idx = 1;
    while idx < argv.len() {
        let Some(flags) = argv[idx].strip_prefix('-').filter(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 't' | 'a' | 'p' | 'P'))) else {
            break;
        };
        for c in flags.chars() {
            match c {
                't' => just_kind = true,
                'a' => all = true,
                'p' => path_only = true,
                'P' => force_path = true,
                _ => unreachable!(),
            }
        }
        idx += 1;
    }
    let names = &argv[idx..];
    if names.is_empty() {
        eprintln!("type: usage: type [-aptP] name ...");
        return 2;
    }
    let mut status = 0;
    for name in names {
        if path_only || force_path {
            // `-p`: print the disk path only when plain `type` would run
            // it (i.e. no function/builtin/alias shadows it); `-P` prints
            // it regardless. Nothing found → status 1, silently.
            let shadowed = crate::func::exists(name) || is_builtin(name);
            match resolve_in_path(name) {
                Some(p) if force_path || !shadowed => println!("{}", p.display()),
                Some(_) => {}
                // A builtin/function with no disk file is still "found"
                // for `-p` (status 0, nothing printed) — only a name that
                // resolves to nothing at all fails.
                None if force_path || !shadowed => status = 1,
                None => {}
            }
            continue;
        }
        let kinds = if all {
            classify_all(name)
        } else {
            classify(name, true).into_iter().collect()
        };
        if kinds.is_empty() {
            eprintln!("type: {name}: not found");
            status = 1;
            continue;
        }
        for kind in kinds {
            println!("{}", if just_kind { kind.label().to_string() } else { kind.describe(name) });
        }
    }
    status
}

thread_local! {
    // The shell history's authoritative mirror (C103/C122–C124): the
    // interactive loop records accepted commands here (alongside the line
    // editor's own list) so the `history` builtin can list and mutate it;
    // `HISTORY_RESET` tells the loop to rebuild the editor from this
    // mirror after `history -c`/`-d`/`-r`.
    static SHELL_HISTORY: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static HISTORY_RESET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn history_record(line: &str) {
    SHELL_HISTORY.with(|h| h.borrow_mut().push(line.to_string()));
}

pub fn history_entries() -> Vec<String> {
    SHELL_HISTORY.with(|h| h.borrow().clone())
}

/// Take (and clear) the "editor must rebuild its history" flag.
pub fn history_reset_pending() -> bool {
    HISTORY_RESET.with(|f| f.replace(false))
}

/// Define functions exported by a parent shell (C98): every environment
/// entry named `BASH_FUNC_name%%` whose value looks like `() { ... }` —
/// the encoding both bash and rush's own `export -f` emit — becomes a
/// defined (and re-exported) function. Malformed values are skipped.
pub fn import_functions() {
    for name in crate::vars::names() {
        let Some(func_name) = name.strip_prefix("BASH_FUNC_").and_then(|n| n.strip_suffix("%%"))
        else {
            continue;
        };
        let Some(value) = crate::vars::get(&name) else { continue };
        let Some(body_src) = value.trim_start().strip_prefix("()") else { continue };
        if let Ok(list) = crate::parser::parse(&format!("{func_name} () {}", body_src.trim_start()))
            && let Some(crate::parser::RawCommand::Compound(rc)) =
                list.jobs.first().map(|j| &j.list.first.commands[0])
            && let crate::parser::Compound::FuncDef { name: parsed, body } = rc.compound.as_ref()
        {
            crate::func::define(parsed, body.clone());
        }
    }
}

/// `history` (C103): list numbered entries; `-c` clear, `-d N` delete,
/// `-s line` record without executing, `-p args...` expand-and-print,
/// `-w`/`-r` write/read `$HISTFILE`, `-a`/`-n` accepted (the interactive
/// loop already appends incrementally — C123).
fn history_cmd(argv: &[String]) -> i32 {
    let histfile = || {
        crate::vars::get("HISTFILE").filter(|f| !f.is_empty()).or_else(|| {
            crate::vars::get("HOME").map(|h| format!("{h}/.rush_history"))
        })
    };
    match argv.get(1).map(String::as_str) {
        Some("-c") => {
            SHELL_HISTORY.with(|h| h.borrow_mut().clear());
            HISTORY_RESET.with(|f| f.set(true));
            0
        }
        Some("-d") => {
            let Some(n) = argv.get(2).and_then(|v| v.parse::<usize>().ok()).filter(|&n| n >= 1)
            else {
                eprintln!("history: -d: history position required");
                return 2;
            };
            let ok = SHELL_HISTORY.with(|h| {
                let mut h = h.borrow_mut();
                (n <= h.len()).then(|| h.remove(n - 1)).is_some()
            });
            if !ok {
                eprintln!("history: {n}: history position out of range");
                return 1;
            }
            HISTORY_RESET.with(|f| f.set(true));
            0
        }
        Some("-s") => {
            history_record(&argv[2..].join(" "));
            HISTORY_RESET.with(|f| f.set(true));
            0
        }
        Some("-p") => {
            let entries = history_entries();
            let mut status = 0;
            for arg in &argv[2..] {
                match crate::history_expand::expand(arg, &entries) {
                    Ok(Some(e)) => println!("{e}"),
                    Ok(None) => println!("{arg}"),
                    Err(e) => {
                        eprintln!("history: {e}");
                        status = 1;
                    }
                }
            }
            status
        }
        Some("-w") => {
            let Some(path) = histfile() else { return 1 };
            let body = history_entries().join("\n");
            match std::fs::write(&path, body + "\n") {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("history: {path}: {e}");
                    1
                }
            }
        }
        Some("-r") => {
            let Some(path) = histfile() else { return 1 };
            match std::fs::read_to_string(&path) {
                Ok(body) => {
                    SHELL_HISTORY.with(|h| {
                        h.borrow_mut().extend(body.lines().map(str::to_string));
                    });
                    HISTORY_RESET.with(|f| f.set(true));
                    0
                }
                Err(e) => {
                    eprintln!("history: {path}: {e}");
                    1
                }
            }
        }
        Some("-a") | Some("-n") => 0, // the loop appends incrementally (C123)
        Some(other) if other.starts_with('-') && other.parse::<i32>().is_err() => {
            eprintln!("history: {other}: invalid option");
            2
        }
        tail => {
            // `history [n]` — the whole list, or just the last n.
            let entries = history_entries();
            let skip = tail
                .and_then(|n| n.parse::<usize>().ok())
                .map_or(0, |n| entries.len().saturating_sub(n));
            for (i, e) in entries.iter().enumerate().skip(skip) {
                println!("{:5}  {e}", i + 1);
            }
            0
        }
    }
}

thread_local! {
    // `hash`'s remembered name→path table (C100). Spawns consult it via
    // `hashed_path` before searching `$PATH`, so `hash -p /path name`
    // really redirects future lookups of `name`.
    static HASH_TABLE: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The remembered path for `name`, if `hash` has one — consulted by the
/// spawn path ahead of the `$PATH` search.
pub fn hashed_path(name: &str) -> Option<String> {
    HASH_TABLE.with(|t| t.borrow().get(name).cloned())
}

/// `hash [-r] [-d name...] [-t name...] [-p path name] [name...]` (C100):
/// a real remembered-paths table. Plain `hash name` resolves on `$PATH`
/// and remembers; `-t` reports a remembered path, `-d` forgets one, `-p`
/// plants an explicit path, `-r` empties the table.
fn hash_cmd(argv: &[String]) -> i32 {
    match argv.get(1).map(String::as_str) {
        Some("-r") => {
            HASH_TABLE.with(|t| t.borrow_mut().clear());
            return 0;
        }
        Some("-d") => {
            let mut status = 0;
            for name in &argv[2..] {
                if HASH_TABLE.with(|t| t.borrow_mut().remove(name)).is_none() {
                    eprintln!("hash: {name}: not found");
                    status = 1;
                }
            }
            return status;
        }
        Some("-t") => {
            let mut status = 0;
            for name in &argv[2..] {
                match hashed_path(name) {
                    Some(p) => println!("{p}"),
                    None => {
                        eprintln!("hash: {name}: not found");
                        status = 1;
                    }
                }
            }
            return status;
        }
        Some("-p") => {
            let (Some(path), Some(name)) = (argv.get(2), argv.get(3)) else {
                eprintln!("hash: -p: option requires an argument");
                return 2;
            };
            HASH_TABLE.with(|t| t.borrow_mut().insert(name.clone(), path.clone()));
            return 0;
        }
        _ => {}
    }
    let names = &argv[1..];
    if names.is_empty() {
        let entries: Vec<(String, String)> =
            HASH_TABLE.with(|t| t.borrow().iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        if entries.is_empty() {
            println!("hash: hash table empty");
        } else {
            println!("hits\tcommand");
            for (_, path) in entries {
                println!("   0\t{path}");
            }
        }
        return 0;
    }
    let mut status = 0;
    for name in names {
        match resolve_in_path(name) {
            Some(path) => {
                HASH_TABLE.with(|t| t.borrow_mut().insert(name.clone(), path.display().to_string()));
            }
            None => {
                eprintln!("hash: {name}: not found");
                status = 1;
            }
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
        Ok(status) => {
            // `trap 'cmd' RETURN` (C65) fires as a sourced script
            // finishes, same as a function return.
            crate::vars::set_last_status(status);
            crate::trap::fire_preserving("RETURN");
            status
        }
        Err(e) => {
            eprintln!("{}: {name}: {e}", argv[0]);
            1
        }
    }
}

/// `umask [-S] [mode]` — with no `mode`, report the process's current file
/// creation mask (plain octal, or `u=rwx,g=rx,o=rx`-style with `-S`); with
/// one, set it. A real `crate::sys::umask()` call (Unix only), so it actually
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
            let m = crate::sys::umask(0);
            crate::sys::umask(m);
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
                crate::sys::umask(mode as crate::sys::mode_t);
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
fn symbolic_umask(mask: crate::sys::mode_t) -> String {
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

/// One `ulimit` resource: flag letter, `RLIMIT_*` id, the label bash's own
/// `-a` output uses, and the unit scale (bash reports and accepts values
/// in these units — 512-byte blocks for `-c`/`-f`, kbytes for the memory
/// ones — converting to raw bytes for `setrlimit`).
#[cfg(unix)]
struct UlimitResource {
    letter: char,
    resource: i32,
    label: &'static str,
    scale: u64,
}

#[cfg(unix)]
const ULIMIT_RESOURCES: &[UlimitResource] = &[
    UlimitResource { letter: 'c', resource: crate::sys::RLIMIT_CORE as i32, label: "core file size              (blocks, -c)", scale: 512 },
    UlimitResource { letter: 'd', resource: crate::sys::RLIMIT_DATA as i32, label: "data seg size               (kbytes, -d)", scale: 1024 },
    UlimitResource { letter: 'f', resource: crate::sys::RLIMIT_FSIZE as i32, label: "file size                   (blocks, -f)", scale: 512 },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UlimitResource { letter: 'e', resource: crate::sys::RLIMIT_NICE as i32, label: "scheduling priority                 (-e)", scale: 1 },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UlimitResource { letter: 'i', resource: crate::sys::RLIMIT_SIGPENDING as i32, label: "pending signals                     (-i)", scale: 1 },
    UlimitResource { letter: 'l', resource: crate::sys::RLIMIT_MEMLOCK as i32, label: "max locked memory           (kbytes, -l)", scale: 1024 },
    UlimitResource { letter: 'm', resource: crate::sys::RLIMIT_RSS as i32, label: "max memory size             (kbytes, -m)", scale: 1024 },
    UlimitResource { letter: 'n', resource: crate::sys::RLIMIT_NOFILE as i32, label: "open files                          (-n)", scale: 1 },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UlimitResource { letter: 'q', resource: crate::sys::RLIMIT_MSGQUEUE as i32, label: "POSIX message queues         (bytes, -q)", scale: 1 },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UlimitResource { letter: 'r', resource: crate::sys::RLIMIT_RTPRIO as i32, label: "real-time priority                  (-r)", scale: 1 },
    UlimitResource { letter: 's', resource: crate::sys::RLIMIT_STACK as i32, label: "stack size                  (kbytes, -s)", scale: 1024 },
    UlimitResource { letter: 't', resource: crate::sys::RLIMIT_CPU as i32, label: "cpu time                   (seconds, -t)", scale: 1 },
    UlimitResource { letter: 'u', resource: crate::sys::RLIMIT_NPROC as i32, label: "max user processes                  (-u)", scale: 1 },
    UlimitResource { letter: 'v', resource: crate::sys::RLIMIT_AS as i32, label: "virtual memory              (kbytes, -v)", scale: 1024 },
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UlimitResource { letter: 'x', resource: crate::sys::RLIMIT_LOCKS as i32, label: "file locks                          (-x)", scale: 1 },
];

/// `ulimit [-SH] [-a | -<letter> [limit]]` (C46) — get/set process resource
/// limits over real `getrlimit`/`setrlimit` calls, in bash's units (blocks
/// for `-c`/`-f`, kbytes for memory sizes) with the `unlimited` keyword.
/// With no resource flag, `-f` (file size) is the subject, same as bash.
/// Reading reports the soft limit unless `-H`; setting applies to both
/// limits unless `-S`/`-H` narrows it. `-a` dumps every resource in bash's
/// own labeled format. Not implemented (accepted narrowing, documented in
/// docs/CAPABILITY_GAPS.md): bash's read-only `-p` (pipe size) and `-b`/
/// `-k`/`-P`/`-T`, and the `hard`/`soft` keywords as a limit operand.
#[cfg(unix)]
fn ulimit_cmd(argv: &[String]) -> i32 {
    let mut soft_only = false;
    let mut hard_only = false;
    let mut all = false;
    let mut letter: Option<char> = None;
    let mut operand: Option<&str> = None;

    for arg in &argv[1..] {
        if let Some(flags) = arg.strip_prefix('-').filter(|f| !f.is_empty() && f.chars().all(char::is_alphabetic)) {
            for c in flags.chars() {
                match c {
                    'S' => soft_only = true,
                    'H' => hard_only = true,
                    'a' => all = true,
                    c if ULIMIT_RESOURCES.iter().any(|r| r.letter == c) => letter = Some(c),
                    _ => {
                        eprintln!("ulimit: -{c}: invalid option");
                        eprintln!("ulimit: usage: ulimit [-SHa{}] [limit]", ULIMIT_RESOURCES.iter().map(|r| r.letter).collect::<String>());
                        return 2;
                    }
                }
            }
        } else if operand.is_none() {
            operand = Some(arg);
        } else {
            eprintln!("ulimit: too many arguments");
            return 1;
        }
    }

    let display = |raw: crate::sys::rlim_t, scale: u64| -> String {
        if raw == crate::sys::RLIM_INFINITY { "unlimited".to_string() } else { (raw / scale).to_string() }
    };
    let get = |res: &UlimitResource| -> Option<crate::sys::rlimit> {
        let mut rl = crate::sys::rlimit { rlim_cur: 0, rlim_max: 0 };
        (unsafe { crate::sys::getrlimit(res.resource as _, &mut rl) } == 0).then_some(rl)
    };

    if all {
        for res in ULIMIT_RESOURCES {
            if let Some(rl) = get(res) {
                let raw = if hard_only { rl.rlim_max } else { rl.rlim_cur };
                println!("{} {}", res.label, display(raw, res.scale));
            }
        }
        return 0;
    }

    let res = match letter {
        Some(c) => ULIMIT_RESOURCES.iter().find(|r| r.letter == c).unwrap(),
        None => ULIMIT_RESOURCES.iter().find(|r| r.letter == 'f').unwrap(),
    };
    let Some(mut rl) = get(res) else {
        eprintln!("ulimit: cannot read limit: {}", crate::sys::last_os_error());
        return 1;
    };

    let Some(operand) = operand else {
        let raw = if hard_only { rl.rlim_max } else { rl.rlim_cur };
        println!("{}", display(raw, res.scale));
        return 0;
    };

    let new_raw: crate::sys::rlim_t = if operand == "unlimited" {
        crate::sys::RLIM_INFINITY
    } else {
        match operand.parse::<u64>() {
            Ok(n) => n.saturating_mul(res.scale) as crate::sys::rlim_t,
            Err(_) => {
                eprintln!("ulimit: {operand}: invalid number");
                return 1;
            }
        }
    };
    // Without -S/-H a new limit applies to both; -S leaves the hard limit
    // alone, -H the soft — same as bash.
    if !hard_only {
        rl.rlim_cur = new_raw;
    }
    if !soft_only {
        rl.rlim_max = new_raw;
    }
    if unsafe { crate::sys::setrlimit(res.resource as _, &rl) } != 0 {
        eprintln!("ulimit: cannot modify limit: {}", crate::sys::last_os_error());
        return 1;
    }
    0
}

#[cfg(not(unix))]
fn ulimit_cmd(argv: &[String]) -> i32 {
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
/// `exec::pipeline_status`). `set -x` / `set +x` — xtrace: echo each
/// command to stderr before running it (see `exec::trace_command`). Other
/// flags/`-o` names aren't implemented yet; naming one is an error rather
/// than a silently-ignored no-op.
/// `shopt [-s|-u|-q|-p] [name...]` (C58) — query/toggle bash's shell
/// options; rush recognizes the glob family (`nullglob`, `failglob`,
/// `dotglob`, `globstar`, plus `extglob`, which defaults *on* — see
/// C57). No names: list all (a `name<tab>on|off` table, or re-runnable
/// `shopt -s/-u name` lines with `-p`). Names without `-s`/`-u`: query —
/// print each (unless `-q`) and return 0 only if *all* are set. Unknown
/// names are "invalid shell option name", status 1 — all formats and
/// statuses matching real bash, verified directly.
fn shopt_cmd(argv: &[String]) -> i32 {
    let mut set_on = false;
    let mut set_off = false;
    let mut quiet = false;
    let mut runnable = false;
    let mut idx = 1;
    while idx < argv.len() {
        let Some(flags) = argv[idx]
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 's' | 'u' | 'q' | 'p')))
        else {
            break;
        };
        for c in flags.chars() {
            match c {
                's' => set_on = true,
                'u' => set_off = true,
                'q' => quiet = true,
                'p' => runnable = true,
                _ => unreachable!(),
            }
        }
        idx += 1;
    }
    let names = &argv[idx..];

    let print_one = |name: &str, on: bool| {
        if runnable {
            println!("shopt {} {name}", if on { "-s" } else { "-u" });
        } else {
            println!("{name:<15}\t{}", if on { "on" } else { "off" });
        }
    };

    if names.is_empty() {
        for &(name, _) in crate::vars::SHOPT_DEFAULTS {
            let on = crate::vars::shopt(name);
            if (set_on && !on) || (set_off && on) {
                continue; // `shopt -s` alone lists only the set ones (and -u the unset), like bash
            }
            print_one(name, on);
        }
        return 0;
    }

    let mut status = 0;
    for name in names {
        if set_on || set_off {
            if !crate::vars::set_shopt(name, set_on) {
                eprintln!("shopt: {name}: invalid shell option name");
                status = 1;
            }
        } else if crate::vars::SHOPT_DEFAULTS.iter().any(|(n, _)| n == name) {
            let on = crate::vars::shopt(name);
            if !quiet {
                print_one(name, on);
            }
            if !on {
                status = 1;
            }
        } else {
            eprintln!("shopt: {name}: invalid shell option name");
            status = 1;
        }
    }
    status
}

/// `set -o` (bare) / `set +o` (bare) — list every tracked option (C52):
/// the `-o` spelling in bash's own `name<padding>on|off` table format,
/// the `+o` spelling as directly re-runnable `set -o name`/`set +o name`
/// lines (both formats verified against real bash).
fn list_options(dash_spelling: bool) {
    let options: &[(&str, bool)] = &[
        ("allexport", crate::vars::allexport()),
        ("errexit", crate::vars::errexit()),
        ("noglob", crate::vars::noglob()),
        ("noclobber", crate::vars::noclobber()),
        ("noexec", crate::vars::noexec()),
        ("nounset", crate::vars::nounset()),
        ("pipefail", crate::vars::pipefail()),
        ("vi", crate::vars::edit_mode_vi()),
        ("xtrace", crate::vars::xtrace()),
    ];
    for (name, on) in options {
        if dash_spelling {
            println!("{:<15}\t{}", name, if *on { "on" } else { "off" });
        } else {
            println!("set {}o {name}", if *on { "-" } else { "+" });
        }
    }
}

fn set_cmd(argv: &[String]) -> i32 {
    // Flag changes are collected first and only applied once the *whole*
    // invocation has validated — an invalid flag anywhere means nothing is
    // applied at all, matching real bash exactly (verified directly:
    // `set -eu -z` leaves errexit/nounset both off, and the shell survives
    // even though `-e` textually preceded the bad `-z`; partial application
    // would have errexit-killed the shell on `set`'s own failure).
    let mut pending: Vec<(char, bool)> = Vec::new();
    let apply = |pending: &[(char, bool)]| {
        for &(flag, on) in pending {
            match flag {
                'e' => crate::vars::set_errexit(on),
                'a' => crate::vars::set_allexport(on),
                'f' => crate::vars::set_noglob(on),
                'u' => crate::vars::set_nounset(on),
                'x' => crate::vars::set_xtrace(on),
                'C' => crate::vars::set_noclobber(on),
                'v' => crate::vars::set_edit_mode_vi(on),
                // `set -n` is ignored by an interactive shell (matching
                // bash — it would lock the session out otherwise).
                'n' => {
                    if !crate::vars::interactive() {
                        crate::vars::set_noexec(on);
                    }
                }
                'p' => crate::vars::set_pipefail(on),
                _ => unreachable!(),
            }
        }
    };
    let mut args = argv[1..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // `--`: everything after becomes the new positional parameters,
            // even if it looks like a flag (`set -- -x` makes `$1` the
            // literal text `-x`, not the xtrace flag) — verified directly.
            "--" => {
                apply(&pending);
                crate::vars::set_positional(args.cloned().collect());
                return 0;
            }
            // A bare word, not `-`/`+`-prefixed (so a genuinely unknown flag
            // like `-z` still falls through to the flag arm below and
            // errors, matching real bash): this and everything after it
            // becomes the new positional parameters — `set a b c`, no `--`
            // needed, works exactly like `set -- a b c` does. `$0` is never
            // touched by either form. Flags before the first bare word still
            // apply (`set -e a b` turns errexit on *and* sets `$1`).
            other if !other.starts_with(['-', '+']) => {
                apply(&pending);
                let mut new_args = vec![other.to_string()];
                new_args.extend(args.cloned());
                crate::vars::set_positional(new_args);
                return 0;
            }
            // A `-`/`+`-prefixed flag word, possibly clustered (`set -eu`,
            // `set -euo pipefail` — the near-universal script header) —
            // each letter queues in turn; `o` consumes the *next word* as
            // its option name even mid-cluster, same as real bash.
            //
            // An unrecognized letter (`set -z`, `set -ez`) is a hard error —
            // it must *not* fall through and reassign positional parameters
            // from whatever follows it, matching real bash exactly
            // (`set -z a b` leaves `$1`/`$2` untouched). Same rule for an
            // invalid or missing `-o` name — verified directly: `set -o
            // badname a b` leaves `$1`/`$2` untouched too.
            other => {
                let on = other.starts_with('-');
                for c in other[1..].chars() {
                    match c {
                        'e' | 'u' | 'x' | 'C' | 'n' | 'a' | 'f' => pending.push((c, on)),
                        // Accepted but inert (C107): `set -euEbo pipefail`
                        // preambles must not hard-error. `b` notify, `h`
                        // hashall, `k` keyword, `m` monitor, `B`
                        // braceexpand, `H` histexpand, `P` physical, `E`
                        // errtrace, `T` functrace, `v` verbose.
                        'b' | 'h' | 'k' | 'm' | 'B' | 'H' | 'P' | 'E' | 'T' | 'v' => {}
                        // `-o name` — the long spellings (C52), mapped to
                        // the same letters the short forms queue. A bare
                        // `set -o`/`set +o` (no name following) lists every
                        // tracked option instead: current on/off state for
                        // `-o`, a directly re-runnable `set -o name`/`set +o
                        // name` line for `+o` — both matching real bash's
                        // own formats exactly.
                        'o' => match args.next().map(String::as_str) {
                            Some("errexit") => pending.push(('e', on)),
                            Some("nounset") => pending.push(('u', on)),
                            Some("xtrace") => pending.push(('x', on)),
                            Some("noclobber") => pending.push(('C', on)),
                            Some("noexec") => pending.push(('n', on)),
                            Some("pipefail") => pending.push(('p', on)),
                            // `set -o vi` / `set -o emacs` (C73): flip the
                            // line-editing mode; the interactive loop
                            // rebuilds its editor before the next prompt.
                            // (`set +o vi` = emacs, and vice versa.)
                            Some("vi") => pending.push(('v', on)),
                            Some("emacs") => pending.push(('v', !on)),
                            Some("allexport") => pending.push(('a', on)),
                            Some("noglob") => pending.push(('f', on)),
                            // Accepted but inert (C107) — see the letter
                            // cluster above.
                            Some(
                                "braceexpand" | "errtrace" | "functrace" | "hashall"
                                | "histexpand" | "history" | "ignoreeof" | "keyword"
                                | "monitor" | "notify" | "onecmd" | "physical" | "posix"
                                | "privileged" | "verbose",
                            ) => {}
                            Some(name) => {
                                eprintln!("set: {name}: invalid option name");
                                return 1;
                            }
                            None => {
                                apply(&pending);
                                list_options(on);
                                return 0;
                            }
                        },
                        _ => {
                            eprintln!("set: {other}: not supported");
                            return 1;
                        }
                    }
                }
            }
        }
    }
    apply(&pending);
    0
}

/// `trap` (list) / `trap 'command' NAME...` (register) / `trap - NAME...`
/// (reset to default). Only `EXIT` and `INT` are ever fired (see `trap.rs`).
/// `abbr [name=value ...]` (C70) — define fish/zsh-style abbreviations
/// that live-expand in place on the interactive line (on space, in
/// command position — see `completion::abbr_expansion`). With no
/// arguments, list every abbreviation re-runnably.
fn abbr_cmd(argv: &[String]) -> i32 {
    if argv.len() == 1 {
        for (name, value) in crate::alias::abbr_all() {
            println!("abbr {name}='{value}'");
        }
        return 0;
    }
    let mut status = 0;
    for arg in &argv[1..] {
        match arg.split_once('=') {
            Some((name, value)) if !name.is_empty() => crate::alias::abbr_set(name, value),
            _ => match crate::alias::abbr_get(arg) {
                Some(value) => println!("abbr {arg}='{value}'"),
                None => {
                    eprintln!("abbr: {arg}: not found");
                    status = 1;
                }
            },
        }
    }
    status
}

/// `unabbr name...` — remove abbreviations.
fn unabbr_cmd(argv: &[String]) -> i32 {
    let mut status = 0;
    for name in &argv[1..] {
        if !crate::alias::abbr_unset(name) {
            eprintln!("unabbr: {name}: not found");
            status = 1;
        }
    }
    status
}

fn trap_cmd(argv: &[String]) -> i32 {
    // `trap -l` (C65): the numbered signal-name table, in bash's own
    // five-per-line tab-separated format.
    if argv.get(1).map(String::as_str) == Some("-l") {
        let entries: Vec<String> = crate::trap::signal_list()
            .iter()
            .map(|(name, num)| format!("{num:>2}) SIG{name}"))
            .collect();
        for chunk in entries.chunks(5) {
            println!("{}", chunk.join("\t"));
        }
        return 0;
    }
    // `trap -p [name...]` (C65): print registered traps re-runnably —
    // the same format bare `trap` uses, optionally filtered.
    if argv.get(1).map(String::as_str) == Some("-p") {
        let filter: Vec<&str> =
            argv[2..].iter().filter_map(|s| crate::trap::normalize_signal_spec(s)).collect();
        for (name, command) in crate::trap::all() {
            if !argv[2..].is_empty() && !filter.contains(&name.as_str()) {
                continue;
            }
            let display = if name == "EXIT" { name } else { format!("SIG{name}") };
            println!("trap -- '{command}' {display}");
        }
        return 0;
    }
    if argv.len() == 1 {
        for (name, command) in crate::trap::all() {
            // Listing prints real signals `SIG`-prefixed, `EXIT` bare —
            // matching real bash's own `trap` output format exactly.
            let display = if name == "EXIT" { name } else { format!("SIG{name}") };
            println!("trap -- '{command}' {display}");
        }
        return 0;
    }
    // A leading `--` ends option parsing (C101) — load-bearing because
    // `trap -p`'s own re-runnable output uses it (`trap -- 'cmd' EXIT`).
    let argv: Vec<&String> =
        argv.iter().enumerate().filter(|&(i, a)| !(i == 1 && a == "--")).map(|(_, a)| a).collect();
    if argv.len() < 2 {
        return 0; // bare `trap --`
    }
    // Signal specs normalize to the canonical bare name delivery keys on
    // (C44): `15`, `SIGTERM`, `sigterm`, and `TERM` are all the same trap.
    // An invalid spec errors (status 1) without blocking the *other* specs
    // in the same call from registering — matching real bash, verified
    // directly (`trap 'cmd' BOGUS TERM` errors *and* registers TERM).
    let command = argv[1];
    let mut status = 0;
    for spec in &argv[2..] {
        match crate::trap::normalize_signal_spec(spec) {
            Some(name) if command == "-" => crate::trap::unset(name),
            Some(name) => crate::trap::set(name, command),
            None => {
                eprintln!("trap: {spec}: invalid signal specification");
                status = 1;
            }
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(args: &[&str]) -> Result<bool, String> {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        test_eval(&v)
    }

    // C72: interactive-only spelling correction and the distance helper.
    #[test]
    fn cd_spelling_correction() {
        assert_eq!(edit_distance("correctme", "CorrectMe".to_lowercase().as_str()), 0);
        assert_eq!(edit_distance("shre", "share"), 1);
        assert_eq!(edit_distance("abc", "xyz"), 3);

        let dir = std::env::temp_dir().join(format!("rush_c72_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("CorrectMe")).unwrap();
        let typo = dir.join("correctme");

        // Non-interactive: no correction.
        crate::vars::set_interactive(false);
        assert_eq!(cd_fallback_target(typo.to_str().unwrap()), None);

        // Interactive: the unique close-spelled sibling is offered.
        crate::vars::set_interactive(true);
        let corrected = cd_fallback_target(typo.to_str().unwrap());
        assert_eq!(corrected.as_deref(), Some(dir.join("CorrectMe").to_str().unwrap()));
        crate::vars::set_interactive(false);

        let _ = std::fs::remove_dir_all(&dir);
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
