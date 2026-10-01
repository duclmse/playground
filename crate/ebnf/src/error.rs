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
    Ll1Conflict { rule: String },

    #[error("left recursion in rule `{0}`")]
    LeftRecursion(String),

    #[error("backend error: {0}")]
    Backend(String),
}
