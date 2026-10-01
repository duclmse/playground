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
