use ebnfgen::{backend::rust::RustBackend, generate, grammar::parser::parse_grammar};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let grammar = std::fs::read_to_string("language.ebnf")?;
    let grammar = parse_grammar(&grammar)?;
    let backend = RustBackend::new();
    let generated = generate(&grammar, &backend)?;

    std::fs::write("generated_parser.rs", generated)?;

    Ok(())
}
