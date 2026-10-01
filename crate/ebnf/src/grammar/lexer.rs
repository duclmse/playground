use crate::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    String(String),
    Regex(String),

    Lexer,
    Parser,
    Skip,

    Equal,
    Semi,
    Pipe,
    Comma,

    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    Arrow,

    Eof,
}

pub struct Lexer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_whitespace(&mut self) {
        loop {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                self.pos += 1;
            }

            if self.peek() == Some(b'#') {
                while let Some(c) = self.advance() {
                    if c == b'\n' {
                        break;
                    }
                }
            } else if self.peek() == Some(b'/') && self.input.get(self.pos + 1) == Some(&b'/') {
                while let Some(c) = self.advance() {
                    if c == b'\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn read_identifier(&mut self) -> String {
        let start = self.pos;

        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }

        String::from_utf8(self.input[start..self.pos].to_vec()).unwrap()
    }

    fn read_string(&mut self) -> Result<String, Error> {
        let quote = self.advance().unwrap();
        let mut out = String::new();

        loop {
            let c = self
                .advance()
                .ok_or_else(|| Error::Grammar("unterminated string".into()))?;

            if c == quote {
                break;
            }

            if c == b'\\' {
                let escaped = self
                    .advance()
                    .ok_or_else(|| Error::Grammar("unterminated escape".into()))?;

                let value = match escaped {
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'\\' => '\\',
                    b'\'' => '\'',
                    b'"' => '"',
                    other => other as char,
                };

                out.push(value);
            } else {
                out.push(c as char);
            }
        }

        Ok(out)
    }

    fn read_regex(&mut self) -> Result<String, Error> {
        self.advance(); // /

        let mut out = String::new();
        let mut escaped = false;

        loop {
            let c = self
                .advance()
                .ok_or_else(|| Error::Grammar("unterminated regex".into()))?;

            if !escaped && c == b'/' {
                break;
            }

            if c == b'\\' && !escaped {
                escaped = true;
                out.push('\\');
            } else {
                escaped = false;
                out.push(c as char);
            }
        }

        Ok(out)
    }

    pub fn next(&mut self) -> Result<Token, Error> {
        self.skip_whitespace();

        let c = match self.peek() {
            Some(c) => c,
            None => return Ok(Token::Eof),
        };

        let token = match c {
            b'=' => {
                self.advance();
                Token::Equal
            }

            b';' => {
                self.advance();
                Token::Semi
            }

            b'|' => {
                self.advance();
                Token::Pipe
            }

            b',' => {
                self.advance();
                Token::Comma
            }

            b'(' => {
                self.advance();
                Token::LParen
            }

            b')' => {
                self.advance();
                Token::RParen
            }

            b'[' => {
                self.advance();
                Token::LBracket
            }

            b']' => {
                self.advance();
                Token::RBracket
            }

            b'{' => {
                self.advance();
                Token::LBrace
            }

            b'}' => {
                self.advance();
                Token::RBrace
            }

            b'-' if self.input.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::Arrow
            }

            b'"' | b'\'' => Token::String(self.read_string()?),

            b'/' => Token::Regex(self.read_regex()?),

            c if c.is_ascii_alphabetic() || c == b'_' => {
                let ident = self.read_identifier();

                match ident.as_str() {
                    "lexer" => Token::Lexer,
                    "parser" => Token::Parser,
                    "skip" => Token::Skip,
                    _ => Token::Ident(ident),
                }
            }

            _ => {
                return Err(Error::Grammar(format!(
                    "unexpected character `{}`",
                    c as char
                )))
            }
        };

        Ok(token)
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, Error> {
        let mut result = Vec::new();

        loop {
            let token = self.next()?;
            let eof = token == Token::Eof;

            result.push(token);

            if eof {
                break;
            }
        }

        Ok(result)
    }
}
