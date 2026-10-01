use crate::{
    analysis::analyze, backend::Backend, error::Error, grammar::ast::Grammar, lower::lower,
};

pub fn generate<B: Backend>(grammar: &Grammar, backend: &B) -> Result<B::Output, Error> {
    let ir = lower(grammar)?;
    let analysis = analyze(&ir)?;

    backend.generate(&ir, &analysis)
}
