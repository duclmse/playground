// Linear byte scanner for Lua lexical syntax plus sol's typed extensions.
// Strings are bytes, not UTF-8 text; line numbers normalize CR/LF pairs.

use crate::diagnostic::{Diagnostic, SourceSpan};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Function,
    End,
    Local,
    If,
    Then,
    Else,
    ElseIf,
    While,
    Do,
    For,
    Return,
    True,
    False,
    Not,
    And,
    Or,
    Repeat,
    Until,
    Break,
    Nil,
    Goto,
    In,
    Global,
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    StringLit(Vec<u8>),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Arrow,
    Dot,
    Hash,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    NotEq,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Semi,
    FloorDiv,
    Caret,
    Amp,
    Pipe,
    Tilde,
    Shl,
    Shr,
    Concat,
    Vararg,
    Label,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spanned {
    pub token: Token,
    /// Exact source bytes for the token. This preserves contextual-keyword
    /// spelling and literal syntax for diagnostics, formatters, and editors.
    pub lexeme: Vec<u8>,
    /// Kept as a convenient compatibility accessor while consumers migrate to
    /// `span`, which also records byte offsets and columns.
    pub line: u32,
    pub span: SourceSpan,
}

pub fn lex(source: &str) -> Result<Vec<Spanned>, String> {
    lex_bytes(source.as_bytes())
}

/// Lex Lua source as bytes. Lua source and string literals are byte-oriented;
/// callers that load `.lua` files must use this entry point rather than first
/// decoding the entire file as UTF-8.
pub fn lex_bytes(source: &[u8]) -> Result<Vec<Spanned>, String> {
    Scanner {
        bytes: source,
        pos: 0,
        line: 1,
        line_start: 0,
    }
    .scan()
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    line_start: usize,
}

impl Scanner<'_> {
    fn error(&self, msg: &str) -> String {
        Diagnostic::error(self.span_at(self.pos), msg)
            .with_code("ELEX001")
            .to_string()
    }

    fn span_at(&self, pos: usize) -> SourceSpan {
        SourceSpan::new(
            pos,
            pos,
            self.line,
            (pos.saturating_sub(self.line_start) + 1) as u32,
        )
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn newline(&mut self) {
        let first = self.bytes[self.pos];
        self.pos += 1;
        if self
            .peek()
            .is_some_and(|b| (b == b'\r' || b == b'\n') && b != first)
        {
            self.pos += 1;
        }
        self.line += 1;
        self.line_start = self.pos;
    }

    fn delimiter(&self, at: usize, bracket: u8) -> Option<usize> {
        if self.bytes.get(at) != Some(&bracket) {
            return None;
        }
        let mut end = at + 1;
        while self.bytes.get(end) == Some(&b'=') {
            end += 1;
        }
        (self.bytes.get(end) == Some(&bracket)).then_some(end - at - 1)
    }

    fn long_string(&mut self, level: usize) -> Result<Vec<u8>, String> {
        self.pos += level + 2;
        if matches!(self.peek(), Some(b'\r' | b'\n')) {
            self.newline();
        }
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if b == b']' && self.delimiter(self.pos, b']') == Some(level) {
                self.pos += level + 2;
                return Ok(out);
            }
            if b == b'\r' || b == b'\n' {
                self.newline();
                out.push(b'\n');
            } else {
                out.push(b);
                self.pos += 1;
            }
        }
        Err(self.error("unfinished long string/comment"))
    }

    fn quoted(&mut self, quote: u8) -> Result<Vec<u8>, String> {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == quote {
                return Ok(out);
            }
            if b == b'\r' || b == b'\n' {
                return Err(self.error("unfinished string"));
            }
            if b != b'\\' {
                out.push(b);
                continue;
            }
            let e = self.peek().ok_or_else(|| self.error("unfinished escape"))?;
            self.pos += 1;
            match e {
                b'a' => out.push(7),
                b'b' => out.push(8),
                b'f' => out.push(12),
                b'n' => out.push(10),
                b'r' => out.push(13),
                b't' => out.push(9),
                b'v' => out.push(11),
                b'\\' | b'\'' | b'"' => out.push(e),
                b'\n' | b'\r' => {
                    self.pos -= 1;
                    self.newline();
                    out.push(b'\n');
                }
                b'z' => {
                    // Same vertical-tab gap as the top-level whitespace skip
                    // above: Rust's `is_ascii_whitespace` excludes 0x0B, but
                    // Lua's `\z` escape skips it along with every other space
                    // character (llex.c's `lisspace`).
                    while let Some(c) = self
                        .peek()
                        .filter(|c| c.is_ascii_whitespace() || *c == 0x0b)
                    {
                        if c == b'\n' || c == b'\r' {
                            self.newline();
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                b'x' => {
                    let mut n = 0;
                    for _ in 0..2 {
                        let d = self
                            .peek()
                            .and_then(|c| (c as char).to_digit(16))
                            .ok_or_else(|| self.error("expected two hex digits"))?;
                        self.pos += 1;
                        n = n * 16 + d;
                    }
                    out.push(n as u8);
                }
                b'0'..=b'9' => {
                    let mut n = (e - b'0') as u16;
                    for _ in 0..2 {
                        if let Some(c) = self.peek().filter(|c| c.is_ascii_digit()) {
                            n = n * 10 + (c - b'0') as u16;
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    if n > 255 {
                        return Err(self.error("decimal escape exceeds 255"));
                    }
                    out.push(n as u8);
                }
                b'u' => {
                    if self.peek() != Some(b'{') {
                        return Err(self.error("expected '{' after \\u"));
                    }
                    self.pos += 1;
                    let start = self.pos;
                    let mut n = 0u32;
                    while let Some(d) = self.peek().and_then(|c| (c as char).to_digit(16)) {
                        n = n
                            .checked_mul(16)
                            .and_then(|n| n.checked_add(d))
                            .ok_or_else(|| self.error("Unicode escape too large"))?;
                        self.pos += 1;
                    }
                    if self.pos == start || self.peek() != Some(b'}') || n > 0x7fffffff {
                        return Err(self.error("invalid Unicode escape"));
                    }
                    self.pos += 1;
                    // Lua permits the extended UTF-8 range, including surrogates.
                    if n < 128 {
                        out.push(n as u8);
                    } else {
                        let count = if n < 0x800 {
                            2
                        } else if n < 0x10000 {
                            3
                        } else if n < 0x200000 {
                            4
                        } else if n < 0x4000000 {
                            5
                        } else {
                            6
                        };
                        let mut encoded = vec![0; count];
                        for i in (1..count).rev() {
                            encoded[i] = 0x80 | (n as u8 & 63);
                            n >>= 6;
                        }
                        encoded[0] = (!0u8 << (8 - count)) | n as u8;
                        out.extend(encoded);
                    }
                }
                _ => return Err(self.error("invalid escape sequence")),
            }
        }
        Err(self.error("unfinished string"))
    }

    fn number(&mut self) -> Result<Token, String> {
        let start = self.pos;
        let hex = self.bytes.get(start) == Some(&b'0')
            && matches!(self.bytes.get(start + 1), Some(b'x' | b'X'));
        if hex {
            self.pos += 2;
        }
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'.' {
                self.pos += 1;
                if ((!hex && (b == b'e' || b == b'E')) || (hex && (b == b'p' || b == b'P')))
                    && matches!(self.peek(), Some(b'+' | b'-'))
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
        if hex {
            let digits = &text[2..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                let n = digits.bytes().fold(0u64, |n, b| {
                    n.wrapping_mul(16)
                        .wrapping_add((b as char).to_digit(16).unwrap() as u64)
                });
                return Ok(Token::IntLit(n as i64));
            }
            let mut split = digits.split(['p', 'P']);
            let mantissa = split.next().unwrap();
            let exp = match split.next() {
                Some(e) => e
                    .parse::<i32>()
                    .map_err(|_| self.error("malformed numeral"))?,
                None => 0,
            };
            if split.next().is_some() {
                return Err(self.error("malformed numeral"));
            }
            // Mirrors real Lua's `lua_strx2number` (`lobject.c`): naively
            // folding every mantissa hex digit into `n` (as a plain
            // `n = n * 16.0 + d` / fractional-scale accumulation) overflows
            // `n` to infinity long before the `p`-exponent ever gets
            // applied, once the corpus's very-long-numeral cases pile on
            // hundreds or thousands of hex digits
            // (`tonumber('0xe03' .. string.rep('0', 1000) .. 'p-4000')`).
            // Real Lua instead only ever accumulates the first `MAXSIGDIG`
            // *significant* digits (leading zeros before the first nonzero
            // digit don't count) into `n`, and folds every digit beyond
            // that - plus every fractional digit - into a separate exponent
            // correction instead, so `n` never grows past a bounded
            // magnitude regardless of the source's length.
            const MAX_SIGNIFICANT_DIGITS: u32 = 30;
            let mut n = 0.0;
            let mut hasdot = false;
            let mut significant_digits: u32 = 0;
            let mut leading_zero_digits: u32 = 0;
            let mut extra_exponent: i64 = 0;
            for b in mantissa.bytes() {
                if b == b'.' {
                    if hasdot {
                        return Err(self.error("malformed numeral"));
                    }
                    hasdot = true;
                    continue;
                }
                let d = (b as char)
                    .to_digit(16)
                    .ok_or_else(|| self.error("malformed numeral"))?;
                if significant_digits == 0 && d == 0 {
                    leading_zero_digits += 1;
                } else {
                    significant_digits += 1;
                    if significant_digits <= MAX_SIGNIFICANT_DIGITS {
                        n = n * 16.0 + d as f64;
                    } else {
                        extra_exponent += 1;
                    }
                }
                if hasdot {
                    extra_exponent -= 1;
                }
            }
            if significant_digits + leading_zero_digits == 0 {
                return Err(self.error("malformed numeral"));
            }
            let total_exponent =
                (extra_exponent * 4 + exp as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            Ok(Token::FloatLit(n * 2f64.powi(total_exponent)))
        } else if let Ok(n) = text.parse::<i64>() {
            Ok(Token::IntLit(n))
        } else {
            text.parse::<f64>()
                .map(Token::FloatLit)
                .map_err(|_| self.error("malformed numeral"))
        }
    }

    fn scan(mut self) -> Result<Vec<Spanned>, String> {
        let mut out = Vec::new();
        if self.bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            self.pos = 3;
        }
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\r' | b'\n')) {
                self.pos += 1;
            }
        }
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b'\r' {
                self.newline();
                continue;
            }
            // Rust's `is_ascii_whitespace` deliberately excludes vertical tab
            // (0x0B), but Lua's lexer treats it as a space character alongside
            // ' ', '\t', and '\f' (llex.c's `case ' ': case '\f': case '\t':
            // case '\v':`), so it must be recognized here too.
            if b.is_ascii_whitespace() || b == 0x0b {
                self.pos += 1;
                continue;
            }
            let start = self.pos;
            let line = self.line;
            let column = (start - self.line_start + 1) as u32;
            if self.bytes[self.pos..].starts_with(b"--") {
                self.pos += 2;
                if let Some(level) = self.delimiter(self.pos, b'[') {
                    self.long_string(level)?;
                } else {
                    while !matches!(self.peek(), None | Some(b'\r' | b'\n')) {
                        self.pos += 1;
                    }
                }
                continue;
            }
            let token = if b.is_ascii_alphabetic() || b == b'_' {
                let start = self.pos;
                self.pos += 1;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    self.pos += 1;
                }
                let name = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
                match name {
                    "function" => Token::Function,
                    "end" => Token::End,
                    "local" => Token::Local,
                    "if" => Token::If,
                    "then" => Token::Then,
                    "else" => Token::Else,
                    "elseif" => Token::ElseIf,
                    "while" => Token::While,
                    "do" => Token::Do,
                    "for" => Token::For,
                    "return" => Token::Return,
                    "true" => Token::True,
                    "false" => Token::False,
                    "not" => Token::Not,
                    "and" => Token::And,
                    "or" => Token::Or,
                    "repeat" => Token::Repeat,
                    "until" => Token::Until,
                    "break" => Token::Break,
                    "nil" => Token::Nil,
                    "goto" => Token::Goto,
                    "in" => Token::In,
                    "global" => Token::Global,
                    _ => Token::Ident(name.into()),
                }
            } else if b.is_ascii_digit()
                || (b == b'.'
                    && self
                        .bytes
                        .get(self.pos + 1)
                        .is_some_and(|c| c.is_ascii_digit()))
            {
                self.number()?
            } else if b == b'\'' || b == b'"' {
                Token::StringLit(self.quoted(b)?)
            } else if let Some(level) = self.delimiter(self.pos, b'[') {
                Token::StringLit(self.long_string(level)?)
            } else {
                let rest = &self.bytes[self.pos..];
                let pairs: [(&[u8], Token); 11] = [
                    (b"...", Token::Vararg),
                    (b"..", Token::Concat),
                    (b"->", Token::Arrow),
                    (b"//", Token::FloorDiv),
                    (b"<<", Token::Shl),
                    (b">>", Token::Shr),
                    (b"<=", Token::Le),
                    (b">=", Token::Ge),
                    (b"==", Token::EqEq),
                    (b"~=", Token::NotEq),
                    (b"::", Token::Label),
                ];
                if let Some((text, token)) = pairs.into_iter().find(|(s, _)| rest.starts_with(s)) {
                    self.pos += text.len();
                    token
                } else {
                    self.pos += 1;
                    match b {
                        b'(' => Token::LParen,
                        b')' => Token::RParen,
                        b'[' => Token::LBracket,
                        b']' => Token::RBracket,
                        b'{' => Token::LBrace,
                        b'}' => Token::RBrace,
                        b',' => Token::Comma,
                        b':' => Token::Colon,
                        b'.' => Token::Dot,
                        b'#' => Token::Hash,
                        b'<' => Token::Lt,
                        b'>' => Token::Gt,
                        b'=' => Token::Eq,
                        b'+' => Token::Plus,
                        b'-' => Token::Minus,
                        b'*' => Token::Star,
                        b'/' => Token::Slash,
                        b'%' => Token::Percent,
                        b';' => Token::Semi,
                        b'^' => Token::Caret,
                        b'&' => Token::Amp,
                        b'|' => Token::Pipe,
                        b'~' => Token::Tilde,
                        _ => {
                            return Err(Diagnostic::error(
                                SourceSpan::new(start, self.pos, line, column),
                                "unexpected character",
                            )
                            .with_code("ELEX001")
                            .to_string())
                        }
                    }
                }
            };
            out.push(Spanned {
                token,
                lexeme: self.bytes[start..self.pos].to_vec(),
                line,
                span: SourceSpan::new(start, self.pos, line, column),
            });
        }
        Ok(out)
    }
}
