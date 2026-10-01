use crate::{analysis::Analysis, error::Error, ir::GrammarIr};

pub mod rust;

pub trait Backend {
    type Output;

    fn generate(&self, grammar: &GrammarIr, analysis: &Analysis) -> Result<Self::Output, Error>;
}
