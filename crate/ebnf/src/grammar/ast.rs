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
