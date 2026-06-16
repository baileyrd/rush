//! Build a `CommandList` from a token stream.
//!
//! Grammar (v0):
//!   list     := job ( (';' | '&') job )* (';' | '&')?
//!   job      := pipeline ( ('&&' | '||') pipeline )*
//!   pipeline := command ( '|' command )*
//!   command  := word+ redirection*
//!   redirect := ('<' | '>' | '>>') word
//!
//! Two levels of grouping: `&&`/`||` bind a chain of pipelines into one *job*
//! (equal precedence, left-associative); `;` and `&` then separate jobs, with
//! `&` marking the preceding job to run in the background.
//!
//! "Raw" here means words still carry their quoting (see [`crate::lexer::Word`])
//! and have *not* been expanded. Each pipeline is expanded lazily, left to
//! right, as the list runs (so `cd /tmp && ls *` globs in the new directory).

use std::vec::IntoIter;

use crate::lexer::{self, Token, Word};

/// A whole command line: a sequence of jobs separated by `;`/`&`.
#[derive(Debug, Clone)]
pub struct CommandList {
    pub jobs: Vec<Job>,
}

/// One job — an and-or chain of pipelines — plus whether it runs in the
/// background (a trailing `&`).
#[derive(Debug, Clone)]
pub struct Job {
    pub list: AndOrList,
    pub background: bool,
}

/// Pipelines joined by `&&`/`||`, run left-to-right; the runner decides whether
/// to run each from the previous pipeline's exit status.
#[derive(Debug, Clone)]
pub struct AndOrList {
    pub first: RawPipeline,
    pub rest: Vec<(Connector, RawPipeline)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// `&&` — run the next pipeline only if the previous one succeeded.
    And,
    /// `||` — run the next pipeline only if the previous one failed.
    Or,
}

#[derive(Debug, Clone)]
pub struct RawCommand {
    pub argv: Vec<Word>,
    pub redirects: Vec<RawRedirect>,
}

#[derive(Debug, Clone)]
pub enum RawRedirect {
    /// `< file`
    Stdin(Word),
    /// `> file` (truncate) or `>> file` (append)
    Stdout { file: Word, append: bool },
}

#[derive(Debug, Clone)]
pub struct RawPipeline {
    pub commands: Vec<RawCommand>,
}

pub fn parse(input: &str) -> Result<CommandList, String> {
    let tokens = lexer::lex(input)?;
    let mut iter = tokens.into_iter().peekable();

    let mut jobs = Vec::new();
    loop {
        let list = parse_andor(&mut iter)?;
        // `;` ends a foreground job, `&` a background one; either may also end
        // the whole line.
        match iter.next() {
            None => {
                jobs.push(Job { list, background: false });
                break;
            }
            Some(Token::Semi) => jobs.push(Job { list, background: false }),
            Some(Token::Amp) => jobs.push(Job { list, background: true }),
            Some(other) => return Err(format!("unexpected token after job: {other:?}")),
        }
        if iter.peek().is_none() {
            break;
        }
    }

    Ok(CommandList { jobs })
}

/// Parse an and-or chain: one pipeline, then `&&`/`||`-joined pipelines.
fn parse_andor(iter: &mut Peekable) -> Result<AndOrList, String> {
    let first = parse_pipeline(iter)?;
    let mut rest = Vec::new();
    loop {
        let connector = match iter.peek() {
            Some(Token::And) => Connector::And,
            Some(Token::Or) => Connector::Or,
            _ => break,
        };
        iter.next();
        rest.push((connector, parse_pipeline(iter)?));
    }
    Ok(AndOrList { first, rest })
}

/// Parse one pipeline, stopping (without consuming) at a control operator or
/// the end of the token stream.
fn parse_pipeline(iter: &mut Peekable) -> Result<RawPipeline, String> {
    let mut commands = Vec::new();
    let mut cur = RawCommand {
        argv: Vec::new(),
        redirects: Vec::new(),
    };

    loop {
        match iter.peek() {
            None | Some(Token::Semi | Token::And | Token::Or | Token::Amp) => break,
            _ => {}
        }
        match iter.next().unwrap() {
            Token::Word(w) => cur.argv.push(w),
            Token::Pipe => {
                if cur.argv.is_empty() {
                    return Err("unexpected '|'".into());
                }
                commands.push(std::mem::replace(
                    &mut cur,
                    RawCommand { argv: Vec::new(), redirects: Vec::new() },
                ));
            }
            Token::Less => {
                let file = expect_word(iter.next(), "<")?;
                cur.redirects.push(RawRedirect::Stdin(file));
            }
            Token::Great => {
                let file = expect_word(iter.next(), ">")?;
                cur.redirects.push(RawRedirect::Stdout { file, append: false });
            }
            Token::DGreat => {
                let file = expect_word(iter.next(), ">>")?;
                cur.redirects.push(RawRedirect::Stdout { file, append: true });
            }
            // The peek above guarantees we never reach a connector here.
            tok => return Err(format!("unexpected token: {tok:?}")),
        }
    }

    if cur.argv.is_empty() {
        return Err("expected a command".into());
    }
    commands.push(cur);
    Ok(RawPipeline { commands })
}

type Peekable = std::iter::Peekable<IntoIter<Token>>;

fn expect_word(tok: Option<Token>, after: &str) -> Result<Word, String> {
    match tok {
        Some(Token::Word(w)) => Ok(w),
        _ => Err(format!("expected filename after '{after}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::WordPart;

    /// Collapse a word's parts into their raw text (ignoring quoting) so tests
    /// can assert against plain strings.
    fn text(word: &Word) -> String {
        word.iter()
            .map(|p| match p {
                WordPart::Literal(s) | WordPart::Unquoted(s) | WordPart::Quoted(s) => s.as_str(),
            })
            .collect()
    }

    fn argv_text(cmd: &RawCommand) -> Vec<String> {
        cmd.argv.iter().map(text).collect()
    }

    /// The lone job's and-or list, for tests that don't care about job structure.
    fn only(list: &CommandList) -> &AndOrList {
        assert_eq!(list.jobs.len(), 1);
        &list.jobs[0].list
    }

    #[test]
    fn single_command() {
        let p = parse("ls -la").unwrap();
        let a = only(&p);
        assert!(a.rest.is_empty());
        assert_eq!(a.first.commands.len(), 1);
        assert_eq!(argv_text(&a.first.commands[0]), vec!["ls", "-la"]);
    }

    #[test]
    fn pipeline_splits() {
        let p = parse("ls | grep rs | wc -l").unwrap();
        assert_eq!(only(&p).first.commands.len(), 3);
    }

    #[test]
    fn captures_redirects() {
        let p = parse("sort < in.txt >> out.txt").unwrap();
        let c = &only(&p).first.commands[0];
        assert_eq!(argv_text(c), vec!["sort"]);
        assert_eq!(c.redirects.len(), 2);
    }

    #[test]
    fn dangling_pipe_errors() {
        assert!(parse("ls |").is_err());
        assert!(parse("| ls").is_err());
    }

    #[test]
    fn parses_and_or() {
        let p = parse("a && b | c || d").unwrap();
        let a = only(&p);
        assert_eq!(argv_text(&a.first.commands[0]), vec!["a"]);
        let connectors: Vec<Connector> = a.rest.iter().map(|(c, _)| *c).collect();
        assert_eq!(connectors, vec![Connector::And, Connector::Or]);
        // The `b | c` pipeline keeps both stages.
        assert_eq!(a.rest[0].1.commands.len(), 2);
    }

    #[test]
    fn semicolon_separates_jobs() {
        let p = parse("a ; b ; c").unwrap();
        assert_eq!(p.jobs.len(), 3);
        assert!(p.jobs.iter().all(|j| !j.background));
    }

    #[test]
    fn ampersand_marks_background() {
        let p = parse("sleep 1 & echo done").unwrap();
        assert_eq!(p.jobs.len(), 2);
        assert!(p.jobs[0].background);
        assert!(!p.jobs[1].background);
    }

    #[test]
    fn trailing_separator_is_ok() {
        assert_eq!(parse("ls ;").unwrap().jobs.len(), 1);
        let p = parse("sleep 1 &").unwrap();
        assert_eq!(p.jobs.len(), 1);
        assert!(p.jobs[0].background);
    }

    #[test]
    fn empty_between_operators_errors() {
        assert!(parse("a ;; b").is_err());
        assert!(parse("&& a").is_err());
        assert!(parse("a && && b").is_err());
    }
}
