//! Expansion: the stage between parse and exec.
//!
//! Turns a [`RawPipeline`] (words still carrying their quoting) into an
//! [`exec::Pipeline`] of concrete strings, applying:
//!
//!   * tilde expansion   — a leading unquoted `~` becomes `$HOME`
//!   * variables         — `$VAR`, `${VAR}` (unset → empty)
//!   * command substitution — `$(...)` runs a sub-pipeline and inlines its stdout
//!   * globbing          — `*`, `?`, `[…]` match against the filesystem
//!
//! Single-quoted and backslash-escaped text is taken verbatim, and only
//! metacharacters originating from *unquoted* text are active for globbing
//! (`"*.rs"` is literal). Globbing can turn one word into several arguments;
//! a pattern that matches nothing is left as its literal text. An unquoted
//! expansion is also split into fields on `$IFS` (default: space/tab/newline;
//! see `Ifs`) — a bare expansion that comes out empty drops out the way
//! `echo $UNSET` does in a real shell.

use std::iter::Peekable;
use std::str::Chars;

use crate::exec::{Command, Pipeline, Redirect};
use crate::lexer::{Word, WordPart};
use crate::parser::{self, RawCommand, RawPipeline, RawRedirect, RawSimple};

pub fn expand(raw: &RawPipeline) -> Result<Pipeline, String> {
    let mut commands = Vec::with_capacity(raw.commands.len());
    for rc in &raw.commands {
        let stage = match rc {
            RawCommand::Simple(s) => crate::exec::Stage::Simple(expand_simple(s)?),
            // A compound stage isn't expanded here (its own body is expanded
            // lazily, same as a sole compound) — just carried through. Only
            // Unix's job-control runner can actually run one as one stage
            // among several (it forks); elsewhere that's still an error, but
            // one raised at run time, not here at expansion time.
            RawCommand::Compound(c) => crate::exec::Stage::Compound(c.clone()),
        };
        commands.push(stage);
    }
    Ok(Pipeline { commands })
}

/// Expand a list of words into arguments (splitting + globbing) — used by the
/// `for` loop to compute its iteration values.
pub fn expand_words(words: &[Word]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for w in words {
        out.extend(expand_argv_word(w)?);
    }
    Ok(out)
}

/// Expand a word to a single string (no splitting or globbing) — used for a
/// `case` subject.
pub fn expand_to_string(word: &Word) -> Result<String, String> {
    expand_word(word)
}

/// Expand a `case` pattern: like a glob pattern, metacharacters from quoted or
/// literal parts are escaped so only unquoted `*?[` stay active. No tilde or
/// word-splitting (a pattern is a single match template).
pub fn expand_pattern(word: &Word) -> Result<String, String> {
    let mut pattern = String::new();
    for part in word {
        match part {
            WordPart::Literal(s) => escape_meta_into(&mut pattern, s),
            WordPart::Quoted(s) => escape_meta_into(&mut pattern, &expand_dollars(s)?),
            WordPart::Unquoted(s) => pattern.push_str(&expand_dollars(s)?),
        }
    }
    Ok(pattern)
}

fn expand_simple(rc: &RawSimple) -> Result<Command, String> {
    // Leading `NAME=value` words are assignments; they stop at the first word
    // that isn't one (the program name).
    let mut assignments = Vec::new();
    let mut idx = 0;
    while idx < rc.argv.len() {
        match assignment_split(&rc.argv[idx]) {
            Some((name, value_word)) => {
                assignments.push((name, expand_word(&value_word)?));
                idx += 1;
            }
            None => break,
        }
    }

    let mut argv = Vec::new();
    for word in &rc.argv[idx..] {
        argv.extend(expand_argv_word(word)?);
    }

    // A single, non-recursive alias substitution: `ll -a` with `alias
    // ll='ls -l'` becomes `ls -l -a`. The expanded words aren't re-checked
    // against the alias table, so `alias ls='ls --color=auto'` can't loop.
    if let Some(value) = argv.first().and_then(|first| crate::alias::get(first)) {
        let mut expanded: Vec<String> = value.split_whitespace().map(String::from).collect();
        expanded.extend(argv.into_iter().skip(1));
        argv = expanded;
    }

    let mut redirects = Vec::with_capacity(rc.redirects.len());
    let mut heredoc = None;
    for r in &rc.redirects {
        match r {
            RawRedirect::File { fd, file, mode } => redirects.push(Redirect::File {
                fd: *fd,
                file: expand_word(file)?,
                mode: *mode,
            }),
            RawRedirect::Both { file, append } => redirects.push(Redirect::Both {
                file: expand_word(file)?,
                append: *append,
            }),
            RawRedirect::Dup { fd, target } => {
                redirects.push(Redirect::Dup { fd: *fd, target: *target })
            }
            // A here-doc body feeds stdin; it's a Command field, not a Redirect.
            // Its `$`-expansions run unless the delimiter was quoted.
            RawRedirect::Heredoc { body, expand } => {
                heredoc = Some(if *expand { expand_dollars(body)? } else { body.clone() });
            }
        }
    }

    Ok(Command { argv, redirects, assignments, heredoc })
}

/// If `word` has the form `NAME=...` with `NAME` a valid identifier whose `=`
/// comes from unquoted text, split it into the name and a `Word` for the value
/// (the rest of the first part plus any following parts). Otherwise `None`.
fn assignment_split(word: &Word) -> Option<(String, Word)> {
    let WordPart::Unquoted(text) = word.first()? else {
        return None;
    };
    let eq = text.find('=')?;
    let name = &text[..eq];

    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
    if !valid {
        return None;
    }

    let mut value: Word = vec![WordPart::Unquoted(text[eq + 1..].to_string())];
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), value))
}

/// Expand a word destined for `argv`, possibly into several arguments.
///
/// Whitespace inside an *unquoted* expansion splits the word into fields
/// (`x="a b"; echo $x` → two args) — and since the lexer already split on
/// literal whitespace, any whitespace left in an unquoted part can only have
/// come from a `$`/`$(…)` expansion. Quoted and literal text never split.
///
/// Each field is also a glob pattern: metacharacters from quoted/literal text
/// are escaped, so only unquoted `*?[` are active. A field that matches files is
/// replaced by the sorted matches; otherwise its literal text is used. A field
/// that is entirely unquoted and empty (e.g. `$UNSET`) drops out; a quoted empty
/// (`""`) is kept.
fn expand_argv_word(word: &Word) -> Result<Vec<String>, String> {
    // A standalone `"$@"` expands to one argument per positional parameter,
    // preserving any spaces within each — the common arg-forwarding idiom.
    if let [WordPart::Quoted(s)] = word.as_slice() {
        if s == "$@" {
            return Ok(crate::vars::args());
        }
    }

    let ifs = Ifs::current();
    let mut sp = Splitter::default();

    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => sp.add_unsplit(s),
            WordPart::Quoted(s) => sp.add_unsplit(&expand_dollars(s)?),
            WordPart::Unquoted(s) => {
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                sp.add_split(&expand_dollars(&text)?, &ifs);
            }
        }
    }

    // A single trailing non-whitespace IFS delimiter at the very end of the
    // text doesn't produce a trailing empty field — real bash keeps a
    // *leading* one (`IFS=,`'s `,a` is `""`, `a`) but drops a *trailing* one
    // (`a,` is just `a`, not `a`, `""`) even though internal and repeated
    // trailing delimiters still do (`a,,` is `a`, `""`). The last field is
    // exactly this "opened by a hard boundary, never touched again" case iff
    // it's still unquoted and empty by the time every part's been processed.
    let mut fields = sp.fields;
    if matches!(fields.last(), Some(f) if f.explicit && f.plain.is_empty() && !f.quoted) {
        fields.pop();
    }

    let mut out = Vec::new();
    for field in fields {
        if field.globbable {
            let matches = crate::glob::glob(&field.pattern);
            if !matches.is_empty() {
                out.extend(matches);
                continue;
            }
        }
        if field.plain.is_empty() && !field.quoted && !field.explicit {
            continue; // unquoted-empty field drops out, unless $IFS itself demarcated it
        }
        out.push(field.plain);
    }
    Ok(out)
}

/// `$IFS`'s field-splitting rules (POSIX §2.6.5). Unset IFS defaults to
/// space/tab/newline. An explicit empty IFS (`IFS=`) disables field
/// splitting entirely. Otherwise, space/tab/newline characters *present in
/// the value* are the collapsing "IFS whitespace" class (runs collapse, and
/// leading/trailing runs vanish with no empty field); every other character
/// in the value is a "non-whitespace" delimiter where *each occurrence*
/// starts a new field on its own, even with nothing in it — `IFS=,` on
/// `a,,b` is three fields (`a`, ``, `b`), not two.
struct Ifs {
    whitespace: Vec<char>,
    other: Vec<char>,
    disabled: bool,
    /// The separator unquoted `$*` joins positional parameters with: IFS's
    /// first character, a space if IFS is unset, or nothing if IFS is set
    /// but empty.
    star_sep: String,
}

impl Ifs {
    fn current() -> Ifs {
        match var_raw("IFS") {
            None => Ifs {
                whitespace: vec![' ', '\t', '\n'],
                other: Vec::new(),
                disabled: false,
                star_sep: " ".to_string(),
            },
            Some(s) if s.is_empty() => Ifs {
                whitespace: Vec::new(),
                other: Vec::new(),
                disabled: true,
                star_sep: String::new(),
            },
            Some(s) => {
                let mut whitespace = Vec::new();
                let mut other = Vec::new();
                for c in s.chars() {
                    let bucket = if matches!(c, ' ' | '\t' | '\n') { &mut whitespace } else { &mut other };
                    if !bucket.contains(&c) {
                        bucket.push(c);
                    }
                }
                let star_sep = s.chars().next().unwrap().to_string();
                Ifs { whitespace, other, disabled: false, star_sep }
            }
        }
    }

    fn is_whitespace(&self, c: char) -> bool {
        self.whitespace.contains(&c)
    }

    fn is_delim(&self, c: char) -> bool {
        self.whitespace.contains(&c) || self.other.contains(&c)
    }
}

/// One argument under construction: its literal text, its glob pattern (with
/// non-active metacharacters escaped), whether any of it was quoted or has
/// active glob metacharacters, and whether `$IFS` itself demarcated this
/// field (kept even if empty, unlike an ordinary empty unquoted expansion).
#[derive(Default)]
struct Field {
    plain: String,
    pattern: String,
    quoted: bool,
    globbable: bool,
    explicit: bool,
}

/// Assembles a word's parts into fields, splitting on `$IFS` from unquoted
/// expansions.
#[derive(Default)]
struct Splitter {
    fields: Vec<Field>,
    /// An IFS-whitespace run was seen: the *next* real content opens a new
    /// field, but nothing is forced if none follows (trailing whitespace
    /// produces no empty field). A non-whitespace IFS delimiter is handled
    /// separately by `hard_boundary`, which opens (and closes) a field
    /// immediately, empty or not.
    soft_pending: bool,
}

impl Splitter {
    /// The field currently accepting content, opening a new one if a boundary
    /// is pending or none exists yet.
    fn current(&mut self) -> &mut Field {
        if self.soft_pending || self.fields.is_empty() {
            self.fields.push(Field::default());
            self.soft_pending = false;
        }
        self.fields.last_mut().unwrap()
    }

    /// Add quoted/literal text: never split, metacharacters escaped.
    fn add_unsplit(&mut self, s: &str) {
        let f = self.current();
        f.plain.push_str(s);
        escape_meta_into(&mut f.pattern, s);
        f.quoted = true;
    }

    /// Add the result of an unquoted expansion: `$IFS` characters become
    /// field boundaries (whitespace collapses; non-whitespace delimiters
    /// don't), and metacharacters stay active for globbing.
    fn add_split(&mut self, text: &str, ifs: &Ifs) {
        if ifs.disabled {
            let f = self.current();
            f.plain.push_str(text);
            f.pattern.push_str(text);
            if text.contains(['*', '?', '[']) {
                f.globbable = true;
            }
            return;
        }

        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if ifs.is_delim(c) {
                // A maximal run of IFS characters: each non-whitespace one is
                // its own delimiter (hard boundary); whitespace anywhere in
                // the run is filler, absorbed rather than adding a boundary
                // of its own — but only when at least one non-whitespace
                // delimiter is actually present in this run.
                let mut hard = 0usize;
                while let Some(&next) = chars.peek() {
                    if !ifs.is_delim(next) {
                        break;
                    }
                    if !ifs.is_whitespace(next) {
                        hard += 1;
                    }
                    chars.next();
                }
                if hard > 0 {
                    for _ in 0..hard {
                        self.hard_boundary();
                    }
                } else {
                    self.soft_pending = true;
                }
            } else {
                let mut chunk = String::new();
                while matches!(chars.peek(), Some(&c) if !ifs.is_delim(c)) {
                    chunk.push(chars.next().unwrap());
                }
                let f = self.current();
                f.plain.push_str(&chunk);
                f.pattern.push_str(&chunk);
                if chunk.contains(['*', '?', '[']) {
                    f.globbable = true;
                }
            }
        }
    }

    /// A non-whitespace `$IFS` character always delimits a field on its own,
    /// even with nothing on one (or both) sides.
    fn hard_boundary(&mut self) {
        if self.fields.is_empty() {
            self.fields.push(Field::default());
        }
        self.fields.last_mut().unwrap().explicit = true;
        self.fields.push(Field::default());
        self.fields.last_mut().unwrap().explicit = true;
        self.soft_pending = false;
    }
}

/// Append `s` to a glob pattern, backslash-escaping characters that would
/// otherwise be metacharacters — used for text that must stay literal.
fn escape_meta_into(pattern: &mut String, s: &str) {
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
}

fn expand_word(word: &Word) -> Result<String, String> {
    let mut out = String::new();
    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => out.push_str(s),
            WordPart::Quoted(s) => out.push_str(&expand_dollars(s)?),
            WordPart::Unquoted(s) => {
                // Tilde only expands at the very start of a word.
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                out.push_str(&expand_dollars(&text)?);
            }
        }
    }
    Ok(out)
}

/// `~` or `~/...` at the start of a string becomes `$HOME`. `~user` is not
/// supported; it is left untouched.
fn tilde_expand(text: &str) -> String {
    if let Some(rest) = text.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            if let Some(home) = home_dir() {
                return format!("{home}{rest}");
            }
        }
    }
    text.to_string()
}

/// Scan a string for `$VAR`, `${VAR}`, and `$(...)`, expanding each in place.
fn expand_dollars(text: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }

        match chars.peek() {
            Some('(') => {
                chars.next(); // consume the first '('
                if chars.peek() == Some(&'(') {
                    // `$((expr))` — arithmetic. `$`-references inside (e.g. `$1`,
                    // `$x`) are expanded first, then the result is evaluated.
                    chars.next();
                    let expr = take_arith(&mut chars)?;
                    let expr = expand_dollars(&expr)?;
                    out.push_str(&crate::arith::eval(&expr)?.to_string());
                } else {
                    // `$(...)` — command substitution. Drops trailing newlines
                    // (and the `\r` that precedes them on Windows).
                    let inner = take_balanced_paren(&mut chars)?;
                    let output = command_substitute(&inner)?;
                    out.push_str(output.trim_end_matches(['\n', '\r']));
                }
            }
            // `$?` — the last pipeline's exit status.
            Some('?') => {
                chars.next();
                out.push_str(&crate::vars::last_status().to_string());
            }
            // `$#` — number of positional parameters.
            Some('#') => {
                chars.next();
                out.push_str(&crate::vars::arg_count().to_string());
            }
            // `$@` — all positional parameters, space-joined here. (A
            // standalone `"$@"` keeps each parameter separate; see below.)
            Some('@') => {
                chars.next();
                out.push_str(&crate::vars::args().join(" "));
            }
            // `$*` — all positional parameters, joined with `$IFS`'s first
            // character (space if unset, nothing if IFS is set but empty).
            Some('*') => {
                chars.next();
                out.push_str(&crate::vars::args().join(&Ifs::current().star_sep));
            }
            // `$0`–`$9` — positional parameters.
            Some(&c) if c.is_ascii_digit() => {
                chars.next();
                let n = (c as u8 - b'0') as usize;
                out.push_str(&crate::vars::arg(n).unwrap_or_default());
            }
            Some('{') => {
                chars.next(); // consume '{'
                let mut inner = String::new();
                let mut depth = 1usize;
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            closed = true;
                            break;
                        }
                    }
                    inner.push(c);
                }
                if !closed {
                    return Err("unterminated `${`".into());
                }
                out.push_str(&expand_braced(&inner)?);
            }
            Some(&c2) if c2 == '_' || c2.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '_' || nc.is_ascii_alphanumeric() {
                        name.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(&var_lookup(&name));
            }
            // A lone `$` (or one before punctuation/digits we don't handle yet)
            // is just a literal dollar sign.
            _ => out.push('$'),
        }
    }

    Ok(out)
}

/// Read up to the matching `)` after an already-consumed `(`, returning the
/// inner text. Tracks nesting and quoted spans.
fn take_balanced_paren(chars: &mut Peekable<Chars>) -> Result<String, String> {
    let mut inner = String::new();
    let mut depth = 1usize;
    let mut quote: Option<char> = None;

    for c in chars.by_ref() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(inner);
                    }
                }
                _ => {}
            },
        }
        inner.push(c);
    }

    Err("unterminated `$(`".into())
}

/// Read an arithmetic expression up to the closing `))`, after `$((` has been
/// consumed. Inner parentheses are balanced.
fn take_arith(chars: &mut Peekable<Chars>) -> Result<String, String> {
    let mut expr = String::new();
    let mut depth = 0usize;
    loop {
        match chars.next() {
            None => return Err("unterminated `$((`".into()),
            Some('(') => {
                depth += 1;
                expr.push('(');
            }
            Some(')') if depth > 0 => {
                depth -= 1;
                expr.push(')');
            }
            // A `)` at depth 0 must be the first of the closing `))`.
            Some(')') => {
                return match chars.next() {
                    Some(')') => Ok(expr),
                    _ => Err("unterminated `$((`".into()),
                };
            }
            Some(c) => expr.push(c),
        }
    }
}

/// Run `src` as its own command line (operators and all) and capture its stdout.
fn command_substitute(src: &str) -> Result<String, String> {
    let list = parser::parse(src).map_err(|e| e.to_string())?;
    crate::exec::capture_list(&list)
}

/// A variable's value, or `None` if unset — shell variables shadow the
/// environment.
fn var_raw(name: &str) -> Option<String> {
    crate::vars::get(name).or_else(|| std::env::var(name).ok())
}

/// As [`var_raw`], but an unset variable expands to empty.
fn var_lookup(name: &str) -> String {
    var_raw(name).unwrap_or_default()
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Expand the inside of a `${...}`: a plain name, `${#name}` (length), one of
/// the pattern-removal operators `#` `##` `%` `%%`, or one of the
/// default/alternate operators `:-` `-` `:=` `=` `:+` `+` `:?` `?`. With a
/// colon the test is "unset *or* empty"; without, just "unset". (Unlike the
/// default/alternate family, `#`/`##`/`%`/`%%` have no colon form — bash
/// doesn't define one either.)
fn expand_braced(inner: &str) -> Result<String, String> {
    // Special parameters: `${#}`, `${@}`/`${*}`, and numeric `${10}`.
    match inner {
        "#" => return Ok(crate::vars::arg_count().to_string()),
        "@" => return Ok(crate::vars::args().join(" ")),
        "*" => return Ok(crate::vars::args().join(&Ifs::current().star_sep)),
        _ if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) => {
            let n: usize = inner.parse().map_err(|_| format!("${{{inner}}}: bad substitution"))?;
            return Ok(crate::vars::arg(n).unwrap_or_default());
        }
        _ => {}
    }

    if let Some(name) = inner.strip_prefix('#') {
        if !is_valid_name(name) {
            return Err(format!("${{{inner}}}: bad substitution"));
        }
        return Ok(var_lookup(name).chars().count().to_string());
    }

    let name_end = inner
        .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .unwrap_or(inner.len());
    let name = &inner[..name_end];
    let rest = &inner[name_end..];

    if !is_valid_name(name) {
        return Err(format!("${{{inner}}}: bad substitution"));
    }
    if rest.is_empty() {
        return Ok(var_lookup(name));
    }

    // Pattern-removal: `##`/`%%` before `#`/`%` so the doubled (greedy) form
    // isn't mistaken for the single form plus a literal leading `#`/`%`.
    if let Some(word_src) = rest.strip_prefix("##") {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_prefix_pattern(&var_lookup(name), &pattern, true));
    }
    if let Some(word_src) = rest.strip_prefix('#') {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_prefix_pattern(&var_lookup(name), &pattern, false));
    }
    if let Some(word_src) = rest.strip_prefix("%%") {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_suffix_pattern(&var_lookup(name), &pattern, true));
    }
    if let Some(word_src) = rest.strip_prefix('%') {
        let pattern = expand_dollars(word_src)?;
        return Ok(strip_suffix_pattern(&var_lookup(name), &pattern, false));
    }

    let colon = rest.starts_with(':');
    let ops = if colon { &rest[1..] } else { rest };
    let op = ops.chars().next();
    let word = expand_dollars(&ops[op.map_or(0, char::len_utf8)..])?;

    let value = var_raw(name);
    let use_word = match &value {
        None => true,
        Some(v) => colon && v.is_empty(),
    };

    match op {
        // `:-` / `-`: substitute the word when unset (or empty).
        Some('-') => Ok(if use_word { word } else { value.unwrap() }),
        // `:=` / `=`: also assign the word back to the variable.
        Some('=') => {
            if use_word {
                crate::vars::set(name, &word);
                Ok(word)
            } else {
                Ok(value.unwrap())
            }
        }
        // `:+` / `+`: substitute the word only when set (and non-empty).
        Some('+') => Ok(if use_word { String::new() } else { word }),
        // `:?` / `?`: error out when unset (or empty).
        Some('?') => {
            if use_word {
                let msg = if word.is_empty() {
                    format!("{name}: parameter null or not set")
                } else {
                    word
                };
                Err(msg)
            } else {
                Ok(value.unwrap())
            }
        }
        _ => Err(format!("${{{inner}}}: bad substitution")),
    }
}

/// `${var#pattern}` (shortest) / `${var##pattern}` (longest, `greedy`): strip
/// a matching prefix. Tries prefixes of increasing length (shortest first) or
/// decreasing length (longest first) against `pattern` as a whole — a glob
/// pattern, via the same matcher `case` patterns use — and removes the first
/// one that fully matches. No match: the value is returned unchanged.
fn strip_prefix_pattern(value: &str, pattern: &str, greedy: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let lens: Box<dyn Iterator<Item = usize>> = if greedy {
        Box::new((0..=chars.len()).rev())
    } else {
        Box::new(0..=chars.len())
    };
    for l in lens {
        let prefix: String = chars[..l].iter().collect();
        if crate::glob::match_component(pattern, &prefix) {
            return chars[l..].iter().collect();
        }
    }
    value.to_string()
}

/// `${var%pattern}` (shortest) / `${var%%pattern}` (longest, `greedy`): strip
/// a matching suffix — the mirror image of [`strip_prefix_pattern`].
fn strip_suffix_pattern(value: &str, pattern: &str, greedy: bool) -> String {
    let chars: Vec<char> = value.chars().collect();
    let starts: Box<dyn Iterator<Item = usize>> = if greedy {
        Box::new(0..=chars.len())
    } else {
        Box::new((0..=chars.len()).rev())
    };
    for start in starts {
        let suffix: String = chars[start..].iter().collect();
        if crate::glob::match_component(pattern, &suffix) {
            return chars[..start].iter().collect();
        }
    }
    value.to_string()
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(input: &str) -> Vec<String> {
        expand_cmd(input).argv
    }

    fn expand_cmd(input: &str) -> Command {
        let list = parser::parse(input).unwrap();
        let pipeline = expand(&list.jobs[0].list.first).unwrap();
        match &pipeline.commands[0] {
            crate::exec::Stage::Simple(cmd) => cmd.clone(),
            crate::exec::Stage::Compound(_) => panic!("expected a simple command"),
        }
    }

    // All env-mutating cases live in one test: `set_var` is process-global and
    // unsafe under edition 2024, so we keep the mutations off the shared
    // parallel test threads' way by confining them here.
    #[test]
    fn variable_tilde_and_quoting() {
        unsafe {
            std::env::set_var("RUSH_X", "hello world");
            std::env::set_var("HOME", "/home/rush");
            std::env::remove_var("RUSH_UNSET");
        }

        // Unquoted $VAR / ${VAR} word-split on whitespace; quotes suppress that.
        assert_eq!(one("echo $RUSH_X"), vec!["echo", "hello", "world"]);
        assert_eq!(one("echo ${RUSH_X}"), vec!["echo", "hello", "world"]);
        assert_eq!(one("echo \"$RUSH_X\""), vec!["echo", "hello world"]);

        // Single quotes are literal.
        assert_eq!(one("echo '$RUSH_X'"), vec!["echo", "$RUSH_X"]);

        // Unset → empty. Bare empty drops out; quoted empty is kept.
        assert_eq!(one("echo $RUSH_UNSET done"), vec!["echo", "done"]);
        assert_eq!(one("echo \"$RUSH_UNSET\" done"), vec!["echo", "", "done"]);

        // Tilde at word start only, and joined with the rest of the word.
        assert_eq!(one("echo ~"), vec!["echo", "/home/rush"]);
        assert_eq!(one("echo ~/src"), vec!["echo", "/home/rush/src"]);
        assert_eq!(one("echo a~b"), vec!["echo", "a~b"]);

        // Adjacency: a literal joins the first split field.
        assert_eq!(one("echo pre$RUSH_X"), vec!["echo", "prehello", "world"]);
    }

    #[test]
    fn word_splitting() {
        crate::vars::set("RUSH_LIST", "a b c");
        assert_eq!(one("echo $RUSH_LIST"), vec!["echo", "a", "b", "c"]);

        // Leading/trailing and runs of whitespace collapse.
        crate::vars::set("RUSH_PAD", "  x   y  ");
        assert_eq!(one("echo $RUSH_PAD"), vec!["echo", "x", "y"]);

        // A field that splits away to nothing leaves no argument.
        crate::vars::set("RUSH_EMPTY", "");
        assert_eq!(one("echo a$RUSH_EMPTY b"), vec!["echo", "a", "b"]);

        // Command substitution splits the same way.
        crate::vars::set("RUSH_CS", "one two");
        assert_eq!(one("echo \"$RUSH_CS\""), vec!["echo", "one two"]);
    }

    #[test]
    fn custom_ifs_field_splitting() {
        // A non-whitespace `$IFS` character: each occurrence delimits a
        // field on its own, unlike whitespace's collapsing — `a,,b` is three
        // fields, not two with a merged gap.
        crate::vars::set("IFS", ",");
        crate::vars::set("RUSH_CSV", "a,,b,c");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", "", "b", "c"]);

        // A leading delimiter produces a leading empty field; a single
        // *trailing* one at the very end does not (matches real bash) — but
        // a repeated trailing one still leaves one behind.
        crate::vars::set("RUSH_CSV", ",a,");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "", "a"]);
        crate::vars::set("RUSH_CSV", "a,,");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", ""]);

        // Mixed whitespace + non-whitespace IFS: whitespace immediately
        // adjacent to a non-whitespace delimiter is absorbed into it rather
        // than adding its own extra boundary.
        crate::vars::set("IFS", " ,");
        crate::vars::set("RUSH_CSV", "a, b,, c");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a", "b", "", "c"]);

        // `IFS=` (explicitly empty) disables field splitting entirely — the
        // whole expansion is one field, whitespace and all.
        crate::vars::set("IFS", "");
        crate::vars::set("RUSH_CSV", "a  b");
        assert_eq!(one("echo $RUSH_CSV"), vec!["echo", "a  b"]);

        crate::vars::unset("IFS");
    }

    #[test]
    fn star_join_honors_ifs_first_char() {
        crate::vars::set_args("rush".to_string(), vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        // Unset IFS: `$*`/`${*}` join with a space, same as always.
        crate::vars::unset("IFS");
        assert_eq!(one("echo \"$*\""), vec!["echo", "a b c"]);
        assert_eq!(one("echo \"${*}\""), vec!["echo", "a b c"]);

        // Custom IFS: joined with its *first* character, not a literal space.
        crate::vars::set("IFS", ":");
        assert_eq!(one("echo \"$*\""), vec!["echo", "a:b:c"]);
        assert_eq!(one("echo \"${*}\""), vec!["echo", "a:b:c"]);

        // `$@` is unaffected — always space-joined regardless of IFS (when
        // not the standalone `"$@"` idiom, which instead yields separate
        // arguments — see `variable_tilde_and_quoting`-adjacent behavior).
        assert_eq!(one("echo \"x$@y\""), vec!["echo", "xa b cy"]);

        crate::vars::unset("IFS");
        crate::vars::set_args("rush".to_string(), Vec::new());
    }

    #[test]
    fn lone_dollar_is_literal() {
        assert_eq!(one("echo $"), vec!["echo", "$"]);
        assert_eq!(one("echo a$ b"), vec!["echo", "a$", "b"]);
    }

    #[test]
    fn last_status_expands() {
        crate::vars::set_last_status(42);
        assert_eq!(one("echo $?"), vec!["echo", "42"]);
        crate::vars::set_last_status(0);
        assert_eq!(one("echo code=$?"), vec!["echo", "code=0"]);
    }

    #[test]
    fn assignments_split_from_argv() {
        let c = expand_cmd("FOO=bar");
        assert!(c.argv.is_empty());
        assert_eq!(c.assignments, vec![("FOO".into(), "bar".into())]);

        let c = expand_cmd("A=1 B=2 echo hi");
        assert_eq!(c.argv, vec!["echo", "hi"]);
        assert_eq!(c.assignments, vec![("A".into(), "1".into()), ("B".into(), "2".into())]);
    }

    #[test]
    fn not_an_assignment() {
        // After the command word, `NAME=value` is a plain argument.
        let c = expand_cmd("echo FOO=bar");
        assert!(c.assignments.is_empty());
        assert_eq!(c.argv, vec!["echo", "FOO=bar"]);

        // Invalid identifier → not an assignment.
        let c = expand_cmd("1FOO=bar");
        assert!(c.assignments.is_empty());
        assert_eq!(c.argv, vec!["1FOO=bar"]);
    }

    #[test]
    fn assignment_value_is_expanded() {
        crate::vars::set("RUSH_BASE", "/base");
        let c = expand_cmd("P=$RUSH_BASE/x");
        assert_eq!(c.assignments, vec![("P".into(), "/base/x".into())]);
    }

    #[test]
    fn shell_var_shadows_env() {
        crate::vars::set("RUSH_SHADOW", "shellval");
        assert_eq!(one("echo $RUSH_SHADOW"), vec!["echo", "shellval"]);
    }

    #[test]
    fn braced_default_and_alternate() {
        crate::vars::unset("RUSH_D");
        // :- substitutes a default for unset/empty (default may have spaces).
        assert_eq!(one("echo ${RUSH_D:-fallback}"), vec!["echo", "fallback"]);
        assert_eq!(one("echo \"${RUSH_D:-a b}\""), vec!["echo", "a b"]);

        crate::vars::set("RUSH_D", "set");
        assert_eq!(one("echo ${RUSH_D:-fallback}"), vec!["echo", "set"]);
        // :+ is the mirror: word only when set.
        assert_eq!(one("echo ${RUSH_D:+yes}"), vec!["echo", "yes"]);
        crate::vars::set("RUSH_D", "");
        assert_eq!(one("echo ${RUSH_D:+yes}"), vec!["echo"]); // empty → dropped
    }

    #[test]
    fn braced_assign_default_and_length() {
        crate::vars::unset("RUSH_A");
        // := assigns the default back to the variable...
        assert_eq!(one("echo ${RUSH_A:=created}"), vec!["echo", "created"]);
        // ...so a later reference sees it.
        assert_eq!(one("echo $RUSH_A"), vec!["echo", "created"]);
        // ${#name} is the length.
        assert_eq!(one("echo ${#RUSH_A}"), vec!["echo", "7"]);
    }

    #[test]
    fn braced_error_when_unset() {
        crate::vars::unset("RUSH_Q");
        let list = parser::parse("echo ${RUSH_Q:?missing}").unwrap();
        let err = expand(&list.jobs[0].list.first).unwrap_err();
        assert!(err.contains("missing"));
    }

    #[test]
    fn braced_prefix_and_suffix_pattern_removal() {
        crate::vars::set("RUSH_P", "/usr/local/bin/rush");
        // `#`/`%` remove the shortest match; `##`/`%%` the longest.
        assert_eq!(one("echo ${RUSH_P#*/}"), vec!["echo", "usr/local/bin/rush"]);
        assert_eq!(one("echo ${RUSH_P##*/}"), vec!["echo", "rush"]);

        crate::vars::set("RUSH_P", "archive.tar.gz");
        assert_eq!(one("echo ${RUSH_P%.*}"), vec!["echo", "archive.tar"]);
        assert_eq!(one("echo ${RUSH_P%%.*}"), vec!["echo", "archive"]);

        // No match: the value is returned unchanged.
        crate::vars::set("RUSH_P", "hello");
        assert_eq!(one("echo ${RUSH_P#foo}"), vec!["echo", "hello"]);

        // `*` can match zero characters, so the shortest-match forms are a
        // no-op while the longest-match forms consume the whole value.
        // Quoted so the brackets can't be mistaken for a glob character class.
        assert_eq!(one("echo \"[${RUSH_P#*}]\""), vec!["echo", "[hello]"]);
        assert_eq!(one("echo \"[${RUSH_P##*}]\""), vec!["echo", "[]"]);

        // Unset: empty string in, empty string out.
        crate::vars::unset("RUSH_P");
        assert_eq!(one("echo \"[${RUSH_P#foo}]\""), vec!["echo", "[]"]);
    }

    // Globbing tests run from the crate root against stable repo fixtures.
    #[test]
    fn glob_expands_unquoted_pattern() {
        let mut got = one("ls Cargo.*");
        got.sort();
        assert_eq!(got, vec!["Cargo.lock", "Cargo.toml", "ls"]);
    }

    #[test]
    fn quoted_pattern_is_literal() {
        // The `*` came from a quoted part, so it must not glob.
        assert_eq!(one("ls \"Cargo.*\""), vec!["ls", "Cargo.*"]);
    }

    #[test]
    fn unmatched_glob_stays_literal() {
        assert_eq!(one("ls no-such-*.zzz"), vec!["ls", "no-such-*.zzz"]);
    }
}

