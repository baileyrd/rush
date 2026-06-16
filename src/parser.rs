//! Build a `CommandList` from a token stream.
//!
//! Grammar (v0):
//!   list     := pipeline ( (';' | '&&' | '||') pipeline )* ';'?
//!   pipeline := command ( '|' command )*
//!   command  := word+ redirection*
//!   redirect := ('<' | '>' | '>>') word
//!
//! "Raw" here means words still carry their quoting (see [`crate::lexer::Word`])
//! and have *not* been expanded. Each pipeline is expanded lazily, left to
//! right, as the list runs (so `cd /tmp && ls *` globs in the new directory).

use std::vec::IntoIter;

use crate::lexer::{self, Token, Word};

/// A sequence of pipelines joined by control operators. `&&`/`||`/`;` have
/// equal precedence and associate left-to-right; the runner decides whether to
/// run each pipeline from the previous one's exit status.
#[derive(Debug, Clone)]
pub struct CommandList {
    pub first: RawPipeline,
    pub rest: Vec<(Connector, RawPipeline)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// `;` — run the next pipeline unconditionally.
    Seq,
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

    let first = parse_pipeline(&mut iter)?;
    let mut rest = Vec::new();

    while let Some(tok) = iter.next() {
        let connector = match tok {
            Token::Semi => Connector::Seq,
            Token::And => Connector::And,
            Token::Or => Connector::Or,
            Token::Amp => return Err("background jobs are not yet supported".into()),
            // parse_pipeline only stops at a connector or end of input.
            other => return Err(format!("unexpected token after pipeline: {other:?}")),
        };
        // A trailing `;` (or `&& ` with nothing after) just ends the list.
        if iter.peek().is_none() {
            break;
        }
        let pipeline = parse_pipeline(&mut iter)?;
        rest.push((connector, pipeline));
    }

    Ok(CommandList { first, rest })
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

    #[test]
    fn single_command() {
        let p = parse("ls -la").unwrap();
        assert!(p.rest.is_empty());
        assert_eq!(p.first.commands.len(), 1);
        assert_eq!(argv_text(&p.first.commands[0]), vec!["ls", "-la"]);
    }

    #[test]
    fn pipeline_splits() {
        let p = parse("ls | grep rs | wc -l").unwrap();
        assert_eq!(p.first.commands.len(), 3);
    }

    #[test]
    fn captures_redirects() {
        let p = parse("sort < in.txt >> out.txt").unwrap();
        let c = &p.first.commands[0];
        assert_eq!(argv_text(c), vec!["sort"]);
        assert_eq!(c.redirects.len(), 2);
    }

    #[test]
    fn dangling_pipe_errors() {
        assert!(parse("ls |").is_err());
        assert!(parse("| ls").is_err());
    }

    #[test]
    fn parses_control_operators() {
        let p = parse("a && b | c || d ; e").unwrap();
        assert_eq!(argv_text(&p.first.commands[0]), vec!["a"]);
        let connectors: Vec<Connector> = p.rest.iter().map(|(c, _)| *c).collect();
        assert_eq!(connectors, vec![Connector::And, Connector::Or, Connector::Seq]);
        // The `b | c` pipeline keeps both stages.
        assert_eq!(p.rest[0].1.commands.len(), 2);
    }

    #[test]
    fn trailing_semicolon_is_ok() {
        let p = parse("ls ;").unwrap();
        assert!(p.rest.is_empty());
    }

    #[test]
    fn empty_between_operators_errors() {
        assert!(parse("a ;; b").is_err());
        assert!(parse("&& a").is_err());
    }

    #[test]
    fn background_is_rejected_for_now() {
        assert!(parse("sleep 1 &").is_err());
    }
}
