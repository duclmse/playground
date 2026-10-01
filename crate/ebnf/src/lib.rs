pub mod analysis;
pub mod backend;
pub mod error;
pub mod generate;
pub mod grammar;
pub mod ir;
pub mod lower;

pub use error::Error;
pub use generate::generate;
pub use grammar::ast::Grammar;
pub use grammar::parser::parse_grammar;
pub use lower::lower;
