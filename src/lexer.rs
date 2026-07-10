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
    /// `(a "b c" d)` immediately after `NAME=`/`NAME+=` with no space — an
    /// array-literal assignment (`arr=(a b c)`). Each element is its own
    /// `Word` (so quoting/expansion inside one element still works); only
    /// ever appears as the part right after an `Unquoted` part ending in
    /// `=`/`+=` (see `looks_like_array_assign_prefix`), so every other
    /// consumer of `Word`/`WordPart` can treat it as effectively
    /// unreachable outside `expand::assignment_split`.
    ArrayLiteral(Vec<Word>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A word, ready for the expansion stage.
    Word(Word),
    Pipe,             // |
    Redirect(Redir),  // < > >> 2> 2>> 2>&1 &> …
    Semi,             // ;
    DSemi,            // ;; (case item terminator: stop)
    SemiAmp,          // ;& (case item terminator: fall through unconditionally)
    DSemiAmp,         // ;;& (case item terminator: resume pattern testing)
    And,              // &&
    Or,               // ||
    Amp,              // & (single — background)
    LParen,           // (
    RParen,           // )
    /// `((...))` with no space between the two `(` — an arithmetic
    /// command or a C-style `for ((init; cond; update))` header, always
    /// (unconditionally, matching real bash exactly, verified directly)
    /// taking priority over the alternative reading as two nested
    /// subshells, which needs an explicit space (`( (cmd) )`) to get that
    /// reading instead. Holds the raw text between the matching `((`/`))`,
    /// unsplit — the parser decides whether it's one expression or three
    /// `;`-separated clauses depending on where it appears.
    DblParen(String),
    Newline,          // a line break (also lets `&&`/`|` continue)
}

/// A redirection operator. `fd` is the file descriptor being redirected (e.g.
/// `0` for `<`, `1` for `>`, `2` for `2>`); the filename, if any, is the next
/// `Word` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redir {
    pub fd: u32,
    pub op: RedirOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirOp {
    Read,        // `<`        — fd from a file
    Write,       // `>`        — fd to a file (truncate)
    Append,      // `>>`       — fd to a file (append)
    Both,        // `&>`       — stdout+stderr to a file (truncate)
    BothAppend,  // `&>>`      — stdout+stderr to a file (append)
    Dup(u32),    // `>&n`/`<&n`— fd duplicates fd n
    /// `<<` here-document: `body` is the collected text, `expand` is false when
    /// the delimiter was quoted.
    Heredoc { body: String, expand: bool },
    /// `<<<` here-string (bash/ksh/zsh — not POSIX sh/dash): the next word,
    /// `$`-expanded and with a trailing `\n` appended, becomes stdin — the
    /// parser reads that word same as it would for `<`'s filename, and
    /// expansion feeds it through the same `heredoc` slot `<<` itself uses.
    HereString,
}

/// A lexing failure. `Incomplete` means the input is an unfinished prefix (an
/// open quote, `$(`, or here-doc) and the REPL should read more; `Syntax` is a
/// hard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    Incomplete,
    Syntax(String),
}

pub fn lex(input: &str) -> Result<Vec<Token>, LexError> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    // Here-docs seen on the current line whose bodies are read after its newline.
    let mut pending: Vec<Pending> = Vec::new();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\r' => {
                chars.next();
            }
            '\n' => {
                chars.next();
                // The bodies of any here-docs opened on this line follow now.
                for p in pending.drain(..) {
                    let body = collect_heredoc_body(&mut chars, &p.delim, p.strip)?;
                    if let Token::Redirect(Redir { op: RedirOp::Heredoc { body: slot, .. }, .. }) =
                        &mut tokens[p.idx]
                    {
                        *slot = body;
                    }
                }
                tokens.push(Token::Newline);
            }
            // A `#` at a word boundary starts a comment to end of line. Mid-word
            // (`foo#bar`) it's consumed literally by `lex_word`, never reaching
            // here; quoted, it's handled inside `lex_word` too.
            '#' => {
                while matches!(chars.peek(), Some(&c) if c != '\n') {
                    chars.next();
                }
            }
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
                match chars.peek() {
                    Some('&') => {
                        chars.next();
                        tokens.push(Token::And);
                    }
                    // `&>` / `&>>` — redirect both stdout and stderr to a file.
                    Some('>') => {
                        chars.next();
                        let op = if chars.peek() == Some(&'>') {
                            chars.next();
                            RedirOp::BothAppend
                        } else {
                            RedirOp::Both
                        };
                        tokens.push(Token::Redirect(Redir { fd: 1, op }));
                    }
                    _ => tokens.push(Token::Amp),
                }
            }
            ';' => {
                chars.next();
                if chars.peek() == Some(&';') {
                    chars.next();
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        tokens.push(Token::DSemiAmp);
                    } else {
                        tokens.push(Token::DSemi);
                    }
                } else if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::SemiAmp);
                } else {
                    tokens.push(Token::Semi);
                }
            }
            // Bare parens are operators (used by `case`); literal parens in a
            // command must be quoted. `$(...)`/`$((...))` are consumed in
            // `lex_word` before reaching here. A *second* `(` with no space
            // — `((` — is always an arithmetic command/`for` header instead
            // (see `Token::DblParen`'s own doc comment).
            '(' => {
                chars.next();
                if chars.peek() == Some(&'(') {
                    chars.next();
                    let expr = take_double_paren(&mut chars)?;
                    tokens.push(Token::DblParen(expr));
                } else {
                    tokens.push(Token::LParen);
                }
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'(') {
                    // `<(cmd)` process substitution (bash/ksh/zsh — not
                    // POSIX sh/dash): a *word*, not a redirect — verified
                    // directly that real bash always reads adjacent `<(`
                    // this way, even where the alternative (two nested
                    // subshells) would otherwise be syntactically valid.
                    let mut seed = String::from("<");
                    consume_balanced_paren(&mut chars, &mut seed)?;
                    let word = lex_word(&mut chars, Some(seed))?;
                    tokens.push(Token::Word(word));
                } else if chars.peek() == Some(&'<') {
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        // `<<<` here-string — the word that follows is
                        // read by the parser same as any other redirect's
                        // filename, not here in the lexer.
                        chars.next();
                        tokens.push(Token::Redirect(Redir { fd: 0, op: RedirOp::HereString }));
                        continue;
                    }
                    // `<<` / `<<-` here-document.
                    let strip = chars.peek() == Some(&'-');
                    if strip {
                        chars.next();
                    }
                    while matches!(chars.peek(), Some(' ') | Some('\t')) {
                        chars.next();
                    }
                    let (delim, expand) = read_heredoc_delim(&mut chars)?;
                    let idx = tokens.len();
                    tokens.push(Token::Redirect(Redir {
                        fd: 0,
                        op: RedirOp::Heredoc { body: String::new(), expand },
                    }));
                    pending.push(Pending { idx, delim, strip });
                } else {
                    tokens.push(Token::Redirect(Redir { fd: 0, op: RedirOp::Read }));
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'(') {
                    // `>(cmd)` process substitution — same rule as `<(`.
                    let mut seed = String::from(">");
                    consume_balanced_paren(&mut chars, &mut seed)?;
                    let word = lex_word(&mut chars, Some(seed))?;
                    tokens.push(Token::Word(word));
                    continue;
                }
                tokens.push(Token::Redirect(Redir { fd: 1, op: lex_gt_op(&mut chars)? }));
            }
            // A digit run immediately before `<`/`>` is an explicit fd (`2>`);
            // otherwise it's the start of a word.
            c if c.is_ascii_digit() => {
                let mut digits = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        digits.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if matches!(chars.peek(), Some('<') | Some('>')) {
                    let fd = digits
                        .parse()
                        .map_err(|_| LexError::Syntax("invalid file descriptor".into()))?;
                    tokens.push(Token::Redirect(lex_redirect(&mut chars, Some(fd))?));
                } else {
                    let word = lex_word(&mut chars, Some(digits))?;
                    tokens.push(Token::Word(word));
                }
            }
            _ => {
                let word = lex_word(&mut chars, None)?;
                tokens.push(Token::Word(word));
            }
        }
    }

    // A here-doc opened with no following line yet needs more input.
    if !pending.is_empty() {
        return Err(LexError::Incomplete);
    }
    Ok(tokens)
}

/// A here-document opened on the current line, awaiting its body.
struct Pending {
    idx: usize,
    delim: String,
    strip: bool,
}

/// Read the delimiter word after `<<`. A quoted delimiter (`<<'EOF'`) disables
/// expansion of the body.
fn read_heredoc_delim(chars: &mut Peekable<Chars>) -> Result<(String, bool), LexError> {
    let mut delim = String::new();
    let mut expand = true;
    match chars.peek() {
        Some(&q @ ('\'' | '"')) => {
            expand = false;
            chars.next();
            for c in chars.by_ref() {
                if c == q {
                    break;
                }
                delim.push(c);
            }
        }
        _ => {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || matches!(c, '|' | '<' | '>' | '&' | ';' | '(' | ')') {
                    break;
                }
                delim.push(c);
                chars.next();
            }
        }
    }
    if delim.is_empty() {
        return Err(LexError::Syntax("expected a here-document delimiter".into()));
    }
    Ok((delim, expand))
}

/// Read here-document lines until one equals the delimiter. With `strip`
/// (`<<-`), leading tabs are removed from each line and the delimiter check.
fn collect_heredoc_body(
    chars: &mut Peekable<Chars>,
    delim: &str,
    strip: bool,
) -> Result<String, LexError> {
    let mut body = String::new();
    loop {
        let Some(line) = read_line(chars) else {
            return Err(LexError::Incomplete); // EOF before the delimiter
        };
        let content = if strip { line.trim_start_matches('\t') } else { &line };
        if content == delim {
            return Ok(body);
        }
        body.push_str(content);
        body.push('\n');
    }
}

/// Read one line (without its trailing newline), or `None` at end of input.
fn read_line(chars: &mut Peekable<Chars>) -> Option<String> {
    chars.peek()?;
    let mut line = String::new();
    for c in chars.by_ref() {
        if c == '\n' {
            break;
        }
        if c != '\r' {
            line.push(c);
        }
    }
    Some(line)
}

/// Read a redirection operator with the cursor on `<` or `>`. `explicit_fd` is
/// a leading file-descriptor number (`2>`), if one was lexed.
fn lex_redirect(chars: &mut Peekable<Chars>, explicit_fd: Option<u32>) -> Result<Redir, LexError> {
    match chars.next() {
        Some('<') => Ok(Redir { fd: explicit_fd.unwrap_or(0), op: RedirOp::Read }),
        Some('>') => Ok(Redir { fd: explicit_fd.unwrap_or(1), op: lex_gt_op(chars)? }),
        _ => unreachable!("lex_redirect called off a redirection"),
    }
}

/// The operator that follows an already-consumed `>`: `>>`, `>&target`
/// (dup), or plain `>` (write) — shared by `lex_redirect` (the
/// explicit-fd-prefixed case, `2>...`) and the top-level `'>'` dispatch
/// (which needs to peek for `>(` — process substitution — before falling
/// back to this).
fn lex_gt_op(chars: &mut Peekable<Chars>) -> Result<RedirOp, LexError> {
    Ok(match chars.peek() {
        Some('>') => {
            chars.next();
            RedirOp::Append
        }
        Some('&') => {
            chars.next();
            let mut target = String::new();
            while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                target.push(chars.next().unwrap());
            }
            let t = target
                .parse()
                .map_err(|_| LexError::Syntax("expected a file descriptor after `>&`".into()))?;
            RedirOp::Dup(t)
        }
        _ => RedirOp::Write,
    })
}

/// Accumulate a single word, honoring single quotes, double quotes, escapes,
/// and keeping a `$(...)` substitution together even across spaces/operators.
/// `seed` is optional leading text (e.g. a digit run that wasn't an fd).
fn lex_word(chars: &mut Peekable<Chars>, seed: Option<String>) -> Result<Word, LexError> {
    let mut parts: Word = Vec::new();
    if let Some(s) = seed {
        if !s.is_empty() {
            parts.push(WordPart::Unquoted(s));
        }
    }

    loop {
        // `<(cmd)`/`>(cmd)` concatenated onto other text with no space
        // (`pre<(cmd)post`) — verified directly that real bash keeps
        // reading the word rather than stopping at `<`/`>` here, same as
        // it would for a `$(...)` in the same position. Checked before the
        // main match below (rather than as one of its arms) since it
        // needs a 2-char lookahead — via a cloned iterator, since
        // `Peekable` only offers one — to tell this apart from an
        // ordinary `<`/`>` that really does end the word.
        if let Some(&c) = chars.peek()
            && matches!(c, '<' | '>')
        {
            let mut lookahead = chars.clone();
            lookahead.next();
            if lookahead.peek() == Some(&'(') {
                chars.next();
                let mut s = c.to_string();
                consume_balanced_paren(chars, &mut s)?;
                push_unquoted(&mut parts, &s);
                continue;
            }
        }
        match chars.peek() {
            None => break,
            // `NAME=(` / `NAME+=(` with no space: an array-literal
            // assignment, not a subshell/group — swallow the whole `(...)`
            // as one `WordPart` instead of stopping the word here. Checked
            // before the generic `(` word-boundary arm below.
            Some(&'(') if looks_like_array_assign_prefix(&parts) => {
                let elements = lex_array_literal(chars)?;
                parts.push(WordPart::ArrayLiteral(elements));
            }
            Some(&c)
                if c == ' '
                    || c == '\t'
                    || matches!(c, '|' | '<' | '>' | '&' | ';' | '\n' | '\r' | '(' | ')') =>
            {
                break
            }
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
                            // `\$` must produce a literal `$` (POSIX-mandated,
                            // same as bash/ksh/zsh) — one that stays literal
                            // through expansion, not just a backslash-free `$`
                            // indistinguishable from a real, unescaped one
                            // (which is all that pushing it into `s` here
                            // would produce, since `s` becomes a
                            // `WordPart::Quoted` string later re-scanned for
                            // `$`/`$(...)`). Flushing `s` so far and emitting a
                            // separate `WordPart::Literal("$")` — never
                            // re-expanded, by definition — keeps that promise
                            // without needing new escape-recognition logic in
                            // `expand.rs` itself.
                            if next == '$' {
                                chars.next();
                                if !s.is_empty() {
                                    push_quoted(&mut parts, &s);
                                    s = String::new();
                                }
                                push_literal(&mut parts, "$");
                                continue;
                            }
                            if matches!(next, '"' | '\\') {
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
                    return Err(LexError::Incomplete);
                }
                // Skip an empty trailing `Quoted` part after an escaped `\$`
                // flush left nothing more to add — unless the whole quoted
                // span really was just `""`, which still needs its own
                // (empty) part to represent an explicit empty argument.
                if !s.is_empty() || parts.is_empty() {
                    push_quoted(&mut parts, &s);
                }
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

/// Whether `parts`, lexed so far, is exactly one `Unquoted` part shaped like
/// `NAME=` or `NAME+=` — the only situation where an immediately-following
/// `(` starts an array literal rather than ending the word at a subshell
/// boundary. Deliberately a lexer-level heuristic (real validation happens
/// again in `expand::assignment_split`): it only needs to be right about
/// *whether to keep lexing as one word*, not about whether the eventual
/// assignment is well-formed.
fn looks_like_array_assign_prefix(parts: &Word) -> bool {
    let [WordPart::Unquoted(s)] = parts.as_slice() else {
        return false;
    };
    let name = s.strip_suffix("+=").or_else(|| s.strip_suffix('='));
    match name {
        Some(name) => is_array_assign_name(name),
        None => false,
    }
}

fn is_array_assign_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Lex an array literal's element list, cursor on the opening `(`: skips
/// whitespace/newlines between elements (bash allows a literal to span
/// several lines), reuses `lex_word` for each element (so quoting inside
/// one still works, and it naturally stops at the next whitespace or `)`),
/// and consumes the closing `)`.
fn lex_array_literal(chars: &mut Peekable<Chars>) -> Result<Vec<Word>, LexError> {
    chars.next(); // consume '('
    let mut elements = Vec::new();
    loop {
        while matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            chars.next();
        }
        match chars.peek() {
            None => return Err(LexError::Incomplete),
            Some(')') => {
                chars.next();
                return Ok(elements);
            }
            _ => {
                let word = lex_word(chars, None)?;
                if word.is_empty() {
                    // `lex_word` consumed nothing (e.g. a stray `|`/`&`
                    // that isn't whitespace or `)`) — bail out rather than
                    // spin forever re-hitting the same character.
                    return Err(LexError::Syntax("unexpected token in array literal".into()));
                }
                elements.push(word);
            }
        }
    }
}

/// Read an arithmetic expression up to the closing `))`, after `((` has
/// already been consumed — mirrors `expand::take_arith`'s identical
/// algorithm for `$((...))`, just returning `LexError` instead of a plain
/// `String` error. A single `)` at depth 0 isn't the terminator by itself —
/// only when immediately followed by a second one.
fn take_double_paren(chars: &mut Peekable<Chars>) -> Result<String, LexError> {
    let mut expr = String::new();
    let mut depth = 0usize;
    loop {
        match chars.next() {
            None => return Err(LexError::Incomplete),
            Some('(') => {
                depth += 1;
                expr.push('(');
            }
            Some(')') if depth > 0 => {
                depth -= 1;
                expr.push(')');
            }
            Some(')') => {
                return match chars.next() {
                    Some(')') => Ok(expr),
                    _ => Err(LexError::Incomplete),
                };
            }
            Some(c) => expr.push(c),
        }
    }
}

/// Append a balanced `(...)` region (including the parens) to `out`, starting
/// at the opening `(` under the cursor. Tracks nesting and skips quoted spans
/// so that `$(echo ")")` is captured correctly.
fn consume_balanced_paren(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), LexError> {
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

    Err(LexError::Incomplete)
}

/// Append a balanced `{...}` region (including the braces) to `out`, starting at
/// the opening `{` under the cursor — used to keep `${...}` whole.
fn consume_balanced_brace(chars: &mut Peekable<Chars>, out: &mut String) -> Result<(), LexError> {
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

    Err(LexError::Incomplete)
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
    fn escaped_dollar_in_double_quotes_is_a_separate_literal_part() {
        // `\$` must produce a literal `$` — not just a backslash-free `$`
        // indistinguishable from a real, unescaped one (which is all that
        // pushing it straight into the `Quoted` string would produce, since
        // that string gets re-scanned for `$`/`$(...)` later). A trailing
        // `Literal("$")` part, never re-expanded by definition, keeps that
        // promise: `"\$?"` becomes the literal text `$?`, not the exit
        // status.
        assert_eq!(
            lex("echo \"\\$?\"").unwrap(),
            vec![
                bare("echo"),
                Token::Word(vec![WordPart::Literal("$".into()), WordPart::Quoted("?".into())]),
            ]
        );
    }

    #[test]
    fn escaped_dollar_alone_in_double_quotes_has_no_spurious_trailing_part() {
        assert_eq!(
            lex("echo \"\\$\"").unwrap(),
            vec![bare("echo"), Token::Word(vec![WordPart::Literal("$".into())])]
        );
    }

    #[test]
    fn unescaped_backslash_before_an_expansion_stays_literal() {
        // `"\\$FOO"` is a literal backslash (from the `\\` escape) followed
        // by an ordinary, still-expanding `$FOO` reference — not to be
        // confused with `\$FOO`, which is one escaped, non-expanding
        // reference. Both end up in the same `Quoted` part (only `\$`
        // triggers the flush-into-a-separate-`Literal`-part treatment), so
        // this exercises that the `\\`-then-`$` sequence isn't misread as
        // the `\$` case by whatever comes after it.
        assert_eq!(
            lex("echo \"\\\\$FOO\"").unwrap(),
            vec![bare("echo"), Token::Word(vec![WordPart::Quoted("\\$FOO".into())])]
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
                Token::Redirect(Redir { fd: 1, op: RedirOp::Write }),
                bare("out"),
            ]
        );
    }

    #[test]
    fn fd_aware_redirects() {
        use RedirOp::*;
        let r = |fd, op| Token::Redirect(Redir { fd, op });
        assert_eq!(lex("> f").unwrap(), vec![r(1, Write), bare("f")]);
        assert_eq!(lex(">> f").unwrap(), vec![r(1, Append), bare("f")]);
        assert_eq!(lex("< f").unwrap(), vec![r(0, Read), bare("f")]);
        assert_eq!(lex("2> f").unwrap(), vec![r(2, Write), bare("f")]);
        assert_eq!(lex("2>> f").unwrap(), vec![r(2, Append), bare("f")]);
        assert_eq!(lex("2>&1").unwrap(), vec![r(2, Dup(1))]);
        assert_eq!(lex("&> f").unwrap(), vec![r(1, Both), bare("f")]);
        // A digit not before a redirect is just a word.
        assert_eq!(lex("echo 2").unwrap(), vec![bare("echo"), bare("2")]);
    }

    #[test]
    fn heredoc_collects_body() {
        let body = |src| {
            lex(src).unwrap().into_iter().find_map(|t| match t {
                Token::Redirect(Redir { op: RedirOp::Heredoc { body, expand }, .. }) => {
                    Some((body, expand))
                }
                _ => None,
            })
        };
        assert_eq!(
            body("cat <<EOF\nline1\nline2\nEOF\n"),
            Some(("line1\nline2\n".to_string(), true))
        );
        // A quoted delimiter disables expansion.
        assert_eq!(body("cat <<'EOF'\n$x\nEOF\n"), Some(("$x\n".to_string(), false)));
        // `<<-` strips leading tabs from body and the delimiter line.
        assert_eq!(body("cat <<-EOF\n\tindented\n\tEOF\n"), Some(("indented\n".to_string(), true)));
    }

    #[test]
    fn heredoc_unterminated_is_incomplete() {
        assert_eq!(lex("cat <<EOF\nbody"), Err(LexError::Incomplete));
        assert_eq!(lex("cat <<EOF"), Err(LexError::Incomplete));
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
