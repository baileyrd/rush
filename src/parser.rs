//! Build a `Pipeline` of `Command`s from a token stream.
//!
//! Grammar (v0):
//!   pipeline := command ( '|' command )*
//!   command  := word+ redirection*
//!   redirect := ('<' | '>' | '>>') word

use crate::lexer::{self, Token};

#[derive(Debug, Clone)]
pub struct Command {
    pub argv: Vec<String>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone)]
pub enum Redirect {
    /// `< file`
    Stdin(String),
    /// `> file` (truncate) or `>> file` (append)
    Stdout { file: String, append: bool },
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<Command>,
}

pub fn parse(input: &str) -> Result<Pipeline, String> {
    let tokens = lexer::lex(input)?;
    let mut commands = Vec::new();
    let mut cur = Command {
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
                    Command { argv: Vec::new(), redirects: Vec::new() },
                ));
            }
            Token::Less => {
                let file = expect_word(iter.next(), "<")?;
                cur.redirects.push(Redirect::Stdin(file));
            }
            Token::Great => {
                let file = expect_word(iter.next(), ">")?;
                cur.redirects.push(Redirect::Stdout { file, append: false });
            }
            Token::DGreat => {
                let file = expect_word(iter.next(), ">>")?;
                cur.redirects.push(Redirect::Stdout { file, append: true });
            }
        }
    }

    if cur.argv.is_empty() {
        return Err("expected a command".into());
    }
    commands.push(cur);
    Ok(Pipeline { commands })
}

fn expect_word(tok: Option<Token>, after: &str) -> Result<String, String> {
    match tok {
        Some(Token::Word(w)) => Ok(w),
        _ => Err(format!("expected filename after '{after}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_command() {
        let p = parse("ls -la").unwrap();
        assert_eq!(p.commands.len(), 1);
        assert_eq!(p.commands[0].argv, vec!["ls", "-la"]);
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
        assert_eq!(c.argv, vec!["sort"]);
        assert_eq!(c.redirects.len(), 2);
    }

    #[test]
    fn dangling_pipe_errors() {
        assert!(parse("ls |").is_err());
        assert!(parse("| ls").is_err());
    }
}
