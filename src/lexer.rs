//! Turn a raw input line into a flat list of tokens.
//!
//! The job here is just enough to be correct about quoting and operators —
//! the parser builds structure out of these tokens, and the expansion stage
//! later resolves `$VAR`, `~`, and `$(...)`.
//!
//! Crucially, words are *not* flattened to a bare string here: quoting decides
//! what may expand later (`'$x'` is literal, `"$x"` expands), so each word is a
//! sequence of [`WordPart`]s that remember where their text came from. The
//! lexer still strips the quote characters themselves.

use std::iter::Peekable;
use std::str::Chars;

/// One word, as a sequence of differently-quoted fragments. `echo a"$b"'c'`
/// lexes to a single word with three parts.
pub type Word = Vec<WordPart>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    /// Verbatim text — from single quotes or backslash escapes. Never expanded.
    Literal(String),
    /// Bare (unquoted) text. Subject to `$`/`$(...)` expansion, and a leading
    /// `~` is eligible for home-directory expansion.
    Unquoted(String),
    /// Double-quoted text. Subject to `$`/`$(...)` expansion, but no tilde and
    /// (later) no globbing.
    Quoted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A word, ready for the expansion stage.
    Word(Word),
    Pipe,   // |
    Less,   // <
    Great,  // >
    DGreat, // >>
    Semi,   // ;
    And,    // &&
    Or,     // ||
    Amp,    // & (single — background, not yet supported)
}

pub fn lex(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            // A `#` at a word boundary starts a comment to end of line. Mid-word
            // (`foo#bar`) it's consumed literally by `lex_word`, never reaching
            // here; quoted, it's handled inside `lex_word` too.
            '#' => break,
            '|' => {
                chars.next();
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::Or);
                } else {
                    tokens.push(Token::Pipe);
                }
            }
            '&' => {
                chars.next();
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::And);
                } else {
                    tokens.push(Token::Amp);
                }
            }
            ';' => {
                chars.next();
                tokens.push(Token::Semi);
            }
            '<' => {
                chars.next();
                tokens.push(Token::Less);
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::DGreat);
                } else {
                    tokens.push(Token::Great);
                }
            }
            _ => {
                let word = lex_word(&mut chars)?;
                tokens.push(Token::Word(word));
            }
        }
    }

    Ok(tokens)
}

/// Accumulate a single word, honoring single quotes, double quotes, escapes,
/// and keeping a `$(...)` substitution together even across spaces/operators.
fn lex_word(chars: &mut Peekable<Chars>) -> Result<Word, String> {
    let mut parts: Word = Vec::new();

    loop {
        match chars.peek() {
            None => break,
            Some(&c) if c == ' ' || c == '\t' || matches!(c, '|' | '<' | '>' | '&' | ';') => break,
            Some(&'\'') => {
                chars.next();
                let mut s = String::new();
                for qc in chars.by_ref() {
                    if qc == '\'' {
                        break;
                    }
                    s.push(qc);
                }
                push_literal(&mut parts, &s);
            }
            Some(&'"') => {
                chars.next();
                let mut s = String::new();
                let mut closed = false;
                while let Some(qc) = chars.next() {
                    if qc == '"' {
                        closed = true;
                        break;
                    }
                    // Inside double quotes, backslash escapes ", \, and $.
                    if qc == '\\' {
                        if let Some(&next) = chars.peek() {
                            if matches!(next, '"' | '\\' | '$') {
                                s.push(chars.next().unwrap());
                                continue;
                            }
                        }
                    }
                    // Keep `$(...)`/`${...}` whole so an inner `"` or space
                    // can't tear them apart.
                    if qc == '$' {
                        s.push('$');
                        match chars.peek() {
                            Some(&'(') => consume_balanced_paren(chars, &mut s)?,
                            Some(&'{') => consume_balanced_brace(chars, &mut s)?,
                            _ => {}
                        }
                        continue;
                    }
                    s.push(qc);
                }
                if !closed {
                    return Err("unterminated double quote".into());
                }
                push_quoted(&mut parts, &s);
            }
            Some(&'\\') => {
                chars.next();
                if let Some(esc) = chars.next() {
                    push_literal(&mut parts, &esc.to_string());
                }
            }
            Some(&'$') => {
                chars.next();
                let mut s = String::from("$");
                // `$(...)` and `${...}` may contain spaces and operators; swallow
                // them whole so word-splitting doesn't tear them apart. A plain
                // `$VAR` falls through to ordinary char accumulation below.
                match chars.peek() {
                    Some(&'(') => consume_balanced_paren(chars, &mut s)?,
                    Some(&'{') => consume_balanced_brace(chars, &mut s)?,
                    _ => {}
                }
                push_unquoted(&mut parts, &s);
            }
            Some(&other) => {
                push_unquoted(&mut parts, &other.to_string());
                chars.next();
            }
        }
    }

    Ok(parts)
}

/// Append a balanced `(...)` region (including the parens) to `out`, starting
/// at the opening `(` under the cursor. Tracks nesting and skips quoted spans
/// so that `$(echo ")")` is captured correctly.
fn consume_balanced_paren(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), String> {
    // Cursor is on the opening '('.
    chars.next();
    out.push('(');
    let mut depth = 1usize;
    let mut quote: Option<char> = None;

    for c in chars.by_ref() {
        out.push(c);
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
                        return Ok(());
                    }
                }
                _ => {}
            },
        }
    }

    Err("unterminated `$(`".into())
}

/// Append a balanced `{...}` region (including the braces) to `out`, starting at
/// the opening `{` under the cursor — used to keep `${...}` whole.
fn consume_balanced_brace(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), String> {
    chars.next(); // opening '{'
    out.push('{');
    let mut depth = 1usize;
    let mut quote: Option<char> = None;

    for c in chars.by_ref() {
        out.push(c);
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            },
        }
    }

    Err("unterminated `${`".into())
}

fn push_unquoted(parts: &mut Word, t: &str) {
    match parts.last_mut() {
        Some(WordPart::Unquoted(s)) => s.push_str(t),
        _ => parts.push(WordPart::Unquoted(t.to_string())),
    }
}

fn push_quoted(parts: &mut Word, t: &str) {
    match parts.last_mut() {
        Some(WordPart::Quoted(s)) => s.push_str(t),
        _ => parts.push(WordPart::Quoted(t.to_string())),
    }
}

fn push_literal(parts: &mut Word, t: &str) {
    match parts.last_mut() {
        Some(WordPart::Literal(s)) => s.push_str(t),
        _ => parts.push(WordPart::Literal(t.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand: a token that is a single unquoted part.
    fn bare(s: &str) -> Token {
        Token::Word(vec![WordPart::Unquoted(s.into())])
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(
            lex("echo hello world").unwrap(),
            vec![bare("echo"), bare("hello"), bare("world")]
        );
    }

    #[test]
    fn double_quotes_group_words() {
        assert_eq!(
            lex("echo \"hello world\"").unwrap(),
            vec![bare("echo"), Token::Word(vec![WordPart::Quoted("hello world".into())])]
        );
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(
            lex("echo '$x'").unwrap(),
            vec![bare("echo"), Token::Word(vec![WordPart::Literal("$x".into())])]
        );
    }

    #[test]
    fn dollar_stays_unquoted() {
        assert_eq!(
            lex("$HOME").unwrap(),
            vec![Token::Word(vec![WordPart::Unquoted("$HOME".into())])]
        );
    }

    #[test]
    fn brace_expansion_with_spaces_is_one_word() {
        assert_eq!(
            lex("${x:-a b c}").unwrap(),
            vec![Token::Word(vec![WordPart::Unquoted("${x:-a b c}".into())])]
        );
    }

    #[test]
    fn command_substitution_is_one_word() {
        // Spaces and the pipe inside `$(...)` must not split the word.
        assert_eq!(
            lex("$(ls | wc -l)").unwrap(),
            vec![Token::Word(vec![WordPart::Unquoted("$(ls | wc -l)".into())])]
        );
    }

    #[test]
    fn adjacent_parts_merge_into_one_word() {
        assert_eq!(
            lex("a\"b\"'c'").unwrap(),
            vec![Token::Word(vec![
                WordPart::Unquoted("a".into()),
                WordPart::Quoted("b".into()),
                WordPart::Literal("c".into()),
            ])]
        );
    }

    #[test]
    fn operators_need_no_spaces() {
        assert_eq!(
            lex("ls|grep x>out").unwrap(),
            vec![
                bare("ls"),
                Token::Pipe,
                bare("grep"),
                bare("x"),
                Token::Great,
                bare("out"),
            ]
        );
    }

    #[test]
    fn control_operators() {
        assert_eq!(
            lex("a && b || c ; d &").unwrap(),
            vec![
                bare("a"),
                Token::And,
                bare("b"),
                Token::Or,
                bare("c"),
                Token::Semi,
                bare("d"),
                Token::Amp,
            ]
        );
    }

    #[test]
    fn pipe_vs_or() {
        assert_eq!(
            lex("a|b||c").unwrap(),
            vec![bare("a"), Token::Pipe, bare("b"), Token::Or, bare("c")]
        );
    }

    #[test]
    fn comment_to_end_of_line() {
        assert_eq!(lex("echo hi # a comment").unwrap(), vec![bare("echo"), bare("hi")]);
        assert!(lex("# whole line").unwrap().is_empty());
    }

    #[test]
    fn hash_is_literal_mid_word_or_quoted() {
        // Mid-word `#` is part of the word.
        assert_eq!(lex("echo foo#bar").unwrap(), vec![bare("echo"), bare("foo#bar")]);
        // Quoted `#` is literal too.
        assert_eq!(
            lex("echo '# x'").unwrap(),
            vec![bare("echo"), Token::Word(vec![WordPart::Literal("# x".into())])]
        );
    }

    #[test]
    fn unterminated_quote_errors() {
        assert!(lex("echo \"oops").is_err());
    }

    #[test]
    fn unterminated_substitution_errors() {
        assert!(lex("echo $(ls").is_err());
    }
}
