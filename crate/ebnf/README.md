Absolutely. I’d structure it as a **Rust compiler-style pipeline** where the
frontend knows nothing about the target language, and code generation is
delegated to backend traits.

The implementation below gives you a working foundation with:

- EBNF grammar lexer
- EBNF parser
- Grammar AST
- EBNF → normalized BNF lowering
- `FIRST` / nullable analysis
- LL(1) validation
- target-independent lexer/parser IR
- backend trait
- a Rust backend as the first example
- generated recursive-descent parser structure
- regex-based lexer rules

## Project structure

```
ebnf/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── error.rs
    ├── grammar/
    │   ├── mod.rs
    │   ├── lexer.rs
    │   ├── parser.rs
    │   └── ast.rs
    ├── analysis.rs
    ├── ir.rs
    ├── lower.rs
    ├── generate.rs
    └── backend/
        ├── mod.rs
        └── rust.rs
```

The important architectural boundary is:

```
┌─────────────────────┐
│      EBNF input     │
└──────────┬──────────┘
┌──────────▼──────────┐
│    Grammar lexer    │
└──────────┬──────────┘
┌──────────▼──────────┐
│     Grammar AST     │
└──────────┬──────────┘
┌──────────▼──────────┐
│ EBNF → BNF lowering │
└──────────┬──────────┘
┌──────────▼──────────┐
│      Analysis       │
│ FIRST / nullable    │
└──────────┬──────────┘
┌──────────▼──────────┐
│   Target-neutral    │
│         IR          │
└──────────┬──────────┘
           │
┌──────────┼──────────┐
▼          ▼          ▼
Rust       C++        JS
backend    backend    backend
```

---

# 1\. `Cargo.toml`

```toml
[package]
name = "ebnf"
version = "0.1.0"
edition = "2021"

[dependencies]
regex = "1"
thiserror = "2"
```

---

# 2\. `src/lib.rs`

```rust
pub mod error;
pub mod grammar;
pub mod analysis;
pub mod lower;
pub mod ir;
pub mod generate;
pub mod backend;

pub use error::Error;
pub use grammar::ast::Grammar;
pub use grammar::parser::parse_grammar;
pub use lower::lower;
pub use generate::generate;
```

---

# 3\. Errors

`src/error.rs`

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("grammar error: {0}")]
    Grammar(String),

    #[error("parse error at {line}:{column}: {message}")]
    Parse {
        line: usize,
        column: usize,
        message: String,
    },

    #[error("undefined symbol: {0}")]
    UndefinedSymbol(String),

    #[error("duplicate symbol: {0}")]
    DuplicateSymbol(String),

    #[error("LL(1) conflict in rule `{rule}`")]
    Ll1Conflict {
        rule: String,
    },

    #[error("left recursion in rule `{0}`")]
    LeftRecursion(String),

    #[error("backend error: {0}")]
    Backend(String),
}
```

---

# 4\. Grammar AST

`src/grammar/ast.rs`

```rust
#[derive(Debug, Clone)]
pub struct Grammar {
    pub lexer: LexerSpec,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Default)]
pub struct LexerSpec {
    pub tokens: Vec<TokenDef>,
}

#[derive(Debug, Clone)]
pub struct TokenDef {
    pub name: String,
    pub pattern: String,
    pub action: TokenAction,
}

#[derive(Debug, Clone)]
pub enum TokenAction {
    Emit,
    Skip,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Empty,

    /// Literal terminal, e.g. `"if"`
    Literal(String),

    /// Token reference, e.g. IDENTIFIER.
    Symbol(String),

    Sequence(Vec<Expr>),

    Choice(Vec<Expr>),

    Optional(Box<Expr>),

    Repeat(Box<Expr>),

    OneOrMore(Box<Expr>),
}
```

---

# 5\. Grammar lexer

`src/grammar/lexer.rs`

```rust
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
            } else if self.peek() == Some(b'/')
                && self.input.get(self.pos + 1) == Some(&b'/')
            {
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
```

---

# 6\. EBNF parser

`src/grammar/parser.rs`

```rust
use crate::error::Error;

use super::{
    ast::*,
    lexer::{Lexer, Token},
};

pub fn parse_grammar(input: &str) -> Result<Grammar, Error> {
    let tokens = Lexer::new(input).tokenize()?;

    Parser {
        tokens,
        pos: 0,
    }
    .parse()
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

                token => {
                    return Err(Error::Grammar(format!(
                        "unexpected token {:?}",
                        token
                    )))
                }
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
                        return Err(Error::Grammar(format!(
                            "expected `skip`, got {:?}",
                            token
                        )))
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
            Token::Ident(_)
                | Token::String(_)
                | Token::LParen
                | Token::LBracket
                | Token::LBrace
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

            token => Err(Error::Grammar(format!(
                "unexpected term {:?}",
                token
            ))),
        }
    }
}

fn regex_escape(value: &str) -> String {
    regex::escape(value)
}
```

---

# 7\. Grammar module

`src/grammar/mod.rs`

```rust
pub mod ast;
pub mod lexer;
pub mod parser;
```

---

# 8\. Target-independent IR

This is the key part.

`src/ir.rs`

```rust
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct GrammarIr {
    pub tokens: Vec<TokenIr>,
    pub rules: Vec<RuleIr>,
    pub start: String,
}

#[derive(Debug, Clone)]
pub struct TokenIr {
    pub name: String,
    pub pattern: String,
    pub skip: bool,
}

#[derive(Debug, Clone)]
pub struct RuleIr {
    pub name: String,
    pub alternatives: Vec<AlternativeIr>,
}

#[derive(Debug, Clone)]
pub struct AlternativeIr {
    pub symbols: Vec<SymbolIr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SymbolIr {
    Terminal(String),
    Token(String),
    Rule(String),
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub nullable: HashMap<String, bool>,
    pub first: HashMap<String, Vec<FirstSymbol>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FirstSymbol {
    Terminal(String),
    Token(String),
    Epsilon,
}
```

---

# 9\. EBNF lowering

This converts things like:

```
expression = term { ("+" | "-") term };
```

into ordinary productions.

`src/lower.rs`

```rust
use std::collections::HashMap;

use crate::{
    error::Error,
    grammar::ast::*,
    ir::*,
};

pub fn lower(grammar: &Grammar) -> Result<GrammarIr, Error> {
    let mut ctx = LowerContext {
        rules: Vec::new(),
        generated: 0,
    };

    for rule in &grammar.rules {
        let alternatives = ctx.lower_expr(&rule.expr)?;

        ctx.rules.push(RuleIr {
            name: rule.name.clone(),
            alternatives,
        });
    }

    let mut tokens = Vec::new();

    for token in &grammar.lexer.tokens {
        tokens.push(TokenIr {
            name: token.name.clone(),
            pattern: token.pattern.clone(),
            skip: matches!(token.action, TokenAction::Skip),
        });
    }

    let start = grammar
        .rules
        .first()
        .ok_or_else(|| Error::Grammar("grammar contains no parser rules".into()))?
        .name
        .clone();

    validate_symbols(&ctx.rules, &tokens)?;

    Ok(GrammarIr {
        tokens,
        rules: ctx.rules,
        start,
    })
}

struct LowerContext {
    rules: Vec<RuleIr>,
    generated: usize,
}

impl LowerContext {
    fn fresh_name(&mut self) -> String {
        let name = format!("__ebnf{}", self.generated);
        self.generated += 1;
        name
    }

    fn lower_expr(
        &mut self,
        expr: &Expr,
    ) -> Result<Vec<AlternativeIr>, Error> {
        match expr {
            Expr::Empty => Ok(vec![AlternativeIr { symbols: vec![] }]),

            Expr::Literal(value) => Ok(vec![AlternativeIr {
                symbols: vec![SymbolIr::Terminal(value.clone())],
            }]),

            Expr::Symbol(name) => Ok(vec![AlternativeIr {
                symbols: vec![SymbolIr::Rule(name.clone())],
            }]),

            Expr::Sequence(items) => {
                let mut alternatives = vec![AlternativeIr { symbols: vec![] }];

                for item in items {
                    let item_alts = self.lower_expr(item)?;

                    let mut next = Vec::new();

                    for a in &alternatives {
                        for b in &item_alts {
                            let mut symbols = a.symbols.clone();
                            symbols.extend(b.symbols.clone());

                            next.push(AlternativeIr { symbols });
                        }
                    }

                    alternatives = next;
                }

                Ok(alternatives)
            }

            Expr::Choice(items) => {
                let mut result = Vec::new();

                for item in items {
                    result.extend(self.lower_expr(item)?);
                }

                Ok(result)
            }

            Expr::Optional(inner) => {
                let name = self.lower_nested(inner)?;

                Ok(vec![
                    AlternativeIr {
                        symbols: vec![SymbolIr::Rule(name.clone())],
                    },
                    AlternativeIr { symbols: vec![] },
                ])
            }

            Expr::Repeat(inner) => {
                let name = self.lower_nested(inner)?;
                let helper = self.fresh_name();

                self.rules.push(RuleIr {
                    name: helper.clone(),
                    alternatives: vec![
                        AlternativeIr {
                            symbols: vec![
                                SymbolIr::Rule(name),
                                SymbolIr::Rule(helper.clone()),
                            ],
                        },
                        AlternativeIr { symbols: vec![] },
                    ],
                });

                Ok(vec![AlternativeIr {
                    symbols: vec![SymbolIr::Rule(helper)],
                }])
            }

            Expr::OneOrMore(inner) => {
                let name = self.lower_nested(inner)?;
                let helper = self.fresh_name();

                self.rules.push(RuleIr {
                    name: helper.clone(),
                    alternatives: vec![
                        AlternativeIr {
                            symbols: vec![
                                SymbolIr::Rule(name.clone()),
                                SymbolIr::Rule(helper.clone()),
                            ],
                        },
                        AlternativeIr { symbols: vec![] },
                    ],
                });

                Ok(vec![AlternativeIr {
                    symbols: vec![SymbolIr::Rule(name), SymbolIr::Rule(helper)],
                }])
            }
        }
    }

    fn lower_nested(&mut self, expr: &Expr) -> Result<String, Error> {
        let name = self.fresh_name();
        let alternatives = self.lower_expr(expr)?;

        self.rules.push(RuleIr {
            name: name.clone(),
            alternatives,
        });

        Ok(name)
    }
}

fn validate_symbols(
    rules: &[RuleIr],
    tokens: &[TokenIr],
) -> Result<(), Error> {
    let mut symbols = HashMap::new();

    for token in tokens {
        symbols.insert(token.name.clone(), true);
    }

    for rule in rules {
        if symbols.insert(rule.name.clone(), true).is_some() {
            // Token/rule name collision.
            return Err(Error::DuplicateSymbol(rule.name.clone()));
        }
    }

    for rule in rules {
        for alternative in &rule.alternatives {
            for symbol in &alternative.symbols {
                match symbol {
                    SymbolIr::Rule(name) => {
                        if !rules.iter().any(|r| r.name == *name) {
                            return Err(Error::UndefinedSymbol(name.clone()));
                        }
                    }

                    SymbolIr::Token(name) => {
                        if !tokens.iter().any(|t| t.name == *name) {
                            return Err(Error::UndefinedSymbol(name.clone()));
                        }
                    }

                    SymbolIr::Terminal(_) => {}
                }
            }
        }
    }

    Ok(())
}
```

There is one thing worth fixing in the lowering implementation: lexer token
names need to be distinguishable from parser rules. We can infer them from the
lexer specification.

Change this section:

```
Expr::Symbol(name) => Ok(vec![AlternativeIr {
    symbols: vec![SymbolIr::Rule(name.clone())],
}]),
```

to:

```
Expr::Symbol(name) => {
    if self.token_names.contains(name) {
        Ok(vec![AlternativeIr {
            symbols: vec![SymbolIr::Token(name.clone())],
        }])
    } else {
        Ok(vec![AlternativeIr {
            symbols: vec![SymbolIr::Rule(name.clone())],
        }])
    }
}
```

and add the token set to `LowerContext`:

```
struct LowerContext {
    rules: Vec<RuleIr>,
    generated: usize,
    token_names: std::collections::HashSet<String>,
}
```

Then initialize it:

```
let token_names = grammar
    .lexer
    .tokens
    .iter()
    .map(|x| x.name.clone())
    .collect();

let mut ctx = LowerContext {
    rules: Vec::new(),
    generated: 0,
    token_names,
};
```

That's important because:

```
expression = NUMBER | IDENTIFIER;
```

must mean token references rather than parser-rule references.

---

# 10\. Grammar analysis

`src/analysis.rs`

```rust
use std::collections::{HashMap, HashSet};

use crate::{
    error::Error,
    ir::*,
};

pub fn analyze(grammar: &GrammarIr) -> Result<Analysis, Error> {
    let mut analysis = Analysis::default();

    for rule in &grammar.rules {
        analysis.nullable.insert(rule.name.clone(), false);
        analysis.first.insert(rule.name.clone(), Vec::new());
    }

    // Fixed-point nullable analysis.
    loop {
        let mut changed = false;

        for rule in &grammar.rules {
            if analysis.nullable[&rule.name] {
                continue;
            }

            for alt in &rule.alternatives {
                if alternative_nullable(alt, &analysis.nullable) {
                    analysis.nullable.insert(rule.name.clone(), true);
                    changed = true;
                    break;
                }
            }
        }

        if !changed {
            break;
        }
    }

    // Fixed-point FIRST analysis.
    loop {
        let mut changed = false;

        for rule in &grammar.rules {
            let mut additions = Vec::new();

            for alt in &rule.alternatives {
                additions.extend(first_of_alternative(
                    alt,
                    &analysis,
                ));
            }

            let first = analysis.first.get_mut(&rule.name).unwrap();

            for item in additions {
                if !first.contains(&item) {
                    first.push(item);
                    changed = true;
                }
            }
        }

        if !changed {
            break;
        }
    }

    detect_left_recursion(grammar, &analysis)?;
    check_ll1(grammar, &analysis)?;

    Ok(analysis)
}

fn alternative_nullable(
    alt: &AlternativeIr,
    nullable: &HashMap<String, bool>,
) -> bool {
    alt.symbols.iter().all(|symbol| match symbol {
        SymbolIr::Rule(name) => nullable.get(name).copied().unwrap_or(false),

        SymbolIr::Terminal(_) | SymbolIr::Token(_) => false,
    })
}

fn first_of_alternative(
    alt: &AlternativeIr,
    analysis: &Analysis,
) -> Vec<FirstSymbol> {
    let mut result = Vec::new();

    if alt.symbols.is_empty() {
        result.push(FirstSymbol::Epsilon);
        return result;
    }

    for symbol in &alt.symbols {
        match symbol {
            SymbolIr::Terminal(value) => {
                result.push(FirstSymbol::Terminal(value.clone()));
                break;
            }

            SymbolIr::Token(name) => {
                result.push(FirstSymbol::Token(name.clone()));
                break;
            }

            SymbolIr::Rule(name) => {
                if let Some(first) = analysis.first.get(name) {
                    for item in first {
                        if *item != FirstSymbol::Epsilon {
                            result.push(item.clone());
                        }
                    }
                }

                if !analysis.nullable.get(name).copied().unwrap_or(false) {
                    break;
                }
            }
        }
    }

    if alt.symbols.iter().all(|s| match s {
        SymbolIr::Rule(name) => {
            analysis.nullable.get(name).copied().unwrap_or(false)
        }
        _ => false,
    }) {
        result.push(FirstSymbol::Epsilon);
    }

    result
}

fn detect_left_recursion(
    grammar: &GrammarIr,
    analysis: &Analysis,
) -> Result<(), Error> {
    for rule in &grammar.rules {
        let mut stack = vec![rule.name.clone()];
        let mut visited = HashSet::new();

        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }

            let r = grammar
                .rules
                .iter()
                .find(|r| r.name == current)
                .unwrap();

            for alt in &r.alternatives {
                if let Some(SymbolIr::Rule(first)) = alt.symbols.first() {
                    if first == &rule.name {
                        return Err(Error::LeftRecursion(rule.name.clone()));
                    }

                    if analysis.nullable.get(first).copied().unwrap_or(false) {
                        stack.push(first.clone());
                    }
                }
            }
        }
    }

    Ok(())
}

fn check_ll1(
    grammar: &GrammarIr,
    analysis: &Analysis,
) -> Result<(), Error> {
    for rule in &grammar.rules {
        for i in 0..rule.alternatives.len() {
            for j in (i + 1)..rule.alternatives.len() {
                let a = first_of_alternative(&rule.alternatives[i], analysis);
                let b = first_of_alternative(&rule.alternatives[j], analysis);

                for x in &a {
                    if *x == FirstSymbol::Epsilon {
                        continue;
                    }

                    if b.contains(x) {
                        return Err(Error::Ll1Conflict {
                            rule: rule.name.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}
```

---

# 11\. Target-neutral generator interface

Now we separate the compiler from the language being generated.

`src/backend/mod.rs`

```rust
use crate::{
    analysis::Analysis,
    error::Error,
    ir::GrammarIr,
};

pub mod rust;

pub trait Backend {
    type Output;

    fn generate(
        &self,
        grammar: &GrammarIr,
        analysis: &Analysis,
    ) -> Result<Self::Output, Error>;
}
```

This is the critical abstraction.

A future backend can simply implement:

```rust
impl Backend for CppBackend {
    type Output = String;

    fn generate(...) -> Result<String, Error> {
        ...
    }
}
```

without touching the EBNF parser, lowering, or analysis.

---

# 12\. Generator facade

`src/generate.rs`

```rust
use crate::{
    analysis::analyze,
    backend::Backend,
    error::Error,
    grammar::ast::Grammar,
    lower::lower,
};

pub fn generate<B: Backend>(
    grammar: &Grammar,
    backend: &B,
) -> Result<B::Output, Error> {
    let ir = lower(grammar)?;
    let analysis = analyze(&ir)?;

    backend.generate(&ir, &analysis)
}
```

---

# 13\. Rust backend

Here's the first actual backend.

`src/backend/rust.rs`

```rust
use std::fmt::Write;

use crate::{
    analysis::Analysis,
    backend::Backend,
    error::Error,
    ir::*,
};

pub struct RustBackend;

impl RustBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for RustBackend {
    type Output = String;

    fn generate(
        &self,
        grammar: &GrammarIr,
        _analysis: &Analysis,
    ) -> Result<String, Error> {
        let mut out = String::new();

        emit_header(&mut out);

        emit_token_type(&mut out, grammar);
        emit_lexer(&mut out, grammar)?;
        emit_parser(&mut out, grammar)?;

        Ok(out)
    }
}

fn emit_header(out: &mut String) {
    writeln!(
        out,
        r#"// Generated by ebnf.
// DO NOT EDIT.

use regex::Regex;

#[derive(Debug, Clone)]
pub struct Span {{
    pub start: usize,
    pub end: usize,
}}

#[derive(Debug, Clone)]
pub struct Token {{
    pub kind: TokenKind,
    pub lexeme: String,
    pub span: Span,
}}
"#
    )
    .unwrap();
}

fn emit_token_type(
    out: &mut String,
    grammar: &GrammarIr,
) {
    writeln!(out, "#[derive(Debug, Clone, PartialEq, Eq)]").unwrap();
    writeln!(out, "pub enum TokenKind {{").unwrap();

    for token in &grammar.tokens {
        if !token.skip {
            writeln!(out, "    {},", token.name).unwrap();
        }
    }

    // Literals become generated token variants.
    let mut literals = Vec::new();

    for rule in &grammar.rules {
        for alt in &rule.alternatives {
            for symbol in &alt.symbols {
                if let SymbolIr::Terminal(value) = symbol {
                    if !literals.contains(value) {
                        literals.push(value.clone());
                    }
                }
            }
        }
    }

    for literal in literals {
        writeln!(out, "    {},", literal_variant(literal)).unwrap();
    }

    writeln!(out, "    Eof,").unwrap();
    writeln!(out, "}}\n").unwrap();
}

fn emit_lexer(
    out: &mut String,
    grammar: &GrammarIr,
) -> Result<(), Error> {
    writeln!(
        out,
        r#"pub struct Lexer {{
    rules: Vec<(Regex, TokenKind, bool)>,
}}

impl Lexer {{
    pub fn new() -> Result<Self, regex::Error> {{
        let mut rules = Vec::new();
"#
    )
    .unwrap();

    for token in &grammar.tokens {
        let kind = token_variant(&token.name);

        writeln!(
            out,
            "        rules.push((Regex::new(r#\"^(?:{})\"#)?, TokenKind::{}, {}));",
            token.pattern,
            kind,
            token.skip
        )
        .unwrap();
    }

    writeln!(
        out,
        r#"        Ok(Self {{ rules }})
    }}

    pub fn tokenize(&self, input: &str) -> Result<Vec<Token>, String> {{
        let mut result = Vec::new();
        let mut pos = 0;

        while pos < input.len() {{
            let rest = &input[pos..];
            let mut matched = false;

            for (regex, kind, skip) in &self.rules {{
                if let Some(m) = regex.find(rest) {{
                    if m.start() != 0 {{
                        continue;
                    }}

                    let text = m.as_str();

                    if text.is_empty() {{
                        return Err(format!(
                            "lexer rule matched empty input at {{}}",
                            pos
                        ));
                    }}

                    let end = pos + text.len();

                    if !*skip {{
                        result.push(Token {{
                            kind: kind.clone(),
                            lexeme: text.to_string(),
                            span: Span {{
                                start: pos,
                                end,
                            }},
                        }});
                    }}

                    pos = end;
                    matched = true;
                    break;
                }}
            }}

            if !matched {{
                return Err(format!(
                    "unexpected character at byte {{}}",
                    pos
                ));
            }}
        }}

        result.push(Token {{
            kind: TokenKind::Eof,
            lexeme: String::new(),
            span: Span {{
                start: pos,
                end: pos,
            }},
        }});

        Ok(result)
    }}
}}
"#
    )
    .unwrap();

    Ok(())
}

fn emit_parser(
    out: &mut String,
    grammar: &GrammarIr,
) -> Result<(), Error> {
    writeln!(
        out,
        r#"#[derive(Debug)]
pub struct Parser {{
    tokens: Vec<Token>,
    pos: usize,
}}

impl Parser {{
    pub fn new(tokens: Vec<Token>) -> Self {{
        Self {{ tokens, pos: 0 }}
    }}

    fn peek(&self) -> &Token {{
        &self.tokens[self.pos]
    }}

    fn advance(&mut self) -> Token {{
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }}

    fn expect(&mut self, kind: TokenKind) -> Result<Token, String> {{
        if self.peek().kind == kind {{
            Ok(self.advance())
        }} else {{
            Err(format!(
                "expected {{:?}}, got {{:?}}",
                kind,
                self.peek().kind
            ))
        }}
    }}
"#
    )
    .unwrap();

    for rule in &grammar.rules {
        emit_rule(out, rule)?;
    }

    writeln!(out, "}}\n").unwrap();

    Ok(())
}

fn emit_rule(
    out: &mut String,
    rule: &RuleIr,
) -> Result<(), Error> {
    writeln!(
        out,
        "    pub fn parse_{}(&mut self) -> Result<(), String> {{",
        rule.name
    )
    .unwrap();

    if rule.alternatives.len() == 1 {
        emit_alternative(out, &rule.alternatives[0])?;
    } else {
        writeln!(
            out,
            "        match &self.peek().kind {{"
        )
        .unwrap();

        for alt in &rule.alternatives {
            if let Some(first) = alt.symbols.first() {
                writeln!(
                    out,
                    "            {} => {{",
                    match_pattern(first)
                )
                .unwrap();

                emit_alternative_body(out, alt)?;

                writeln!(out, "            }}").unwrap();
            }
        }

        writeln!(
            out,
            "            _ => return Err(format!(\"unexpected token: {{:?}}\", self.peek().kind)),"
        )
        .unwrap();

        writeln!(out, "        }}").unwrap();
    }

    writeln!(out, "        Ok(())").unwrap();
    writeln!(out, "    }}\n").unwrap();

    Ok(())
}

fn emit_alternative(
    out: &mut String,
    alt: &AlternativeIr,
) -> Result<(), Error> {
    emit_alternative_body(out, alt)
}

fn emit_alternative_body(
    out: &mut String,
    alt: &AlternativeIr,
) -> Result<(), Error> {
    for symbol in &alt.symbols {
        match symbol {
            SymbolIr::Terminal(value) => {
                writeln!(
                    out,
                    "                self.expect(TokenKind::{})?;",
                    literal_variant(value)
                )
                .unwrap();
            }

            SymbolIr::Token(name) => {
                writeln!(
                    out,
                    "                self.expect(TokenKind::{})?;",
                    token_variant(name)
                )
                .unwrap();
            }

            SymbolIr::Rule(name) => {
                writeln!(
                    out,
                    "                self.parse_{}()?;",
                    name
                )
                .unwrap();
            }
        }
    }

    Ok(())
}

fn match_pattern(symbol: &SymbolIr) -> String {
    match symbol {
        SymbolIr::Terminal(value) => {
            format!("TokenKind::{}", literal_variant(value))
        }

        SymbolIr::Token(name) => {
            format!("TokenKind::{}", token_variant(name))
        }

        SymbolIr::Rule(name) => {
            // This is simplistic. A production backend should use
            // FIRST sets here.
            format!("_ /* FIRST({}) */", name)
        }
    }
}

fn token_variant(name: &str) -> String {
    name.to_string()
}

fn literal_variant(value: &str) -> String {
    let mut result = String::from("Lit_");

    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c);
        } else {
            result.push_str(&format!("U{:04X}", c as u32));
        }
    }

    result
}
```

That backend is deliberately simple. The **IR and analysis aren't
Rust-specific**, which is the important part.

---

# 14\. Example grammar

For example:

```
lexer {
    WS         = /[ \t\r\n]+/ -> skip ;
    NUMBER     = /[0-9]+/ ;
    IDENTIFIER = /[a-zA-Z_][a-zA-Z0-9_]*/ ;
}

parser {
    program = { statement } ;

    statement =
        "let" IDENTIFIER "=" expression ";"
    ;

    expression =
        term { ("+" | "-") term }
    ;

    term =
        factor { ("*" | "/") factor }
    ;

    factor =
        NUMBER
      | IDENTIFIER
      | "(" expression ")"
    ;
}
```

Then:

```rust
use ebnfgen::{
    grammar::parser::parse_grammar,
    backend::rust::RustBackend,
    generate,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let grammar = std::fs::read_to_string("language.ebnf")?;
    let grammar = parse_grammar(&grammar)?;
    let backend = RustBackend::new();
    let generated = generate(&grammar, &backend)?;

    std::fs::write("generated_parser.rs", generated)?;

    Ok(())
}
```

---

## The next change I'd make: improve the IR

The current IR is enough to establish the architecture, but for a serious parser
generator I'd make the intermediate representation richer:

```
pub struct ParserIr {
    pub start: RuleId,
    pub rules: Vec<ParserRule>,
    pub terminals: Vec<Terminal>,
}

pub struct ParserRule {
    pub id: RuleId,
    pub name: String,
    pub alternatives: Vec<Alternative>,
}

pub struct Alternative {
    pub symbols: Vec<Symbol>,
    pub action: Option<Action>,
}

pub enum Symbol {
    Terminal(TerminalId),
    NonTerminal(RuleId),
}

pub enum Terminal {
    Literal(String),
    Token(TokenId),
}
```

And then put the parser decisions explicitly into the IR:

```
pub enum ParseOp {
    Consume(TerminalId),
    Call(RuleId),

    Choice {
        branches: Vec<ParseBranch>,
    },

    Loop {
        body: Vec<ParseOp>,
    },

    Optional {
        body: Vec<ParseOp>,
    },

    Return,
}
```

That gives us a much cleaner architecture:

```
EBNF
 │
 ▼
AST
 │
 ▼
BNF
 │
 ▼
Grammar analysis
 │
 │ FIRST/FOLLOW
 ▼
Parser IR
 │
 ├─────────────┐
 ▼             ▼
Rust          C++
backend       backend
 │             │
 ▼             ▼
.rs           .cpp
```

It also means the backend doesn't need to understand grammar theory at all.
**All parsing decisions are made before code generation.**

### One important issue with the current example

The Rust backend's `match_pattern()` is intentionally incomplete for
nonterminals. A production like:

```
factor = NUMBER | IDENTIFIER | "(" expression ")" ;
```

needs to use the calculated FIRST sets to generate something like:

```
match self.peek().kind {
    TokenKind::NUMBER => self.parse_number_factor(),
    TokenKind::IDENTIFIER => self.parse_identifier_factor(),
    TokenKind::Lit_U0028 => self.parse_parenthesized_factor(),
    _ => return Err(...)
}
```

For a real implementation, I'd therefore make **FIRST/FOLLOW-driven parser IR
construction** the next layer. That also lets us produce substantially better
diagnostics:

```
error: grammar is not LL(1)

  rule: expression

  alternatives 1 and 2 both accept:
      IDENTIFIER

  FIRST(alt 1) = { IDENTIFIER, NUMBER }
  FIRST(alt 2) = { IDENTIFIER }

  suggestion:
      left-factor the grammar or use a different parser strategy
```

And we can eventually support multiple parser strategies behind the same
target-independent frontend:

```
        Grammar
          │
    ┌─────┴─────┐
    ▼           ▼
LL(1)         LR/LALR
    │           │
    └─────┬─────┘
          ▼
      Parser IR
          │
┌─────────┼──────────┐
▼         ▼          ▼
Rust      C++        TypeScript
```

So the **right long-term design is not "Rust parser generator"**; it's a Rust
implementation of a compiler frontend whose output is a language-neutral
parser/lexer IR. The Rust/C++/JS/etc. pieces are just backends.
