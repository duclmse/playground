// `sol run <file.lua>` or `sol run <file.sol>`: lex -> parse -> type-check -> tiered JIT -> call
// `main` -> print its return value. `sol build <file.sol> -o <out>`:
// same front end, ahead-of-time to a standalone executable (aot.rs).
// `sol debug <file.sol>`: same front end, tiered JIT, but with a
// call-boundary REPL debugger attached (debug.rs). See docs/sol.md
// for the language and overall architecture.

use sol::lua_runtime::{BridgeScalar, LuaValue, NativeBridge};
use sol::{
    diagnostic::{Diagnostic, SourceSpan},
    gc,
    parser::{LanguageConfig, TypePolicy},
    tier, types,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    env, fs,
    io::{self, Write},
    path::Path,
    process,
    rc::Rc,
};

const USAGE: &str = r#"usage: sol run [--diagnostic-format human|json] [--type-policy off|infer|strict] [--explain-types] [--dump-ir] [--dump-asm] [--jit-log] [--target-info] [--profile-out <file>] [--profile-in <file>] [--profile-time <file>] <file.lua|file.sol>
    sol build [--diagnostic-format human|json] [--type-policy off|infer|strict] [--explain-types] [--profile-in <file>] <file.lua|file.sol> -o <output>
    sol debug [--diagnostic-format human|json] [--type-policy off|infer|strict] [--explain-types] <file.lua|file.sol>"#;

fn set_diagnostic_format(value: Option<&String>) {
    match value.map(String::as_str) {
        Some("json") => env::set_var("SOL_DIAGNOSTIC_JSON", "1"),
        Some("human") => env::remove_var("SOL_DIAGNOSTIC_JSON"),
        _ => usage_error(),
    }
}

fn set_type_policy(value: Option<&String>) {
    match value.map(String::as_str) {
        Some("off" | "infer" | "strict") => {
            env::set_var("SOL_TYPE_POLICY", value.expect("matched Some"));
        }
        _ => usage_error(),
    }
}

fn validate_source_path(path: &str) -> Result<(), String> {
    match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("lua" | "sol" | "fl") => Ok(()),
        _ => Err(format!(
            "unsupported source extension for '{path}'; expected .lua or .sol (.fl remains supported for compatibility)"
        )),
    }
}

fn read_source(path: &str) -> Result<Vec<u8>, String> {
    match fs::read(path) {
        Ok(source) => Ok(source),
        Err(original) if Path::new(path).extension().and_then(|ext| ext.to_str()) == Some("fl") => {
            let replacement = Path::new(path).with_extension("sol");
            fs::read(&replacement).map_err(|_| format!("failed to read '{path}': {original}"))
        }
        Err(error) => Err(format!("failed to read '{path}': {error}")),
    }
}

/// Real Lua's own `@`-prefixed `debug.getinfo` chunk-name convention for a
/// file loaded from disk (see `lua_State`/`lua_load`'s `chunkname`), applied
/// to the CLI's own top-level compile so a running script's `source`/
/// `short_src` resolve the same way a `load`ed chunk's do.
fn cli_chunk_name(path: &str) -> Vec<u8> {
    format!("@{path}").into_bytes()
}

fn language_config(path: &str) -> LanguageConfig {
    let mut config = if Path::new(path).extension().and_then(|ext| ext.to_str()) == Some("lua") {
        LanguageConfig::LUA
    } else {
        LanguageConfig::SOL
    };
    if let Ok(policy) = env::var("SOL_TYPE_POLICY") {
        config.type_policy = match policy.as_str() {
            "off" => TypePolicy::Dynamic,
            "infer" => TypePolicy::Infer,
            "strict" => TypePolicy::Strict,
            _ => unreachable!("set_type_policy validates the environment value"),
        };
    }
    config
}

fn explain_types(program: &sol::ast::Program, policy: TypePolicy) {
    if env::var_os("SOL_EXPLAIN_TYPES").is_some() {
        eprint!(
            "{}",
            sol::typeck::inference::analyze(program, policy).render()
        );
    }
}

fn explain_source_if_requested(
    path: &str,
    source: &[u8],
    language: LanguageConfig,
) -> Result<(), String> {
    if env::var_os("SOL_EXPLAIN_TYPES").is_none() {
        return Ok(());
    }
    let tokens =
        sol::lexer::lex_bytes(source).map_err(|error| render_source_error(path, source, error))?;
    let program = sol::parser::parse_with_config(tokens, language)
        .map_err(|error| render_source_error(path, source, error))?;
    explain_types(&program, language.type_policy);
    Ok(())
}

/// Chooses the specialized tier from semantic type surface, never from the
/// filename. Annotation-free Lua and Sol programs therefore enter the same
/// generic runtime; explicit contracts and typed-only declarations keep the
/// existing specialized/AOT path.
fn requires_specialized_execution(program: &sol::ast::Program) -> bool {
    !program.imports.is_empty()
        || !program.exports.is_empty()
        || !program.aliases.is_empty()
        || !program.structs.is_empty()
        || !program.externs.is_empty()
        || program.functions.iter().any(function_requires_types)
}

fn function_requires_types(function: &sol::ast::Function) -> bool {
    function.return_type.is_some()
        || function
            .params
            .iter()
            .any(|(_, ty)| !matches!(ty, sol::ast::TypeName::Any))
        || function.body.iter().any(statement_requires_types)
}

fn target_requires_types(target: &sol::ast::AssignTarget) -> bool {
    match target {
        sol::ast::AssignTarget::Name(_) => false,
        sol::ast::AssignTarget::Index(base, index) => {
            expression_requires_types(base) || expression_requires_types(index)
        }
        sol::ast::AssignTarget::Field(base, _) => expression_requires_types(base),
    }
}

fn statement_requires_types(statement: &sol::ast::Stmt) -> bool {
    use sol::ast::Stmt;
    match statement {
        Stmt::Global { values, .. } => values.iter().any(expression_requires_types),
        Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
            function_requires_types(function)
        }
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => false,
        Stmt::MultiLocal { names, values, .. } => {
            names.iter().any(|(_, ty, _, _)| ty.is_some())
                || values.iter().any(expression_requires_types)
        }
        Stmt::MultiAssign {
            targets, values, ..
        } => {
            targets.iter().any(target_requires_types)
                || values.iter().any(expression_requires_types)
        }
        Stmt::Block(body) => body.iter().any(statement_requires_types),
        Stmt::Repeat { body, cond, .. } => {
            body.iter().any(statement_requires_types) || expression_requires_types(cond)
        }
        Stmt::Expr(expression) => expression_requires_types(expression),
        Stmt::Local { ty, value, .. } => ty.is_some() || expression_requires_types(value),
        Stmt::Assign { target, value, .. } => {
            target_requires_types(target) || expression_requires_types(value)
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expression_requires_types(cond)
                || then_block.iter().any(statement_requires_types)
                || else_block
                    .as_ref()
                    .is_some_and(|body| body.iter().any(statement_requires_types))
        }
        Stmt::While { cond, body, .. } => {
            expression_requires_types(cond) || body.iter().any(statement_requires_types)
        }
        Stmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            expression_requires_types(start)
                || expression_requires_types(stop)
                || step.as_ref().is_some_and(expression_requires_types)
                || body.iter().any(statement_requires_types)
        }
        Stmt::GenericFor {
            iterators, body, ..
        } => {
            iterators.iter().any(expression_requires_types)
                || body.iter().any(statement_requires_types)
        }
        Stmt::Return { value, .. } => value.as_ref().is_some_and(expression_requires_types),
        Stmt::MultiReturn { values, .. } => values.iter().any(expression_requires_types),
    }
}

fn expression_requires_types(expression: &sol::ast::Expr) -> bool {
    use sol::ast::{ExprKind, TableField};
    match &expression.kind {
        ExprKind::StructLiteral(..) | ExprKind::TypeTest(..) | ExprKind::Cast(..) => true,
        ExprKind::Table(fields) => fields.iter().any(|field| match field {
            TableField::Value(value) | TableField::Named(_, value) => {
                expression_requires_types(value)
            }
            TableField::Key(key, value) => {
                expression_requires_types(key) || expression_requires_types(value)
            }
        }),
        ExprKind::Function(function) => function_requires_types(function),
        ExprKind::Unary(_, value)
        | ExprKind::Len(value)
        | ExprKind::Field(value, _)
        | ExprKind::Paren(value) => expression_requires_types(value),
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            expression_requires_types(left) || expression_requires_types(right)
        }
        ExprKind::Call(_, args) => args.iter().any(expression_requires_types),
        ExprKind::CallExpr(callee, args) | ExprKind::MethodCall(callee, _, args) => {
            expression_requires_types(callee) || args.iter().any(expression_requires_types)
        }
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => false,
    }
}

fn compile_source(path: &str, source: &[u8]) -> Result<(types::TProgram, types::Type), String> {
    let language = language_config(path);
    if language.sol_extensions {
        sol::modules::compile_project(path, source)
    } else {
        sol::compile_bytes_with_config(source, language)
    }
}

/// `sol build`/`sol debug` have no AOT-compiled or debuggable form of the
/// dynamic Lua interpreter, so unlike `run()` they cannot fall back to it.
/// Turn typeck's "requires dynamic runtime" error into an explicit,
/// actionable message instead of leaking its internal wording (which only
/// makes sense in the context of `run()`'s fallback check).
fn compile_typed_only(
    path: &str,
    source: &[u8],
    command: &str,
) -> Result<(types::TProgram, types::Type), String> {
    compile_source(path, source).map_err(|error| {
        if !language_config(path).sol_extensions && sol::typeck::requires_dynamic_runtime(&error) {
            format!(
                "'{path}' uses dynamic Lua features that `sol {command}` does not support; \
                 only `sol run` can execute them"
            )
        } else {
            render_source_error(path, source, error)
        }
    })
}

fn render_source_error(path: &str, source: &[u8], error: String) -> String {
    let Some(rest) = error.strip_prefix("line ") else {
        return error;
    };
    let line_end = rest.find([',', ':']).unwrap_or(rest.len());
    let Ok(line_number) = rest[..line_end].parse::<usize>() else {
        return error;
    };
    let column = rest
        .get(line_end..)
        .and_then(|tail| tail.strip_prefix(", column "))
        .and_then(|tail| tail.split(':').next())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    if env::var_os("SOL_DIAGNOSTIC_JSON").is_some() {
        let start = source
            .split(|byte| *byte == b'\n')
            .take(line_number.saturating_sub(1))
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + column.saturating_sub(1);
        let mut diagnostic = Diagnostic::error(
            SourceSpan::new(
                start,
                start.saturating_add(1),
                line_number as u32,
                column as u32,
            ),
            error.clone(),
        )
        .with_file(path);
        if let Some(code) = error
            .strip_suffix(']')
            .and_then(|value| value.rsplit_once('['))
            .map(|(_, code)| code)
        {
            diagnostic = diagnostic.with_code(code);
        }
        return diagnostic.to_json();
    }
    let source = String::from_utf8_lossy(source);
    let source_line = source
        .lines()
        .nth(line_number.saturating_sub(1))
        .unwrap_or("");
    format!(
        "{path}:{line_number}:{column}: {error}\n  {source_line}\n  {}^",
        " ".repeat(column.saturating_sub(1))
    )
}

/// `sol run`/`debug` dispatch onto a worker thread with a much larger stack
/// than the OS default (~8MiB). Sol's dynamic-Lua interpreter recurses
/// natively for each nested Lua call (unlike real Lua, which keeps its own
/// heap-allocated call stack), so ordinary, non-pathological recursive Lua
/// programs were hitting a native stack overflow - and aborting the whole
/// process - at roughly 1/2 to 1/3 of `LuaRuntime`'s documented
/// `max_call_depth` budget (1000), well before that budget's own graceful
/// `LuaError` ever had a chance to fire. A generous worker stack gives that
/// existing depth check enough native headroom to actually be the thing that
/// fires first. This does not raise how deep Lua recursion Sol can express
/// (still bounded by `max_call_depth`) - see `docs/features/lua-compatibility.md`
/// for that remaining, separately-tracked gap versus real Lua's effectively
/// unbounded (heap-stack-based) recursion depth.
const RUN_STACK_SIZE: usize = 256 * 1024 * 1024;

fn main() {
    let result = std::thread::Builder::new()
        .stack_size(RUN_STACK_SIZE)
        .spawn(dispatch)
        .expect("failed to spawn sol runtime thread")
        .join()
        .unwrap_or_else(|panic| {
            eprintln!("error: sol runtime thread panicked: {panic:?}");
            process::exit(1);
        });
    if let Err(e) = result {
        if env::var_os("SOL_DIAGNOSTIC_JSON").is_some() {
            if e.starts_with('{') {
                eprintln!("{e}");
            } else {
                eprintln!(
                    "{}",
                    Diagnostic::error(SourceSpan::new(0, 0, 0, 0), e)
                        .with_code("ECLI001")
                        .to_json()
                );
            }
        } else {
            eprintln!("error: {e}");
        }
        process::exit(1);
    }
}

fn dispatch() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("run") => run_cmd(&args[2..]),
        Some("build") => build_cmd(&args[2..]),
        Some("debug") => debug_cmd(&args[2..]),
        _ => {
            eprintln!("{USAGE}");
            process::exit(2);
        }
    }
}

fn usage_error() -> ! {
    eprintln!("{USAGE}");
    process::exit(2);
}

fn run_cmd(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut profile_out = None;
    let mut profile_in = None;
    let mut profile_time = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dump-ir" => env::set_var("SOL_DUMP_CLIF", "1"),
            "--dump-asm" => env::set_var("SOL_DUMP_ASM", "1"),
            "--jit-log" => env::set_var("SOL_JIT_LOG", "1"),
            "--target-info" => env::set_var("SOL_TARGET_INFO", "1"),
            "--profile-out" => {
                profile_out = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage_error()));
                i += 1;
            }
            "--profile-in" => {
                profile_in = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage_error()));
                i += 1;
            }
            "--profile-time" => {
                profile_time = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage_error()));
                i += 1;
            }
            "--diagnostic-format" => {
                set_diagnostic_format(args.get(i + 1));
                i += 1;
            }
            "--type-policy" => {
                set_type_policy(args.get(i + 1));
                i += 1;
            }
            "--explain-types" => env::set_var("SOL_EXPLAIN_TYPES", "1"),
            _ if args[i].starts_with("--") || path.is_some() => usage_error(),
            _ => path = Some(args[i].clone()),
        }
        i += 1;
    }
    let path = path.unwrap_or_else(|| usage_error());
    validate_source_path(&path)?;
    run(
        &path,
        profile_in.as_deref(),
        profile_out.as_deref(),
        profile_time.as_deref(),
    )
}

fn build_cmd(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut output = None;
    let mut profile_in = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--diagnostic-format" => {
                set_diagnostic_format(args.get(i + 1));
                i += 2;
            }
            "--type-policy" => {
                set_type_policy(args.get(i + 1));
                i += 2;
            }
            "--explain-types" => {
                env::set_var("SOL_EXPLAIN_TYPES", "1");
                i += 1;
            }
            "--profile-in" => {
                profile_in = Some(args.get(i + 1).cloned().unwrap_or_else(|| usage_error()));
                i += 2;
            }
            "-o" => {
                output = args.get(i + 1).cloned();
                i += 2;
            }
            _ if path.is_none() => {
                path = Some(args[i].clone());
                i += 1;
            }
            _ => usage_error(),
        }
    }
    let path = path.unwrap_or_else(|| usage_error());
    let output = output.unwrap_or_else(|| usage_error());
    validate_source_path(&path)?;
    let source = read_source(&path)?;
    explain_source_if_requested(&path, &source, language_config(&path))?;
    let (tprogram, return_type) = compile_typed_only(&path, &source, "build")?;
    // U11 item 5: a prior `sol run --profile-out`'s `promoted` list doubles
    // as AOT's "demonstrably hot" signal - see `aot::build`'s doc comment.
    let profile = profile_in.map(|p| sol::tier::Profile::load(&p)).transpose()?;
    sol::aot::build(tprogram, return_type, &output, profile.as_ref())
}

fn debug_cmd(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--diagnostic-format" {
            set_diagnostic_format(args.get(i + 1));
            i += 2;
        } else if args[i] == "--type-policy" {
            set_type_policy(args.get(i + 1));
            i += 2;
        } else if args[i] == "--explain-types" {
            env::set_var("SOL_EXPLAIN_TYPES", "1");
            i += 1;
        } else if path.is_none() {
            path = Some(args[i].clone());
            i += 1;
        } else {
            usage_error();
        }
    }
    let path = path.unwrap_or_else(|| usage_error());
    validate_source_path(&path)?;
    let source = read_source(&path)?;
    explain_source_if_requested(&path, &source, language_config(&path))?;
    let (tprogram, return_type) = compile_typed_only(&path, &source, "debug")?;

    let name_to_id = sol::bccompile::function_index(&tprogram)?;
    let mut id_to_name = vec![String::new(); name_to_id.len()];
    let mut id_to_line = vec![None; name_to_id.len()];
    let mut id_to_file = vec![Some(path.clone()); name_to_id.len()];
    for (name, &id) in &name_to_id {
        id_to_name[id as usize] = name.clone();
        id_to_line[id as usize] = tprogram
            .functions
            .iter()
            .find(|function| function.name == *name)
            .map(|function| function.source_line);
        if let Some(file) = tprogram
            .functions
            .iter()
            .find(|function| function.name == *name)
            .and_then(|function| function.source_file.clone())
        {
            id_to_file[id as usize] = Some(file);
        }
    }
    let hooks = sol::debug::DebugHooks::with_source_locations(id_to_name, id_to_file, id_to_line);
    let engine = tier::Engine::new(tprogram, None, hooks)?;

    gc::init_stack_base();
    let result = engine.call("main", &[]);
    print_result(return_type, result);
    Ok(())
}

fn print_result(return_type: types::Type, result: u64) {
    match return_type {
        types::Type::I64 => println!("{}", result as i64),
        types::Type::F64 => println!("{}", f64::from_bits(result)),
        types::Type::Bool => println!("{}", result != 0),
        types::Type::String => unsafe { sol::strings::sol_print_string(result as *const u8) },
        types::Type::Nil => println!("nil"),
        types::Type::Any => unsafe { sol::dynamic::sol_print_any(result as *const u64) },
        types::Type::Array(_)
        | types::Type::Map(_, _)
        | types::Type::Struct(_)
        | types::Type::Function { .. } => {
            unreachable!("rejected by sol::compile")
        }
    }
}

/// Only `types::Type::I64`/`F64`/`Bool` can cross `LuaValue`'s
/// `Rc<RefCell<_>>` world / Sol's GC arena boundary as raw `u64` bits (see
/// `lua_runtime.rs::BridgeScalar`) - anything else (strings, arrays, maps,
/// structs, function values, `any`) needs real marshaling code this bridge
/// doesn't have.
fn bridge_scalar(ty: &types::Type) -> Option<BridgeScalar> {
    match ty {
        types::Type::I64 => Some(BridgeScalar::I64),
        types::Type::F64 => Some(BridgeScalar::F64),
        types::Type::Bool => Some(BridgeScalar::Bool),
        _ => None,
    }
}

fn ast_bridge_scalar(ty: &sol::ast::TypeName) -> Option<BridgeScalar> {
    match ty {
        sol::ast::TypeName::I64 => Some(BridgeScalar::I64),
        sol::ast::TypeName::F64 => Some(BridgeScalar::F64),
        sol::ast::TypeName::Bool => Some(BridgeScalar::Bool),
        _ => None,
    }
}

enum PartitionOutcome {
    /// Already ran (via `lua_runtime`) and wrote output; `run()` should stop.
    Done,
    /// `main` itself type-checked natively - nothing dynamic can reach it,
    /// so it runs through the same tiered-JIT path as a fully-typed `.lua`
    /// file. Whatever's left in the dynamic set (if anything) has no caller
    /// reachable from `main` by construction (the demotion fixed point in
    /// `check_partitioned` would have pulled `main` into the dynamic set
    /// otherwise), so it's simply never compiled or run.
    RunNative(types::TProgram, types::Type),
}

/// Handles a `.lua` file where the whole-program typed compile failed with
/// `requires_dynamic_runtime`: splits it per-function instead of falling
/// back to the interpreter wholesale (M13 L4's per-function typed/dynamic
/// split - see `docs/features/lua-compatibility.md`).
fn write_lua_run(result: sol::lua_runtime::LuaRun) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&result.output)
        .and_then(|()| stdout.write_all(&result.value.display_bytes()))
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|error| format!("failed to write Lua output: {error}"))
}

/// Converts an uncaught `LuaError` to the CLI's plain-string error type,
/// first flushing whatever `print`/`io.write` output it had already
/// buffered. Real Lua writes each call straight to stdout as it happens;
/// Sol buffers a whole run's output and only flushes it on success
/// (`write_lua_run`), so without this an error occurring after any output
/// would silently discard everything printed before it. A write failure
/// here is not itself fatal to reporting the original Lua error, so it is
/// ignored rather than replacing the error text.
fn report_lua_error(error: sol::lua_runtime::LuaError) -> String {
    if !error.output.is_empty() {
        let _ = io::stdout().lock().write_all(&error.output);
    }
    error.to_string()
}

/// The `sol` CLI runs trusted local scripts, not untrusted embedded code, so
/// it relaxes `LuaRuntime::new`'s sandboxed-embedder defaults (sized for
/// untrusted code) via opt-in env vars, mirroring `os`/`io` capabilities
/// already being enabled unconditionally for the CLI. Defaults stay at
/// `LuaRuntime::new`'s values when unset, so library embedders are unaffected.
fn lua_dynamic_budgets() -> (u64, usize, usize) {
    let instruction_budget = std::env::var("SOL_LUA_INSTRUCTION_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let max_call_depth = std::env::var("SOL_LUA_CALL_DEPTH_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000);
    let allocation_budget = std::env::var("SOL_LUA_ALLOCATION_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(64 * 1024 * 1024);
    (instruction_budget, max_call_depth, allocation_budget)
}

/// Opt-in GC stress mode (see `LuaRuntime::set_gc_stress`): runs a full
/// collection pass at every allocation and after every dynamic instruction
/// instead of only on an explicit `collectgarbage()` call. Far too slow for
/// normal use; exists for manually reproducing GC-timing-sensitive bugs.
fn lua_gc_stress_enabled() -> bool {
    std::env::var("SOL_LUA_GC_STRESS").is_ok_and(|s| s == "1")
}

fn legacy_partition_requested() -> bool {
    matches!(
        std::env::var("SOL_RUNTIME_PATH").as_deref(),
        Ok("legacy-partition")
    )
}

fn unified_mixed_required() -> bool {
    std::env::var("SOL_REQUIRE_UNIFIED_MIXED").is_ok_and(|value| value == "1")
}

fn try_run_mixed_main(
    program: &sol::ast::Program,
    partition: &sol::typeck::LuaPartition,
    dynamic_contracts: &[sol::modules::DynamicModuleContract],
) -> Result<bool, String> {
    if legacy_partition_requested()
        || !partition
            .mixed
            .iter()
            .any(|function| function.name == "main")
    {
        return Ok(false);
    }

    let typed_names: HashSet<String> = partition
        .native
        .functions
        .iter()
        .chain(&partition.mixed)
        .map(|function| function.name.clone())
        .collect();
    let called_from_dynamic = sol::lua_bridge::called_from_dynamic(program, &partition.interpreted);
    if called_from_dynamic
        .iter()
        .any(|name| typed_names.contains(name))
    {
        // Reentrant dynamic -> specialized -> dynamic calls require the same
        // suspended-frame work as coroutine reentry. Keep those graphs on the
        // differential path until the source compiler can lower the cycle.
        return Ok(false);
    }

    let called_dynamic: HashSet<String> = partition
        .mixed
        .iter()
        .flat_map(sol::typeck::called_functions)
        .filter(|name| partition.interpreted.contains(name))
        .collect();
    let mut dynamic_signatures = HashMap::new();
    for name in &called_dynamic {
        let function = program
            .functions
            .iter()
            .find(|function| &function.name == name)
            .expect("partition names originate in the source program");
        if function.vararg {
            return Ok(false);
        }
        let Some(result) = function.return_type.as_ref().and_then(ast_bridge_scalar) else {
            return Ok(false);
        };
        let Some(parameters) = function
            .params
            .iter()
            .map(|(_, ty)| ast_bridge_scalar(ty))
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(false);
        };
        dynamic_signatures.insert(name.clone(), (parameters, result));
    }

    let mut typed_functions: Vec<&types::TFunction> = partition.native.functions.iter().collect();
    typed_functions.extend(partition.mixed.iter());
    let main = typed_functions
        .iter()
        .find(|function| function.name == "main")
        .expect("mixed partition contains main");
    let Some(main_result) = bridge_scalar(&main.return_type) else {
        return Ok(false);
    };
    let total = typed_functions.len() + called_dynamic.len();
    if total > 256 {
        return Ok(false);
    }
    let mut function_ids = HashMap::new();
    for (index, function) in typed_functions.iter().enumerate() {
        function_ids.insert(function.name.clone(), index as u8);
    }
    let mut dynamic_names: Vec<String> = called_dynamic.into_iter().collect();
    dynamic_names.sort();
    for (offset, name) in dynamic_names.iter().enumerate() {
        function_ids.insert(name.clone(), (typed_functions.len() + offset) as u8);
    }
    if typed_functions
        .iter()
        .flat_map(|function| sol::typeck::called_functions(function))
        .any(|name| !function_ids.contains_key(&name))
    {
        return Ok(false);
    }

    let mut slots = Vec::with_capacity(total);
    for function in &typed_functions {
        let Some(bytecode) = sol::bccompile::compile_function(function, &function_ids) else {
            return Ok(false);
        };
        slots.push(sol::interp::Slot::Bytecode(Rc::new(bytecode)));
    }

    let capabilities = sol::lua_runtime::Capabilities::NATIVE_CLI;
    let (instruction_budget, max_call_depth, allocation_budget) = lua_dynamic_budgets();
    let mut dynamic = sol::lua_runtime::LuaRuntime::with_capabilities_and_budgets(
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
    );
    dynamic.set_gc_stress(lua_gc_stress_enabled());
    dynamic
        .load_with_natives(program, &typed_names, HashMap::new())
        .map_err(|error| error.to_string())?;
    for contract in dynamic_contracts {
        dynamic
            .preload_namespace_module(&contract.name, &contract.exports)
            .map_err(|error| error.to_string())?;
    }
    let dynamic = Rc::new(RefCell::new(dynamic));
    for name in dynamic_names {
        let (parameters, result) = dynamic_signatures
            .remove(&name)
            .expect("every dynamic adapter has a checked signature");
        let runtime = dynamic.clone();
        slots.push(sol::interp::Slot::Semantic(Rc::new(move |arguments| {
            runtime
                .borrow_mut()
                .call_global_scalar_outcome(&name, &parameters, result, arguments)
        })));
    }

    let runtime = sol::interp::Runtime::new(
        slots,
        u32::MAX,
        |_| None,
        u32::MAX,
        |_, _| None,
        sol::interp::SpeculativeConfig {
            candidates: HashMap::new(),
            threshold: u32::MAX,
            promote: Box::new(|_| None),
        },
        (),
    );
    let main_id = function_ids["main"];
    let raw = match runtime.call_outcome(main_id, &[]) {
        sol_core::CallOutcome::Returned(values) => values.into_iter().next().unwrap_or(0),
        sol_core::CallOutcome::Raised(error) => return Err(error),
        sol_core::CallOutcome::Yielded(_) => {
            return Err("attempt to yield from main outside a coroutine".into())
        }
        sol_core::CallOutcome::TailCall(_) => {
            unreachable!("the specialized dispatcher consumes tail calls")
        }
    };
    let value = match main_result {
        BridgeScalar::I64 => LuaValue::Integer(raw as i64),
        BridgeScalar::F64 => LuaValue::Float(f64::from_bits(raw)),
        BridgeScalar::Bool => LuaValue::Bool(raw != 0),
    };
    let output = dynamic.borrow_mut().take_output();
    write_lua_run(sol::lua_runtime::LuaRun { value, output })?;
    Ok(true)
}

fn run_lua_partitioned(
    path: &str,
    source: &[u8],
    program: &sol::ast::Program,
    dynamic_contracts: &[sol::modules::DynamicModuleContract],
) -> Result<PartitionOutcome, String> {
    let partition = match sol::typeck::check_partitioned(program) {
        Ok(partition) => partition,
        // A handful of legal `.lua` constructs (e.g. redefining a top-level
        // `function name` more than once, which is just ordinary global
        // reassignment) can't be represented in the partitioned typed/
        // dynamic split at all - `check_partitioned` tags those the same
        // way `check` tags dynamic-only calls. Fall all the way back to a
        // fully interpreted run instead of a hard failure.
        Err(error) if sol::typeck::requires_dynamic_runtime(&error) => {
            let capabilities = sol::lua_runtime::Capabilities::NATIVE_CLI;
            let (instruction_budget, max_call_depth, allocation_budget) = lua_dynamic_budgets();
            let result = sol::lua_runtime::run_program_with_natives_and_budgets(
                program,
                &HashSet::new(),
                HashMap::new(),
                capabilities,
                instruction_budget,
                max_call_depth,
                allocation_budget,
                lua_gc_stress_enabled(),
                Some(cli_chunk_name(path)),
            )
            .map_err(report_lua_error)?;
            write_lua_run(result)?;
            return Ok(PartitionOutcome::Done);
        }
        Err(error) => return Err(render_source_error(path, source, error)),
    };

    let mixed_main = partition
        .mixed
        .iter()
        .any(|function| function.name == "main");
    if try_run_mixed_main(program, &partition, dynamic_contracts)? {
        return Ok(PartitionOutcome::Done);
    }
    if mixed_main && unified_mixed_required() {
        return Err("program could not enter the unified mixed-tier path".into());
    }

    if !partition.dynamic.contains("main") {
        let return_type = partition
            .native
            .functions
            .iter()
            .find(|f| f.name == "main")
            .map(|f| f.return_type.clone())
            .expect("main is native, so check_partitioned put it in partition.native");
        return Ok(PartitionOutcome::RunNative(partition.native, return_type));
    }

    let native_names: HashSet<String> = partition
        .native
        .functions
        .iter()
        .map(|f| f.name.clone())
        .collect();
    let bridge_names = sol::lua_bridge::called_from_dynamic(program, &partition.dynamic);

    struct BridgeSpec {
        name: String,
        params: Vec<BridgeScalar>,
        ret: BridgeScalar,
    }
    let mut specs = Vec::new();
    for f in &partition.native.functions {
        if !bridge_names.contains(&f.name) {
            continue;
        }
        let params = f
            .params
            .iter()
            .map(|(_, ty)| {
                bridge_scalar(ty).ok_or_else(|| {
                    format!(
                        "'{}' is called from dynamic Lua code but its parameter type {ty} \
                         cannot cross the runtime boundary (only i64/f64/bool are supported)",
                        f.name
                    )
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let ret = bridge_scalar(&f.return_type).ok_or_else(|| {
            format!(
                "'{}' is called from dynamic Lua code but its return type {} \
                 cannot cross the runtime boundary (only i64/f64/bool are supported)",
                f.name, f.return_type
            )
        })?;
        specs.push(BridgeSpec {
            name: f.name.clone(),
            params,
            ret,
        });
    }

    let mut natives = HashMap::new();
    if !specs.is_empty() {
        let mut jit = sol::jit::Jit::new(partition.native)
            .map_err(|error| format!("failed to compile '{path}' native helpers: {error}"))?;
        gc::init_stack_base();
        for spec in specs {
            let (ptr, _deps) = jit
                .promote(&spec.name)
                .map_err(|error| format!("failed to compile '{}': {error}", spec.name))?;
            natives.insert(
                spec.name.clone(),
                LuaValue::Native(Rc::new(NativeBridge {
                    name: spec.name,
                    ptr,
                    params: spec.params,
                    ret: spec.ret,
                })),
            );
        }
    }

    let capabilities = sol::lua_runtime::Capabilities::NATIVE_CLI;
    let (instruction_budget, max_call_depth, allocation_budget) = lua_dynamic_budgets();
    let result = sol::lua_runtime::run_program_with_natives_and_budgets(
        program,
        &native_names,
        natives,
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
        lua_gc_stress_enabled(),
        Some(cli_chunk_name(path)),
    )
    .map_err(report_lua_error)?;
    write_lua_run(result)?;
    Ok(PartitionOutcome::Done)
}

fn run(
    path: &str,
    profile_in: Option<&str>,
    profile_out: Option<&str>,
    profile_time: Option<&str>,
) -> Result<(), String> {
    let source = read_source(path)?;
    let language = language_config(path);
    let dynamic_fallback = !language.sol_extensions;
    // Parse once for semantic routing. File extensions select only which Sol
    // extensions are recognized; the AST's explicit type surface selects the
    // generic or specialized execution tier.
    let tokens = sol::lexer::lex_bytes(&source)
        .map_err(|error| render_source_error(path, &source, error))?;
    let parsed_program = sol::parser::parse_with_config(tokens, language)
        .map_err(|error| render_source_error(path, &source, error))?;
    let type_analysis = (language.type_policy != TypePolicy::Dynamic
        || env::var_os("SOL_EXPLAIN_TYPES").is_some())
    .then(|| sol::typeck::inference::analyze(&parsed_program, language.type_policy));
    if env::var_os("SOL_EXPLAIN_TYPES").is_some() {
        eprint!(
            "{}",
            type_analysis.as_ref().expect("requested above").render()
        );
    }
    if !requires_specialized_execution(&parsed_program) && !legacy_partition_requested() {
        if profile_in.is_some() || profile_out.is_some() || profile_time.is_some() {
            return Err("profiling the generic semantic interpreter is not implemented yet".into());
        }
        let capabilities = sol::lua_runtime::Capabilities::NATIVE_CLI;
        let (instruction_budget, max_call_depth, allocation_budget) = lua_dynamic_budgets();
        let result = match &type_analysis {
            Some(analysis) if language.type_policy != TypePolicy::Dynamic => {
                sol::lua_runtime::run_program_with_natives_budgets_and_plan(
                    &parsed_program,
                    &HashSet::new(),
                    HashMap::new(),
                    capabilities,
                    instruction_budget,
                    max_call_depth,
                    allocation_budget,
                    lua_gc_stress_enabled(),
                    &analysis.optimization_plan,
                    Some(cli_chunk_name(path)),
                )
            }
            _ => sol::lua_runtime::run_program_with_natives_and_budgets(
                &parsed_program,
                &HashSet::new(),
                HashMap::new(),
                capabilities,
                instruction_budget,
                max_call_depth,
                allocation_budget,
                lua_gc_stress_enabled(),
                Some(cli_chunk_name(path)),
            ),
        }
        .map_err(report_lua_error)?;
        return write_lua_run(result);
    }
    let mut project_native = None;
    if !parsed_program.imports.is_empty() {
        let project = sol::modules::load_project_program(path, &source)?;
        if project.has_dynamic_modules {
            let mut program = project.program;
            sol::aliases::expand(&mut program)?;
            sol::closures::lower(&mut program)?;
            match run_lua_partitioned(path, &source, &program, &project.dynamic_contracts)? {
                PartitionOutcome::Done => return Ok(()),
                PartitionOutcome::RunNative(tprogram, return_type) => {
                    project_native = Some((tprogram, return_type));
                }
            }
        }
    }
    let lua_program = dynamic_fallback.then_some(parsed_program);
    let compiled = match project_native {
        Some(compiled) => Ok(compiled),
        None => match &lua_program {
            Some(program) => sol::compile_program_with_config(program.clone(), language),
            None => compile_source(path, &source),
        },
    };
    let (tprogram, return_type) = match compiled {
        Ok(compiled) => compiled,
        // Keep the already-shipped typed `.lua` subset on the native path.
        // Parsed dynamic-only syntax enters the compatibility interpreter.
        Err(error) if dynamic_fallback && sol::typeck::requires_dynamic_runtime(&error) => {
            if profile_in.is_some() || profile_out.is_some() || profile_time.is_some() {
                return Err("profiling the dynamic Lua interpreter is not implemented yet".into());
            }
            let program = lua_program.expect("dynamic fallback implies a parsed program");
            match run_lua_partitioned(path, &source, &program, &[])? {
                PartitionOutcome::Done => return Ok(()),
                PartitionOutcome::RunNative(tprogram, return_type) => (tprogram, return_type),
            }
        }
        Err(error) => return Err(render_source_error(path, &source, error)),
    };

    // M7 §23 PGO: a prior run's `--profile-out` preloads this run's
    // promotions/specializations, skipping interpreted warm-up.
    let profile = profile_in.map(tier::Profile::load).transpose()?;

    // `--profile-time` uses `Engine<TimingHooks>` instead of the plain
    // `Engine<()>` every other invocation of `run` uses - two genuinely
    // separate monomorphizations (see `interp::Hooks`), so choosing this branch
    // is the only place the M8 timing instrumentation exists at all; the `else`
    // branch's compiled code is unchanged by M8.
    if let Some(time_report_path) = profile_time {
        let engine =
            tier::Engine::new(tprogram, profile.as_ref(), sol::profile::TimingHooks::new())?;
        gc::init_stack_base();
        let result = engine.call("main", &[]);
        print_result(return_type, result);
        let report = engine.hooks().report(|id| engine.name_of(id).to_string());
        fs::write(time_report_path, report)
            .map_err(|e| format!("failed to write time report '{time_report_path}': {e}"))?;
        if let Some(out) = profile_out {
            engine.dump_profile(out)?;
        }
    } else {
        let engine = tier::Engine::new(tprogram, profile.as_ref(), ())?;
        gc::init_stack_base();
        let result = engine.call("main", &[]);
        print_result(return_type, result);
        if let Some(out) = profile_out {
            engine.dump_profile(out)?;
        }
    }

    // Reports what's still live at exit - useful for spotting leaks.
    if std::env::var_os("SOL_GC_STATS").is_some() {
        gc::collect(); // force a final collection so the count is accurate
        eprintln!(
            "[gc] live at exit: {} block(s), {} byte(s)",
            gc::live_blocks(),
            gc::live_bytes(),
        );
    }
    Ok(())
}
