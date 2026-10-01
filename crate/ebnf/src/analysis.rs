use std::collections::{HashMap, HashSet};

use crate::{error::Error, ir::*};

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
                additions.extend(first_of_alternative(alt, &analysis));
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

fn alternative_nullable(alt: &AlternativeIr, nullable: &HashMap<String, bool>) -> bool {
    alt.symbols.iter().all(|symbol| match symbol {
        SymbolIr::Rule(name) => nullable.get(name).copied().unwrap_or(false),

        SymbolIr::Terminal(_) | SymbolIr::Token(_) => false,
    })
}

fn first_of_alternative(alt: &AlternativeIr, analysis: &Analysis) -> Vec<FirstSymbol> {
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
        SymbolIr::Rule(name) => analysis.nullable.get(name).copied().unwrap_or(false),
        _ => false,
    }) {
        result.push(FirstSymbol::Epsilon);
    }

    result
}

fn detect_left_recursion(grammar: &GrammarIr, analysis: &Analysis) -> Result<(), Error> {
    for rule in &grammar.rules {
        let mut stack = vec![rule.name.clone()];
        let mut visited = HashSet::new();

        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }

            let r = grammar.rules.iter().find(|r| r.name == current).unwrap();

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

fn check_ll1(grammar: &GrammarIr, analysis: &Analysis) -> Result<(), Error> {
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
