//! Build a `RawPipeline` of `RawCommand`s from a token stream.
//!
//! Grammar (v0):
//!   pipeline := command ( '|' command )*
//!   command  := word+ redirection*
//!   redirect := ('<' | '>' | '>>') word
//!
//! "Raw" here means words still carry their quoting (see [`crate::lexer::Word`])
//! and have *not* been expanded. The expansion stage turns a `RawPipeline` into
//! an [`crate::exec::Pipeline`] of concrete strings.

use crate::lexer::{self, Token, Word};

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

pub fn parse(input: &str) -> Result<RawPipeline, String> {
    let tokens = lexer::lex(input)?;
    let mut commands = Vec::new();
    let mut cur = RawCommand {
        argv: Vec::new(),
        redirects: Vec::new(),
    };

    let mut iter = tokens.into_iter().peekable();
    while let Some(tok) = iter.next() {
        match tok {
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
        }
    }

    if cur.argv.is_empty() {
        return Err("expected a command".into());
    }
    commands.push(cur);
    Ok(RawPipeline { commands })
}

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
        assert_eq!(p.commands.len(), 1);
        assert_eq!(argv_text(&p.commands[0]), vec!["ls", "-la"]);
    }

    #[test]
    fn pipeline_splits() {
        let p = parse("ls | grep rs | wc -l").unwrap();
        assert_eq!(p.commands.len(), 3);
    }

    #[test]
    fn captures_redirects() {
        let p = parse("sort < in.txt >> out.txt").unwrap();
        let c = &p.commands[0];
        assert_eq!(argv_text(c), vec!["sort"]);
        assert_eq!(c.redirects.len(), 2);
    }

    #[test]
    fn dangling_pipe_errors() {
        assert!(parse("ls |").is_err());
        assert!(parse("| ls").is_err());
    }
}
