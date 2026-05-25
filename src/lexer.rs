//! Turn a raw input line into a flat list of tokens.
//!
//! The job here is just enough to be correct about quoting and operators —
//! the parser builds structure out of these tokens. We keep operators as
//! distinct tokens so the parser never has to re-scan characters.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A bare or quoted argument, already unquoted (e.g. `hello world`).
    Word(String),
    Pipe,        // |
    Less,        // <
    Great,       // >
    DGreat,      // >>
}

pub fn lex(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '|' => {
                chars.next();
                tokens.push(Token::Pipe);
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
                // Accumulate a word, honoring single and double quotes.
                let mut word = String::new();
                loop {
                    match chars.peek() {
                        None => break,
                        Some(&c) if c == ' ' || c == '\t' || matches!(c, '|' | '<' | '>') => break,
                        Some(&'\'') => {
                            chars.next();
                            for qc in chars.by_ref() {
                                if qc == '\'' {
                                    break;
                                }
                                word.push(qc);
                            }
                        }
                        Some(&'"') => {
                            chars.next();
                            let mut closed = false;
                            while let Some(qc) = chars.next() {
                                if qc == '"' {
                                    closed = true;
                                    break;
                                }
                                // Inside double quotes, backslash escapes " and \.
                                if qc == '\\' {
                                    if let Some(&next) = chars.peek() {
                                        if next == '"' || next == '\\' {
                                            word.push(chars.next().unwrap());
                                            continue;
                                        }
                                    }
                                }
                                word.push(qc);
                            }
                            if !closed {
                                return Err("unterminated double quote".into());
                            }
                        }
                        Some(&'\\') => {
                            chars.next();
                            if let Some(esc) = chars.next() {
                                word.push(esc);
                            }
                        }
                        Some(&other) => {
                            word.push(other);
                            chars.next();
                        }
                    }
                }
                tokens.push(Token::Word(word));
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(
            lex("echo hello world").unwrap(),
            vec![
                Token::Word("echo".into()),
                Token::Word("hello".into()),
                Token::Word("world".into()),
            ]
        );
    }

    #[test]
    fn double_quotes_group_words() {
        assert_eq!(
            lex("echo \"hello world\"").unwrap(),
            vec![Token::Word("echo".into()), Token::Word("hello world".into())]
        );
    }

    #[test]
    fn operators_need_no_spaces() {
        assert_eq!(
            lex("ls|grep x>out").unwrap(),
            vec![
                Token::Word("ls".into()),
                Token::Pipe,
                Token::Word("grep".into()),
                Token::Word("x".into()),
                Token::Great,
                Token::Word("out".into()),
            ]
        );
    }

    #[test]
    fn unterminated_quote_errors() {
        assert!(lex("echo \"oops").is_err());
    }
}
