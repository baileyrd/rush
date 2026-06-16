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
//! a pattern that matches nothing is left as its literal text. We do *not* do
//! whitespace word-splitting of expansion results yet. A bare expansion that
//! comes out empty drops out the way `echo $UNSET` does in a real shell.

use std::iter::Peekable;
use std::str::Chars;

use crate::exec::{Command, Pipeline, Redirect};
use crate::lexer::{Word, WordPart};
use crate::parser::{self, RawCommand, RawPipeline, RawRedirect};

pub fn expand(raw: &RawPipeline) -> Result<Pipeline, String> {
    let mut commands = Vec::with_capacity(raw.commands.len());
    for rc in &raw.commands {
        commands.push(expand_command(rc)?);
    }
    Ok(Pipeline { commands })
}

fn expand_command(rc: &RawCommand) -> Result<Command, String> {
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

    let mut redirects = Vec::with_capacity(rc.redirects.len());
    for r in &rc.redirects {
        redirects.push(match r {
            RawRedirect::Stdin(w) => Redirect::Stdin(expand_word(w)?),
            RawRedirect::Stdout { file, append } => {
                Redirect::Stdout { file: expand_word(file)?, append: *append }
            }
        });
    }

    Ok(Command { argv, redirects, assignments })
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
    let mut sp = Splitter::default();

    for (i, part) in word.iter().enumerate() {
        match part {
            WordPart::Literal(s) => sp.add_unsplit(s),
            WordPart::Quoted(s) => sp.add_unsplit(&expand_dollars(s)?),
            WordPart::Unquoted(s) => {
                let text = if i == 0 { tilde_expand(s) } else { s.clone() };
                sp.add_split(&expand_dollars(&text)?);
            }
        }
    }

    let mut out = Vec::new();
    for field in sp.fields {
        if field.globbable {
            let matches = crate::glob::glob(&field.pattern);
            if !matches.is_empty() {
                out.extend(matches);
                continue;
            }
        }
        if field.plain.is_empty() && !field.quoted {
            continue; // unquoted-empty field drops out
        }
        out.push(field.plain);
    }
    Ok(out)
}

/// One argument under construction: its literal text, its glob pattern (with
/// non-active metacharacters escaped), and whether any of it was quoted or has
/// active glob metacharacters.
#[derive(Default)]
struct Field {
    plain: String,
    pattern: String,
    quoted: bool,
    globbable: bool,
}

/// Assembles a word's parts into fields, splitting on whitespace from unquoted
/// expansions.
#[derive(Default)]
struct Splitter {
    fields: Vec<Field>,
    /// A field boundary is pending: the next content starts a new field.
    delim: bool,
}

impl Splitter {
    /// The field currently accepting content, opening a new one if a boundary
    /// is pending or none exists yet.
    fn current(&mut self) -> &mut Field {
        if self.delim || self.fields.is_empty() {
            self.fields.push(Field::default());
            self.delim = false;
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

    /// Add the result of an unquoted expansion: whitespace becomes field
    /// boundaries, and metacharacters stay active for globbing.
    fn add_split(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
                    chars.next();
                }
                self.delim = true;
            } else {
                let mut chunk = String::new();
                while matches!(chars.peek(), Some(c) if !c.is_whitespace()) {
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
                chars.next(); // consume '('
                let inner = take_balanced_paren(&mut chars)?;
                let output = command_substitute(&inner)?;
                // Command substitution drops trailing newlines (and the `\r`
                // that precedes them on Windows).
                out.push_str(output.trim_end_matches(['\n', '\r']));
            }
            // `$?` — the last pipeline's exit status.
            Some('?') => {
                chars.next();
                out.push_str(&crate::vars::last_status().to_string());
            }
            Some('{') => {
                chars.next(); // consume '{'
                let mut name = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if !closed {
                    return Err("unterminated `${`".into());
                }
                out.push_str(&var_lookup(&name));
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

/// Run `src` as its own command line (operators and all) and capture its stdout.
fn command_substitute(src: &str) -> Result<String, String> {
    let list = parser::parse(src)?;
    crate::exec::capture_list(&list)
}

/// Shell variables shadow the environment; unknown names expand to empty.
fn var_lookup(name: &str) -> String {
    crate::vars::get(name)
        .or_else(|| std::env::var(name).ok())
        .unwrap_or_default()
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
        pipeline.commands[0].clone()
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
