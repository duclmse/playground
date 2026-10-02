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
// U12 item 6: excluded from any `wasm32` target build (a target-arch gate,
// not a new Cargo feature - native builds under every existing feature
// combination, hence every pre-existing test baseline, are completely
// unaffected). Root cause this documents: `lua_runtime::c_api` exposes a
// real embeddable Lua C API (`luaL_newstate`, `lua_pushfstring`, etc.) as
// `#[no_mangle] pub extern "C" fn`s - which the Rust compiler keeps in any
// build's object code regardless of whether anything else in the crate
// calls them, since `#[no_mangle]` alone makes a symbol externally
// reachable by definition. Those functions in turn reference raw,
// body-less `extern "C" { fn malloc/free/realloc }` declarations
// (`lua_runtime/c_api/auxlib.rs::default_alloc`) meant to resolve against a
// real host libc, plus `sol_c_api_shim_anchor` (`lua_runtime/c_api.rs`,
// satisfied for native builds only by `build.rs`'s host-only C compile of
// `lua_runtime/c_api_shim.c`). `wasm32-unknown-unknown` has no implicit
// libc and no native shim: compiling this module in for that target leaves
// `malloc`/`free`/`realloc`/`sol_c_api_shim_anchor` as unresolved externs,
// which surface as required (and, in a plain browser `<script type="module">`
// context, unsatisfiable - there is no real `env` module to import from)
// `env.*` imports on the compiled `.wasm`. This was *not* caught by
// `cargo check --target wasm32-unknown-unknown` (type-checking never
// performs the final link step that would reveal an unresolved-symbol
// problem like this) - it was only found by actually building
// `packages/sol-runtime/pkg` via `scripts/build-sol-wasm.sh` and inspecting
// the resulting `sol.js`/`sol_bg.wasm` directly, which is why this comment
// calls it out explicitly rather than trusting a clean `cargo check` alone.
// Since `lua_runtime` (the dynamic `.lua` interpreter this gates) is not
// reachable at all from `wasm_api.rs`'s own entry points regardless
// (confirmed in that file's own top doc comment: `.lua` source does not
// even typecheck through `compile`/`compile_bytes` without a `print`/stdlib
// it never constructs), excluding it from the wasm32 build loses nothing
// this item claims to support - see
// `docs/features/milestones/u12-wasm-playground.md`'s Work item 6 section
// for the full writeup.
#[cfg(not(target_arch = "wasm32"))]
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
