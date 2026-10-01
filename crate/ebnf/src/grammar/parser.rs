use crate::error::Error;

use super::{
    ast::*,
    lexer::{Lexer, Token},
};

pub fn parse_grammar(input: &str) -> Result<Grammar, Error> {
    let tokens = Lexer::new(input).tokenize()?;

    Parser { tokens, pos: 0 }.parse()
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: &Token) -> Result<(), Error> {
        if self.current() == expected {
            self.advance();
            Ok(())
        } else {
            Err(Error::Grammar(format!(
                "expected {:?}, got {:?}",
                expected,
                self.current()
            )))
        }
    }

    fn parse(&mut self) -> Result<Grammar, Error> {
        let mut lexer = LexerSpec::default();
        let mut rules = Vec::new();

        while *self.current() != Token::Eof {
            match self.current() {
                Token::Lexer => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    self.parse_lexer(&mut lexer)?;
                    self.expect(&Token::RBrace)?;
                }

                Token::Parser => {
                    self.advance();
                    self.expect(&Token::LBrace)?;
                    self.parse_rules(&mut rules)?;
                    self.expect(&Token::RBrace)?;
                }

                Token::Ident(_) => {
                    self.parse_rule(&mut rules)?;
                }

                token => return Err(Error::Grammar(format!("unexpected token {:?}", token))),
            }
        }

        Ok(Grammar { lexer, rules })
    }

    fn parse_lexer(&mut self, lexer: &mut LexerSpec) -> Result<(), Error> {
        while *self.current() != Token::RBrace {
            let name = match self.current() {
                Token::Ident(name) => name.clone(),
                token => {
                    return Err(Error::Grammar(format!(
                        "expected lexer token name, got {:?}",
                        token
                    )))
                }
            };

            self.advance();
            self.expect(&Token::Equal)?;

            let pattern = match self.current() {
                Token::Regex(pattern) => pattern.clone(),
                Token::String(value) => regex_escape(value),
                token => {
                    return Err(Error::Grammar(format!(
                        "expected regex or string, got {:?}",
                        token
                    )))
                }
            };

            self.advance();

            let action = if *self.current() == Token::Arrow {
                self.advance();

                match self.current() {
                    Token::Skip => {
                        self.advance();
                        TokenAction::Skip
                    }

                    token => {
                        return Err(Error::Grammar(format!("expected `skip`, got {:?}", token)))
                    }
                }
            } else {
                TokenAction::Emit
            };

            self.expect(&Token::Semi)?;

            lexer.tokens.push(TokenDef {
                name,
                pattern,
                action,
            });
        }

        Ok(())
    }

    fn parse_rules(&mut self, rules: &mut Vec<Rule>) -> Result<(), Error> {
        while *self.current() != Token::RBrace {
            self.parse_rule(rules)?;
        }

        Ok(())
    }

    fn parse_rule(&mut self, rules: &mut Vec<Rule>) -> Result<(), Error> {
        let name = match self.current() {
            Token::Ident(name) => name.clone(),
            token => {
                return Err(Error::Grammar(format!(
                    "expected rule name, got {:?}",
                    token
                )))
            }
        };

        self.advance();
        self.expect(&Token::Equal)?;

        let expr = self.parse_choice()?;

        self.expect(&Token::Semi)?;

        rules.push(Rule { name, expr });

        Ok(())
    }

    fn parse_choice(&mut self) -> Result<Expr, Error> {
        let mut choices = vec![self.parse_sequence()?];

        while *self.current() == Token::Pipe {
            self.advance();
            choices.push(self.parse_sequence()?);
        }

        if choices.len() == 1 {
            Ok(choices.remove(0))
        } else {
            Ok(Expr::Choice(choices))
        }
    }

    fn parse_sequence(&mut self) -> Result<Expr, Error> {
        let mut values = Vec::new();

        while self.starts_expression() {
            values.push(self.parse_term()?);

            if *self.current() == Token::Comma {
                self.advance();
            }
        }

        if values.is_empty() {
            Ok(Expr::Empty)
        } else if values.len() == 1 {
            Ok(values.remove(0))
        } else {
            Ok(Expr::Sequence(values))
        }
    }

    fn starts_expression(&self) -> bool {
        matches!(
            self.current(),
            Token::Ident(_) | Token::String(_) | Token::LParen | Token::LBracket | Token::LBrace
        )
    }

    fn parse_term(&mut self) -> Result<Expr, Error> {
        match self.current() {
            Token::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::Symbol(name))
            }

            Token::String(value) => {
                let value = value.clone();
                self.advance();
                Ok(Expr::Literal(value))
            }

            Token::LParen => {
                self.advance();
                let expr = self.parse_choice()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }

            Token::LBracket => {
                self.advance();
                let expr = self.parse_choice()?;
                self.expect(&Token::RBracket)?;
                Ok(Expr::Optional(Box::new(expr)))
            }

            Token::LBrace => {
                self.advance();
                let expr = self.parse_choice()?;
                self.expect(&Token::RBrace)?;
                Ok(Expr::Repeat(Box::new(expr)))
            }

            token => Err(Error::Grammar(format!("unexpected term {:?}", token))),
        }
    }
}

fn regex_escape(value: &str) -> String {
    regex::escape(value)
}
