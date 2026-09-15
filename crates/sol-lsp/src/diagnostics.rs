//! Turns a document into diagnostics by running the same lex -> parse ->
//! typeck pipeline `sol run` uses (`main.rs`'s `run()`), minus execution.
//! For `.lua` files, a typeck failure tagged with
//! `sol::typeck::requires_dynamic_runtime` is not an error - it's the normal
//! case for real Lua source (globals, `pairs`/`next`, dynamic function
//! values, ...), which `sol run` itself falls through to the dynamic
//! interpreter for. Only genuine lex/parse errors and non-dynamic typeck
//! errors become diagnostics.

use sol::{
    binder::BindingIndex,
    parser::{LanguageConfig, ParsedProgram, SourceMode},
};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity};

use crate::text::line_range;

/// The parsed AST (when parsing succeeded, regardless of typeck outcome) and
/// any diagnostics to publish.
pub struct Analysis {
    pub syntax: Option<ParsedProgram>,
    pub bindings: Option<BindingIndex>,
    pub diagnostics: Vec<Diagnostic>,
}

fn parse_line_number(error: &str) -> u32 {
    error
        .strip_prefix("line ")
        .and_then(|rest| {
            let end = rest.find([',', ':']).unwrap_or(rest.len());
            rest[..end].parse::<u32>().ok()
        })
        .unwrap_or(1)
}

fn diagnostic_from_error(text: &str, error: &str) -> Diagnostic {
    let line = parse_line_number(error);
    Diagnostic {
        range: line_range(text, line),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("sol".to_string()),
        message: error.to_string(),
        ..Diagnostic::default()
    }
}

pub fn analyze(text: &str, mode: SourceMode) -> Analysis {
    let source = text.as_bytes();

    let tokens = match sol::lexer::lex_bytes(source) {
        Ok(tokens) => tokens,
        Err(error) => {
            return Analysis {
                syntax: None,
                bindings: None,
                diagnostics: vec![diagnostic_from_error(text, &error)],
            }
        }
    };

    let syntax = match sol::parser::parse_document_with_config(tokens, LanguageConfig::from(mode)) {
        Ok(syntax) => syntax,
        Err(error) => {
            return Analysis {
                syntax: None,
                bindings: None,
                diagnostics: vec![diagnostic_from_error(text, &error)],
            }
        }
    };

    let bindings = sol::binder::bind(&syntax.ast);
    let mut diagnostics: Vec<_> = bindings
        .errors
        .iter()
        .map(|error| diagnostic_from_error(text, &format!("line {}: {}", error.line, error.message)))
        .collect();

    match sol::compile_program_with_config(syntax.ast.clone(), syntax.config) {
        Ok(_) => {}
        Err(error) if mode == SourceMode::Lua && sol::typeck::requires_dynamic_runtime(&error) => {
            // The shared parser and binder still provide useful analysis for
            // code that will execute in the dynamic tier.
        }
        Err(error) => diagnostics.push(diagnostic_from_error(text, &error)),
    }

    Analysis {
        syntax: Some(syntax),
        bindings: Some(bindings),
        diagnostics,
    }
}
