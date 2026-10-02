// sol: a typed, Lua-like language compiled via Cranelift. See
// docs/sol.md. This crate builds both as a library (used by main.rs
// for `run`/`build`, and as a `staticlib` the AOT linker links generated
// object files against - see aot.rs/runtime.rs's `#[no_mangle]` functions)
// and its own `sol` CLI binary (main.rs).

pub mod aliases;
#[cfg(feature = "jit")]
pub mod aot;
pub mod ast;
pub mod bccompile;
pub mod binder;
pub mod bytecode;
pub mod closures;
#[cfg(feature = "jit")]
pub mod codegen;
pub mod debug;
pub mod debugger;
pub mod diagnostic;
pub mod dynamic;
pub mod escape;
pub mod gc;
pub mod interp;
#[cfg(feature = "jit")]
pub mod jit;
pub mod lexer;
pub mod lua_bridge;
pub mod lua_bytecode;
pub mod lua_pack;
pub mod lua_pattern;
pub mod lua_runtime;
pub mod modules;
pub mod numeric;
pub mod optimize;
pub mod parser;
pub mod profile;
pub mod runtime;
pub mod sol_ir;
pub mod strings;
#[cfg(feature = "jit")]
pub mod tier;
pub mod tier0;
pub mod typeck;
pub mod types;
pub mod value;
pub mod verify;
#[cfg(feature = "wasm")]
pub mod wasm_api;

use types::{TProgram, Type};

fn with_error_code(error: String, code: &str) -> String {
    if error.ends_with(']') {
        error
    } else {
        format!("{error} [{code}]")
    }
}

/// Shared front end: source -> type-checked, optimized `TProgram`, plus
/// `main`'s return type (validated as printable). Used by both `run`
/// (tiered JIT) and `build` (AOT).
pub fn compile(source: &str) -> Result<(TProgram, Type), String> {
    compile_bytes(source.as_bytes(), parser::SourceMode::Sol)
}

/// Compatibility entry point for callers that select defaults by extension.
/// New integrations should call [`compile_bytes_with_config`]. Source remains
/// byte-oriented in every language profile.
pub fn compile_bytes(source: &[u8], mode: parser::SourceMode) -> Result<(TProgram, Type), String> {
    compile_bytes_with_config(source, mode.into())
}

pub fn compile_bytes_with_config(
    source: &[u8],
    config: parser::LanguageConfig,
) -> Result<(TProgram, Type), String> {
    let tokens = lexer::lex_bytes(source)?;
    let program = parser::parse_with_config(tokens, config)?;
    compile_program_with_config(program, config)
}

pub fn compile_program(program: ast::Program) -> Result<(TProgram, Type), String> {
    compile_program_with_mode(program, parser::SourceMode::Sol)
}

pub fn compile_program_with_mode(
    program: ast::Program,
    mode: parser::SourceMode,
) -> Result<(TProgram, Type), String> {
    compile_program_with_config(program, mode.into())
}

pub fn compile_program_with_config(
    mut program: ast::Program,
    config: parser::LanguageConfig,
) -> Result<(TProgram, Type), String> {
    if config.sol_extensions {
        aliases::expand(&mut program)?;
        closures::lower(&mut program)?;
    }
    // Lua compatibility is a language-profile property, not a type-policy
    // property: `--type-policy strict file.lua` must retain Lua's dynamic
    // fallback while enforcing any explicit contracts it contains.
    let mut tprogram = typeck::check(&program, !config.sol_extensions)
        .map_err(|error| with_error_code(error, "ETYPE001"))?;
    verify::verify(&tprogram).map_err(|error| with_error_code(error, "EIR001"))?;
    optimize::optimize(&mut tprogram);
    verify::verify(&tprogram).map_err(|error| with_error_code(error, "EIR001"))?;
    escape::scalar_replace(&mut tprogram);
    verify::verify(&tprogram).map_err(|error| with_error_code(error, "EIR001"))?;

    let main_func = tprogram
        .functions
        .iter()
        .find(|f| f.name == "main")
        .ok_or_else(|| {
            "no 'main' function found (the entry point must be a niladic function named 'main')"
                .to_string()
        })?;
    if !main_func.params.is_empty() {
        return Err("'main' must take no parameters".to_string());
    }
    let return_type = main_func.return_type.clone();
    match return_type {
        Type::Array(_) => {
            Err("'main' returning an Array isn't supported by the CLI printer".to_string())
        }
        Type::Map(_, _) => {
            Err("'main' returning a Map isn't supported by the CLI printer".to_string())
        }
        Type::Struct(_) => {
            Err("'main' returning a struct isn't supported by the CLI printer".to_string())
        }
        Type::Function { .. } => {
            Err("'main' returning a function isn't supported by the CLI printer".to_string())
        }
        Type::I64 | Type::F64 | Type::Bool | Type::Nil | Type::String | Type::Any => {
            Ok((tprogram, return_type))
        }
    }
}
