use std::collections::HashMap;

use crate::{error::Error, grammar::ast::*, ir::*};

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

    fn lower_expr(&mut self, expr: &Expr) -> Result<Vec<AlternativeIr>, Error> {
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
                            symbols: vec![SymbolIr::Rule(name), SymbolIr::Rule(helper.clone())],
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

fn validate_symbols(rules: &[RuleIr], tokens: &[TokenIr]) -> Result<(), Error> {
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
