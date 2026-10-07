// U12 item 2, plan goal (a): a throwaway wasm-bindgen harness proving
// `tier0::Engine` runs end-to-end from JS before any `apps/web` wiring. Not a
// product surface - a single `.sol`/`.lua` source string in, its `main()`
// result or error out, exactly mirroring `tests/tier0_conformance.rs`'s
// `run_on_tier0` so the same code path is what's actually being proven here.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn run_sol(source: &str) -> Result<String, JsValue> {
    let (program, return_type) = crate::compile(source).map_err(|e| JsValue::from_str(&e))?;
    let engine: crate::tier0::Engine =
        crate::tier0::Engine::new(program, ()).map_err(|e| JsValue::from_str(&e))?;
    match engine.call_outcome("main", &[]) {
        sol_core::CallOutcome::Returned(values) => {
            let result = values.first().copied().unwrap_or(0);
            Ok(match return_type {
                crate::types::Type::I64 => (result as i64).to_string(),
                crate::types::Type::F64 => f64::from_bits(result).to_string(),
                crate::types::Type::Bool => (result != 0).to_string(),
                other => format!("<unsupported return type for this harness: {other:?}>"),
            })
        }
        sol_core::CallOutcome::Raised(error) => Err(JsValue::from_str(&error)),
        other => Err(JsValue::from_str(&format!(
            "unexpected call outcome: {other:?}"
        ))),
    }
}

// =========================================================================
// U12 item 6: the real `packages/sol-runtime` product surface.
//
// Everything above this point (`run_sol`) is item 2's throwaway spike, kept
// unchanged and still used by its own differential harness - do not delete.
// Everything below is new: a non-throwing `execute()` entry point plus a
// `#[wasm_bindgen] WasmDebugSession` wrapping `crate::debugger::DebugSession`
// (see that module's doc comment for the underlying engine's full-trace-
// recording architecture - this file adds no new execution semantics, only
// a wasm-bindgen-friendly surface over what `debugger.rs` already proved out
// natively in items 3-5).
//
// Scope, stated up front (see
// `docs/features/milestones/u12-wasm-playground.md`'s Work item 6 section
// for the full file:line evidence behind each claim):
//
// - **Single-file `.sol` source only.** `crate::modules::compile_project`/
//   `load_project_program` resolve a multi-file project's `import`s via real
//   `std::fs::read` + path canonicalization. That module is already part of
//   this crate's always-compiled, unconditionally-`pub mod`-declared surface
//   (`lib.rs`'s `pub mod modules;` has no `#[cfg(feature = "jit")]`/wasm
//   gate), so it already compiles for `wasm32-unknown-unknown` today - the
//   risk is not a compile failure, it is a *runtime* one: `apps/web`'s
//   worker protocol passes in-memory `names: string[]`/`contents: string[]`
//   with no backing filesystem at all, and `std::fs::read` on
//   `wasm32-unknown-unknown` always fails at runtime (there is no real
//   filesystem to back it - the standard library's wasm32-unknown-unknown
//   `fs` backend is a stub that returns an `Unsupported`-style `io::Error`
//   for every operation, not a working no-op). Adapting `Loader` to accept
//   a pluggable in-memory file map instead of hardcoded `fs::read` is a
//   real, bounded follow-up (parameterize `Loader::load` over an injected
//   `Fn(&Path) -> Result<Vec<u8>, String>` instead of calling `fs::read`
//   directly), deliberately not attempted here so this item lands a working
//   single-file slice rather than a half-wired multi-file path. `execute`/
//   `WasmDebugSession::launch` below therefore only ever compile one
//   in-memory source string via `crate::compile`/`crate::compile_bytes`,
//   never `modules::compile_project`.
// - **`.sol` source, not idiomatic `.lua`.** `crate::compile_bytes` with
//   `parser::SourceMode::Lua` does route through the same `TProgram`/
//   `tier0::Engine` pipeline as `.sol` (confirmed by reading
//   `lib.rs::compile_program_with_config`: both profiles share
//   `typeck::check`, just with `allow_dynamic_fallback` flipped), but
//   verified directly (not assumed) that this does not make idiomatic Lua
//   scripts runnable: a bare `print("hello")` fails to even typecheck via
//   this path with `"unknown function 'print' [EDYNLUA]"`, because
//   `print`/the rest of Lua's standard library are wired up only inside the
//   separate, dynamic `LuaRuntime` (`lua_runtime/init.rs`'s global-table
//   setup), which `compile_bytes`/`tier0::Engine` never constructs at all.
//   So while this file does not hard-reject `.lua` source, there is no
//   product-ready `.lua` support here - only `.sol` is exercised or tested.
// - **No output-buffer/`debugTakeOutput` equivalent exists, for either
//   language, on this engine.** The retired `crate/lua-vm` engine's
//   `take_output`/`ExecuteResult.output` captures an accumulating buffer
//   that a Lua `print()` call writes into as the program runs. Checked
//   directly: typed `.sol` has **no** `print`/`io.write`/any other builtin
//   that writes to anything at all (confirmed by grepping `"print"`/`io.write`
//   across every non-`lua_runtime` module - the `sol` CLI's own `println!`
//   calls in `main.rs` are the CLI printing `main()`'s single final return
//   value once, not something a `.sol` program itself can call). So the only
//   "output" concept this engine has, for either `execute()` or
//   `WasmDebugSession::run()` below, is the rendered return value of
//   `main()` itself - already returned directly as this item's `result`
//   field, not accumulated in a separate buffer a caller polls later. There
//   is no `take_output`/`debugOutput` method here; inventing one that always
//   returns an empty string would be a disguised stub, not an honest gap.
// - **No metatable, mutable-global-variable, or separate-upvalue concept at
//   this tier** - items 3/4's own findings, restated here because this file
//   is the first place those gaps become visible as *missing wasm methods*
//   rather than prose: no `get_globals`/`get_upvalues`/`get_metatable`
//   wrapper exists on `WasmDebugSession` below, and none should be added
//   without the underlying `debugger.rs`/typed-tier capability actually
//   existing first.
// - **`debugSetVariable` has no equivalent** - item 4's finding (a
//   `TraceStep`'s `regs` are a frozen copy already computed by the one
//   completed run; mutating one recorded step's copy cannot retroactively
//   change later, already-recorded steps) applies unchanged here.
// - **`set_breakpoint_condition`/`set_breakpoint_hit_condition`/
//   `set_breakpoint_log_message`/`remove_breakpoint` are deferred, not
//   attempted.** A condition genuinely looks implementable without a live
//   interpreter (a predicate evaluated via `DebugSession::evaluate` at each
//   candidate trace index - the whole trace already exists), but doing that
//   honestly means extending `debugger.rs`'s `breakpoints` storage and its
//   `continue_to_breakpoint`/`first_breakpoint_hit` matching logic, with its
//   own new differential tests - a real, separate, bounded increment this
//   item deliberately leaves for a follow-up rather than rushing under this
//   item's own time budget. `remove_breakpoint` has the same shape (no
//   underlying `debugger.rs` removal API exists yet).
// - **`continue_burst` *is* implemented below, honestly bounded.** See
//   `WasmDebugSession::continue_burst`'s own doc comment for why this one,
//   unlike the others above, does not need any `debugger.rs` change at all:
//   `tier0::Engine`'s instruction budget (`tier0::DEFAULT_INSTRUCTION_BUDGET`,
//   10,000,000, unconditionally enforced inside `interp::Runtime::interpret`'s
//   dispatch loop since item 2) already makes `DebugSession::run` incapable
//   of hanging forever - it always either finishes or raises "instruction
//   budget exceeded" within that many instructions. So the whole trace this
//   file pages through is already bounded before `continue_burst` is ever
//   called; "burst" here means paging through chunks of that
//   already-bounded, already-complete trace, not truly-incremental
//   execution - stated plainly in that method's own doc comment too.

use std::cell::RefCell;

use crate::debugger::{self, DebugSession, DisplayValue, TimelineEventKind};
use crate::types::Type;

mod lua_debug;
pub use lua_debug::*;
mod typed_debug;
pub use typed_debug::*;

// -------------------------------------------------------------------------
// execute(): one-shot run, non-throwing (mirrors crate/lua-vm's own
// `execute()`/`ExecuteResult` calling convention, read directly from
// `crate/lua-vm/src/lib.rs` for field-naming guidance - not required to
// match it exactly, since this is this engine's own, differently-shaped
// result. `result` replaces the old `output` field name deliberately: see
// this file's top doc comment for why there is no output-buffer concept
// here, only a single rendered return value).
// -------------------------------------------------------------------------

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    result: Option<String>,
    error: Option<String>,
}

#[wasm_bindgen]
impl ExecuteResult {
    /// `main()`'s rendered return value, or `None` if compilation/execution
    /// failed (see `error`). There is no separate accumulated-output buffer
    /// to read afterward - this is the entire "output" of a run on this
    /// engine, by construction (see this file's top doc comment).
    #[wasm_bindgen(getter)]
    pub fn result(&self) -> Option<String> {
        self.result.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

fn render_return(ty: &Type, raw: u64) -> String {
    match ty {
        Type::I64 => (raw as i64).to_string(),
        Type::F64 => f64::from_bits(raw).to_string(),
        Type::Bool => (raw != 0).to_string(),
        Type::Nil => "nil".to_string(),
        Type::String => {
            if raw == 0 {
                "nil".to_string()
            } else {
                let bytes = unsafe { crate::strings::bytes(raw as *const u8) };
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
        // `lib.rs::compile_program_with_config` already rejects Array/Map/
        // Struct/Function as `main`'s return type before this point is ever
        // reached - `Any` is the one scalar-adjacent case that can still
        // arrive here (e.g. a `.lua`-profile program whose `main` return
        // type fell back to `any`); rendered as its raw tag-bearing word
        // rather than unboxed, matching `debugger.rs`'s own stated "no
        // reverse tag registry exists" gap.
        other => format!("<unrenderable return type {other:?}, raw={raw}>"),
    }
}

/// Compiles and runs a single typed Sol source to completion. The live
/// Tier-0 dispatcher shares the ordinary interpreter's opcode semantics,
/// but retains explicit rooted frames rather than a recursive Rust stack.
/// Errors return in `ExecuteResult.error`, not as a JS exception. The older
/// typed debugger below remains trace-based until its adapter is replaced.
#[wasm_bindgen]
pub fn execute(source: &str) -> ExecuteResult {
    execute_compiled(crate::compile(source))
}

/// Runs one browser-sandboxed Lua chunk through Sol's canonical dynamic
/// runtime. Its standard output is the result, matching the worker's normal
/// Lua playground contract; filesystem, process, and native-module access
/// remain disabled by `LuaRuntime`'s default capabilities.
#[wasm_bindgen]
pub fn execute_lua(source: &str) -> ExecuteResult {
    execute_lua_run(crate::lua_runtime::run_source_with_modules(source.as_bytes(), Vec::new()))
}

#[wasm_bindgen]
pub fn execute_lua_project(entry: String, names: Vec<String>, contents: Vec<String>) -> ExecuteResult {
    if let Err(error) = validate_lua_project(&entry, &names, &contents) {
        return ExecuteResult { result: None, error: Some(error) };
    }
    if names.len() != contents.len() {
        return ExecuteResult { result: None, error: Some("project file names and contents have different lengths".to_string()) };
    }
    let Some(entry_index) = names.iter().position(|name| name == &entry) else {
        return ExecuteResult { result: None, error: Some(format!("entry file '{entry}' is not present in the in-memory project")) };
    };
    let modules = names.iter().zip(contents.iter()).enumerate().filter_map(|(index, (name, source))| {
        (index != entry_index && name.ends_with(".lua")).then(|| {
            let module = name.strip_suffix(".lua").unwrap().replace('/', ".").into_bytes();
            (module, source.as_bytes().to_vec())
        })
    });
    execute_lua_run(crate::lua_runtime::run_source_with_modules(contents[entry_index].as_bytes(), modules))
}

fn validate_lua_project(entry: &str, names: &[String], contents: &[String]) -> Result<(), String> {
    if names.len() != contents.len() { return Err("project file names and contents have different lengths".into()); }
    let mut paths = std::collections::HashSet::new();
    let mut modules = std::collections::HashSet::new();
    for name in names {
        if !name.ends_with(".lua") || name.contains('\\') || name.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err(format!("invalid in-memory Lua project path '{name}'"));
        }
        if !paths.insert(name) { return Err(format!("duplicate project path '{name}'")); }
        if !modules.insert(name.strip_suffix(".lua").unwrap().replace('/', ".")) {
            return Err(format!("ambiguous Lua module path '{name}'"));
        }
    }
    if !paths.contains(&entry.to_string()) { return Err(format!("entry file '{entry}' is not present in the in-memory project")); }
    Ok(())
}

fn execute_lua_run(run: crate::lua_runtime::LuaResult<crate::lua_runtime::LuaRun>) -> ExecuteResult {
    match run {
        Ok(run) => ExecuteResult {
            result: Some(String::from_utf8_lossy(&run.output).into_owned()),
            error: None,
        },
        Err(error) => ExecuteResult {
            result: Some(String::from_utf8_lossy(&error.output).into_owned()),
            error: Some(error.to_string()),
        },
    }
}

/// Browser-safe multi-file `.sol` entry point. `names` and `contents` use
/// the existing worker protocol's parallel-array shape; imports resolve only
/// within those supplied files and never access the host filesystem.
#[wasm_bindgen]
pub fn execute_project(entry: String, names: Vec<String>, contents: Vec<String>) -> ExecuteResult {
    if names.len() != contents.len() {
        return ExecuteResult {
            result: None,
            error: Some("project file names and contents have different lengths".to_string()),
        };
    }
    let files = names.into_iter().zip(contents).map(|(name, source)| {
        (name, source.into_bytes())
    }).collect::<Vec<_>>();
    execute_compiled(crate::modules::compile_project_from_sources(&entry, &files))
}

fn execute_compiled(compiled: Result<(crate::types::TProgram, Type), String>) -> ExecuteResult {
    let (program, return_type) = match compiled {
        Ok(ok) => ok,
        Err(error) => {
            return ExecuteResult {
                result: None,
                error: Some(error),
            }
        }
    };
    let engine: crate::tier0::Engine = match crate::tier0::Engine::new(program, ()) {
        Ok(engine) => engine,
        Err(error) => {
            return ExecuteResult {
                result: None,
                error: Some(error),
            }
        }
    };
    let mut execution = match engine.start_live("main", &[]) {
        Ok(execution) => execution,
        Err(error) => return ExecuteResult { result: None, error: Some(error) },
    };
    loop {
        match execution.resume(10_000) {
            crate::interp::live::Stop::Paused => {},
            crate::interp::live::Stop::Returned(value) => return ExecuteResult {
                result: Some(render_return(&return_type, value)), error: None,
            },
            crate::interp::live::Stop::Raised(error) => return ExecuteResult {
                result: None, error: Some(error),
            },
        }
    }
}

// -------------------------------------------------------------------------
// Shared JS-facing value types.
// -------------------------------------------------------------------------

/// Wire shape for `debugger::DisplayValue` - either a self-contained scalar
/// string, or an opaque `reference` a caller can later pass back to
/// `WasmDebugSession::expand` (alongside the `type_id` it was reported
/// with - see `WasmLocalView`/`WasmTableEntry`'s own doc comments for why a
/// type id, not the type itself, travels across the wasm boundary).
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmValue {
    is_reference: bool,
    scalar: Option<String>,
    reference: Option<u64>,
    summary: Option<String>,
}

#[wasm_bindgen]
impl WasmValue {
    #[wasm_bindgen(getter)]
    pub fn is_reference(&self) -> bool {
        self.is_reference
    }
    /// Set iff `is_reference` is `false`.
    #[wasm_bindgen(getter)]
    pub fn scalar(&self) -> Option<String> {
        self.scalar.clone()
    }
    /// Set iff `is_reference` is `true` - the raw handle to pass to
    /// `WasmDebugSession::expand`.
    #[wasm_bindgen(getter)]
    pub fn reference(&self) -> Option<u64> {
        self.reference
    }
    /// Set iff `is_reference` is `true` - a short type-tag display string
    /// (e.g. `"Array<i64>"`), not itself expandable further.
    #[wasm_bindgen(getter)]
    pub fn summary(&self) -> Option<String> {
        self.summary.clone()
    }
}

impl From<DisplayValue> for WasmValue {
    fn from(value: DisplayValue) -> Self {
        match value {
            DisplayValue::Scalar(s) => WasmValue {
                is_reference: false,
                scalar: Some(s),
                reference: None,
                summary: None,
            },
            DisplayValue::Reference { reference, summary } => WasmValue {
                is_reference: true,
                scalar: None,
                reference: Some(reference),
                summary: Some(summary),
            },
        }
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmBreakpoint {
    id: u32,
    function_name: std::string::String,
    line: u32,
    verified: bool,
    pc: Option<u32>,
}

#[wasm_bindgen]
impl WasmBreakpoint {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }
    #[wasm_bindgen(getter)]
    pub fn function_name(&self) -> std::string::String {
        self.function_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> u32 {
        self.line
    }
    #[wasm_bindgen(getter)]
    pub fn verified(&self) -> bool {
        self.verified
    }
    #[wasm_bindgen(getter)]
    pub fn pc(&self) -> Option<u32> {
        self.pc
    }
}

impl From<debugger::VerifiedBreakpoint> for WasmBreakpoint {
    fn from(b: debugger::VerifiedBreakpoint) -> Self {
        WasmBreakpoint {
            id: b.id,
            function_name: b.function_name,
            line: b.line,
            verified: b.verified,
            pc: b.pc,
        }
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmTraceStep {
    func_id: u8,
    function_name: std::string::String,
    pc: u32,
    line: u32,
    depth: u32,
}

#[wasm_bindgen]
impl WasmTraceStep {
    #[wasm_bindgen(getter)]
    pub fn func_id(&self) -> u8 {
        self.func_id
    }
    #[wasm_bindgen(getter)]
    pub fn function_name(&self) -> std::string::String {
        self.function_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn pc(&self) -> u32 {
        self.pc
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> u32 {
        self.line
    }
    #[wasm_bindgen(getter)]
    pub fn depth(&self) -> u32 {
        self.depth
    }
}

/// One local, as reported by `WasmDebugSession::locals_at`. `type_id` is an
/// opaque index into this session's own type registry (see
/// `WasmDebugSession`'s doc comment on `types`) - `debugger::Type` is a
/// recursive Rust enum with no direct wasm-bindgen representation, so rather
/// than flatten it into a JS-visible shape, callers that need to expand a
/// `Reference` value just pass `type_id` straight back to
/// `WasmDebugSession::expand` without ever needing to understand its
/// contents. `type_name` is a human-readable display string for the same
/// type (e.g. `"Array<i64>"`), for UI labeling.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmLocalView {
    local_id: u32,
    is_param: bool,
    type_id: u32,
    type_name: std::string::String,
    value: WasmValue,
}

#[wasm_bindgen]
impl WasmLocalView {
    #[wasm_bindgen(getter)]
    pub fn local_id(&self) -> u32 {
        self.local_id
    }
    #[wasm_bindgen(getter)]
    pub fn is_param(&self) -> bool {
        self.is_param
    }
    #[wasm_bindgen(getter)]
    pub fn type_id(&self) -> u32 {
        self.type_id
    }
    #[wasm_bindgen(getter)]
    pub fn type_name(&self) -> std::string::String {
        self.type_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn value(&self) -> WasmValue {
        self.value.clone()
    }
}

/// One entry of an expanded `Array`/`Map`/`Struct` reference - see
/// `WasmLocalView`'s doc comment for what `type_id` means.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmTableEntry {
    label: std::string::String,
    type_id: u32,
    type_name: std::string::String,
    value: WasmValue,
}

#[wasm_bindgen]
impl WasmTableEntry {
    #[wasm_bindgen(getter)]
    pub fn label(&self) -> std::string::String {
        self.label.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn type_id(&self) -> u32 {
        self.type_id
    }
    #[wasm_bindgen(getter)]
    pub fn type_name(&self) -> std::string::String {
        self.type_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn value(&self) -> WasmValue {
        self.value.clone()
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmEvalResult {
    ok: bool,
    display: std::string::String,
}

#[wasm_bindgen]
impl WasmEvalResult {
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.ok
    }
    /// The value's display string on success, or the error message on
    /// failure - matching `crate/lua-vm`'s own `EvalResult` convention.
    #[wasm_bindgen(getter)]
    pub fn display(&self) -> std::string::String {
        self.display.clone()
    }
}

/// `DebugSession::memory_stats`'s report, unchanged field names. See that
/// method's own doc comment (`debugger.rs`) for exactly what `live_bytes`/
/// `live_blocks` do and do not mean here, and why there is no
/// `external_allocation`/`allocation_debt` equivalent (those describe
/// generational-GC bookkeeping concepts this jit-free execution path does
/// not have) - deliberately not invented as always-zero fields here.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy)]
pub struct WasmMemoryStats {
    live_bytes: f64,
    live_blocks: f64,
}

#[wasm_bindgen]
impl WasmMemoryStats {
    #[wasm_bindgen(getter)]
    pub fn live_bytes(&self) -> f64 {
        self.live_bytes
    }
    #[wasm_bindgen(getter)]
    pub fn live_blocks(&self) -> f64 {
        self.live_blocks
    }
}

/// See `DebugSession::threads`'s doc comment for why this always reports
/// exactly one constant entry, not real multi-thread/coroutine support.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmThreadInfo {
    id: u32,
    status: std::string::String,
}

#[wasm_bindgen]
impl WasmThreadInfo {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> std::string::String {
        self.status.clone()
    }
}

/// See `DebugSession::profile`'s doc comment for exactly how
/// `self_instructions`/`total_instructions` are attributed.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmFunctionStats {
    function_name: std::string::String,
    // Browser budgets/depths keep these below 2^53, so JS numbers preserve
    // exact counters without truncating recursive inclusive totals to u32.
    calls: f64,
    self_instructions: f64,
    total_instructions: f64,
}

#[wasm_bindgen]
impl WasmFunctionStats {
    #[wasm_bindgen(getter)]
    pub fn function_name(&self) -> std::string::String {
        self.function_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn calls(&self) -> f64 {
        self.calls
    }
    #[wasm_bindgen(getter)]
    pub fn self_instructions(&self) -> f64 {
        self.self_instructions
    }
    #[wasm_bindgen(getter)]
    pub fn total_instructions(&self) -> f64 {
        self.total_instructions
    }
}

/// See `TimelineEventKind`'s doc comment (`debugger.rs`) for what each
/// `kind` string (`"call_enter"`/`"call_exit"`/`"tail_call"`) means, and
/// `TimelineEvent::step_index`'s doc comment for the one synthetic event
/// (the top-level call's own closing exit) whose `step_index` is one past
/// the end of the trace rather than a real trace index.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct WasmTimelineEvent {
    kind: std::string::String,
    function_name: std::string::String,
    depth: u32,
    step_index: u32,
}

#[wasm_bindgen]
impl WasmTimelineEvent {
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> std::string::String {
        self.kind.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn function_name(&self) -> std::string::String {
        self.function_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn depth(&self) -> u32 {
        self.depth
    }
    #[wasm_bindgen(getter)]
    pub fn step_index(&self) -> u32 {
        self.step_index
    }
}

/// `WasmDebugSession::continue_burst`'s result. See that method's own doc
/// comment for why this is an honest paging operation over an
/// already-complete, already-bounded trace, not true incremental execution.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy)]
pub struct WasmBurstResult {
    stopped: bool,
    index: u32,
    exhausted: bool,
}

#[wasm_bindgen]
impl WasmBurstResult {
    /// `true` iff a verified breakpoint was hit within this burst's
    /// instruction window - `index` is that hit's trace index.
    #[wasm_bindgen(getter)]
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    /// The trace index reached: a breakpoint hit if `stopped`, otherwise
    /// the last index this burst's instruction budget allowed.
    #[wasm_bindgen(getter)]
    pub fn index(&self) -> u32 {
        self.index
    }
    /// `true` iff `index` is the trace's last entry (the run has fully
    /// completed - there is nothing further to burst through).
    #[wasm_bindgen(getter)]
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }
}

// -------------------------------------------------------------------------
// WasmDebugSession
// -------------------------------------------------------------------------

/// The real product surface wrapping `debugger::DebugSession`. Construct via
/// `launch`, then `run()` once (same one-run-per-session convention as the
/// native `DebugSession` - see its doc comment), then page through
/// `trace()`/`locals_at()`/`step_*()`/`evaluate()` against the resulting
/// recorded trace.
///
/// Unlike the native `DebugSession::run(function_name, args)` (generic over
/// any function and arbitrary raw register-word arguments), this wrapper
/// always targets `main()` with zero arguments - a deliberate simplification
/// matching `execute()`/`run_sol`'s own convention and this milestone's
/// actual use case (a browser playground always runs a program's entry
/// point); marshaling arbitrary typed arguments across the wasm boundary
/// for an arbitrary function is a real, separate feature this item does not
/// need. `types` is this session's own type registry: `debugger::Type` is a
/// recursive Rust enum with no direct JS representation, so rather than
/// flatten it into a wasm-bindgen shape, every type this session's
/// `locals_at`/`expand` outputs ever mentions gets pushed here once and
/// handed back to JS as a small opaque `u32` id (`WasmLocalView`/
/// `WasmTableEntry`'s `type_id`) that round-trips back into `expand`.
#[wasm_bindgen]
pub struct WasmDebugSession {
    session: DebugSession,
    return_type: Type,
    types: RefCell<Vec<Type>>,
}

fn timeline_kind_str(kind: TimelineEventKind) -> &'static str {
    match kind {
        TimelineEventKind::CallEnter => "call_enter",
        TimelineEventKind::CallExit => "call_exit",
        TimelineEventKind::TailCall => "tail_call",
    }
}

#[wasm_bindgen]
impl WasmDebugSession {
    /// Compiles `source` as `.sol` (see this file's top doc comment for the
    /// single-file/`.sol`-only scope) and builds a fresh `DebugSession` over
    /// it. Does not run anything yet - call `run()` next.
    pub fn launch(source: &str) -> Result<WasmDebugSession, JsValue> {
        wasm_debug_session_from_compiled(crate::compile(source))
    }

    /// Browser-safe multi-file counterpart to [`Self::launch`]. It shares
    /// `execute_project`'s in-memory-only module resolution and accepts only
    /// all-`.sol` projects that Tier-0 can execute.
    #[wasm_bindgen]
    pub fn launch_project(
        entry: String,
        names: Vec<String>,
        contents: Vec<String>,
    ) -> Result<WasmDebugSession, JsValue> {
        if names.len() != contents.len() {
            return Err(JsValue::from_str(
                "project file names and contents have different lengths",
            ));
        }
        let files = names
            .into_iter()
            .zip(contents)
            .map(|(name, source)| (name, source.into_bytes()))
            .collect::<Vec<_>>();
        wasm_debug_session_from_compiled(crate::modules::compile_project_from_sources(
            &entry, &files,
        ))
    }
}

fn wasm_debug_session_from_compiled(
    compiled: Result<(crate::types::TProgram, Type), String>,
) -> Result<WasmDebugSession, JsValue> {
        let (program, return_type) = compiled.map_err(|e| JsValue::from_str(&e))?;
        let session = DebugSession::new(program).map_err(|e| JsValue::from_str(&e))?;
        Ok(WasmDebugSession {
            session,
            return_type,
            types: RefCell::new(Vec::new()),
        })
}

#[wasm_bindgen]
impl WasmDebugSession {

    /// Deliverable 3 (`debugger::DebugSession::set_breakpoint`). Must be
    /// called before `run()` - breakpoint hits are found by scanning the
    /// trace `run()` produces, so a breakpoint set afterward would simply
    /// never match anything already recorded.
    pub fn set_breakpoint(&self, function_name: &str, line: u32) -> WasmBreakpoint {
        self.session.set_breakpoint(function_name, line).into()
    }

    /// Removes a breakpoint previously returned by `set_breakpoint`.
    #[wasm_bindgen]
    pub fn remove_breakpoint(&self, id: u32) -> bool {
        self.session.remove_breakpoint(id)
    }

    /// Applies an optional boolean condition to a breakpoint. The expression
    /// uses the documented `local<N>` names exposed by `evaluate`.
    #[wasm_bindgen]
    pub fn set_breakpoint_condition(&self, id: u32, condition: Option<String>) -> bool {
        self.session.set_breakpoint_condition(id, condition)
    }

    /// Requires a breakpoint's candidate source location to have been
    /// reached at least `hit_condition` times before it may stop. Passing
    /// `None` or zero disables the threshold.
    #[wasm_bindgen]
    pub fn set_breakpoint_hit_condition(&self, id: u32, hit_condition: Option<u32>) -> bool {
        self.session
            .set_breakpoint_hit_condition(id, hit_condition.map(u64::from))
    }

    /// Runs `main()` to completion, recording the full instruction trace.
    /// Call once per session (same convention as the native `DebugSession`).
    /// Returns the rendered return value on success - there is no separate
    /// output buffer to read afterward (see this file's top doc comment).
    pub fn run(&self) -> ExecuteResult {
        match self.session.run("main", &[]) {
            sol_core::CallOutcome::Returned(values) => ExecuteResult {
                result: Some(render_return(
                    &self.return_type,
                    values.first().copied().unwrap_or(0),
                )),
                error: None,
            },
            sol_core::CallOutcome::Raised(error) => ExecuteResult {
                result: None,
                error: Some(error),
            },
            other => ExecuteResult {
                result: None,
                error: Some(format!("unexpected call outcome: {other:?}")),
            },
        }
    }

    /// Number of recorded instructions in the current trace - `0` before
    /// `run()` is called.
    pub fn trace_length(&self) -> u32 {
        self.session.trace().len() as u32
    }

    /// One recorded instruction's metadata (not its register snapshot -
    /// that is only ever consumed internally by `locals_at`/`evaluate`, both
    /// below).
    pub fn trace_step(&self, index: u32) -> Option<WasmTraceStep> {
        self.session
            .trace()
            .get(index as usize)
            .map(|step| WasmTraceStep {
                func_id: step.func_id,
                function_name: self
                    .session
                    .function_name(step.func_id)
                    .unwrap_or("<unknown>")
                    .to_string(),
                pc: step.pc,
                line: step.line,
                depth: step.depth,
            })
    }

    pub fn first_breakpoint_hit(&self) -> Option<u32> {
        self.session.first_breakpoint_hit().map(|i| i as u32)
    }

    pub fn continue_to_breakpoint(&self, from: u32) -> Option<u32> {
        self.session
            .continue_to_breakpoint(from as usize)
            .map(|i| i as u32)
    }

    /// Pages through the already-complete, already-bounded trace `run()`
    /// produced, in chunks of at most `max_instructions`. This is **not**
    /// true incremental execution: `run()` already executed the whole call
    /// up front (bounded unconditionally by `tier0::DEFAULT_INSTRUCTION_BUDGET`
    /// = 10,000,000 instructions - `interp::Runtime::interpret`'s dispatch
    /// loop checks this every instruction and raises `"instruction budget
    /// exceeded"` if it trips, so `run()` itself can never hang a worker
    /// forever on a non-terminating script, by construction, independent of
    /// this method). `continue_burst` just re-derives "how far did we get
    /// before the next breakpoint or this burst's own window ran out" from
    /// that already-materialized trace - it cannot support a program that
    /// does not terminate within the instruction budget any better than
    /// `run()` already does (that case surfaces as `run()`'s own
    /// `ExecuteResult.error`, immediately, not as a `continue_burst` that
    /// never returns). See this file's top doc comment for why this is
    /// still worth providing even though it's not a live-pause primitive.
    pub fn continue_burst(&self, from: u32, max_instructions: u32) -> WasmBurstResult {
        let trace = self.session.trace();
        let last = trace.len().saturating_sub(1);
        let window_end = (from as usize)
            .saturating_add(max_instructions as usize)
            .min(last);
        if let Some(hit) = self.session.continue_to_breakpoint(from as usize) {
            if hit <= window_end {
                return WasmBurstResult {
                    stopped: true,
                    index: hit as u32,
                    exhausted: hit >= last,
                };
            }
        }
        WasmBurstResult {
            stopped: false,
            index: window_end as u32,
            exhausted: window_end >= last,
        }
    }

    pub fn step_into(&self, from: u32) -> Option<u32> {
        self.session.step_into(from as usize).map(|i| i as u32)
    }

    pub fn step_over(&self, from: u32) -> Option<u32> {
        self.session.step_over(from as usize).map(|i| i as u32)
    }

    pub fn step_out(&self, from: u32) -> Option<u32> {
        self.session.step_out(from as usize).map(|i| i as u32)
    }

    fn register_type(&self, ty: &Type) -> u32 {
        let mut types = self.types.borrow_mut();
        let id = types.len() as u32;
        types.push(ty.clone());
        id
    }

    /// Deliverable 4 (`debugger::DebugSession::locals_at`). `None` if `at`
    /// is out of range.
    pub fn locals_at(&self, at: u32) -> Option<Vec<WasmLocalView>> {
        let locals = self.session.locals_at(at as usize)?;
        Some(
            locals
                .into_iter()
                .map(|local| WasmLocalView {
                    local_id: local.local_id as u32,
                    is_param: local.is_param,
                    type_id: self.register_type(&local.ty),
                    type_name: local.type_name,
                    value: local.value.into(),
                })
                .collect(),
        )
    }

    /// Deliverable 4's lazy/paginated expansion. `type_id` must be one this
    /// same session previously handed back via `locals_at`/`expand` itself
    /// (see `WasmDebugSession`'s doc comment on `types`) - an out-of-range
    /// id returns `None`, same as an unexpandable `reference`.
    pub fn expand(&self, type_id: u32, reference: u64) -> Option<Vec<WasmTableEntry>> {
        let ty = self.types.borrow().get(type_id as usize)?.clone();
        let entries = self.session.expand(&ty, reference)?;
        Some(
            entries
                .into_iter()
                .map(|(label, entry_ty, word)| WasmTableEntry {
                    label,
                    type_name: debugger::type_name(&entry_ty),
                    type_id: self.register_type(&entry_ty),
                    value: self.session.render(&entry_ty, word).into(),
                })
                .collect(),
        )
    }

    /// U12 item 4's `evaluate`. See `debugger::DebugSession::evaluate`'s doc
    /// comment for the full "frame-scoped" architecture and why
    /// `debugSetVariable` has no equivalent.
    pub fn evaluate(&self, at: u32, expr_source: &str) -> WasmEvalResult {
        match self.session.evaluate(at as usize, expr_source) {
            Ok(value) => {
                let display = match value {
                    DisplayValue::Scalar(s) => s,
                    DisplayValue::Reference { summary, .. } => summary,
                };
                WasmEvalResult { ok: true, display }
            }
            Err(error) => WasmEvalResult {
                ok: false,
                display: error,
            },
        }
    }

    pub fn memory_stats(&self) -> WasmMemoryStats {
        let stats = self.session.memory_stats();
        WasmMemoryStats {
            live_bytes: stats.live_bytes as f64,
            live_blocks: stats.live_blocks as f64,
        }
    }

    /// See `DebugSession::force_gc`'s doc comment: safe to call, but a
    /// complete no-op in this jit-free execution context - calling this
    /// never changes what `memory_stats()` subsequently reports.
    pub fn force_gc(&self) {
        self.session.force_gc();
    }

    pub fn threads(&self) -> Vec<WasmThreadInfo> {
        self.session
            .threads()
            .into_iter()
            .map(|t| WasmThreadInfo {
                id: t.id,
                status: t.status,
            })
            .collect()
    }

    /// Runs `main()` again under profiling instrumentation (same one-shot
    /// convention as `run()` - see `DebugSession::profile`'s doc comment).
    pub fn profile(&self) -> Vec<WasmFunctionStats> {
        self.session
            .profile("main", &[])
            .into_iter()
            .map(|s| WasmFunctionStats {
                function_name: s.function_name,
                calls: s.calls as f64,
                self_instructions: s.self_instructions as f64,
                total_instructions: s.total_instructions as f64,
            })
            .collect()
    }

    /// Runs `main()` again recording a timeline (same one-shot convention as
    /// `run()`/`profile()` - see `DebugSession::record_timeline`'s doc
    /// comment).
    pub fn record_timeline(&self) -> Vec<WasmTimelineEvent> {
        self.session
            .record_timeline("main", &[])
            .into_iter()
            .map(|e| WasmTimelineEvent {
                kind: timeline_kind_str(e.kind).to_string(),
                function_name: e.function_name,
                depth: e.depth,
                step_index: e.step_index as u32,
            })
            .collect()
    }
}

// U12 item 6, deliverable 6: regression coverage over this file's own
// behavior. `#[wasm_bindgen]`-annotated items are still ordinary Rust items
// on a non-wasm32 target (the macro only changes codegen under
// `cfg(target_arch = "wasm32")`), so these run as plain native unit tests -
// `cargo test --manifest-path crate/sol/Cargo.toml --features wasm` (no
// `--target wasm32-unknown-unknown` needed; a real JS/wasm host is not
// required to exercise this module's own Rust logic).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_runs_a_simple_sol_program() {
        let result = execute("function main(): i64 return 1 + 2 end");
        assert_eq!(result.result(), Some("3".to_string()));
        assert_eq!(result.error(), None);
    }

    #[test]
    fn execute_lua_runs_with_captured_output() {
        let result = execute_lua("print('canonical lua')");
        assert_eq!(result.result(), Some("canonical lua\n".to_string()));
        assert_eq!(result.error(), None);
    }

    #[test]
    fn portable_lua_fixture_contract_matches_the_native_adapter() {
        for row in include_str!("../tests/wasm-portable.tsv").lines() {
            let fields = row.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 3, "invalid portable fixture row");
            let expected = fields[2].replace("\\n", "\n").replace("\\t", "\t");
            let result = execute_lua(fields[1]);
            assert_eq!(result.error(), None, "{}", fields[0]);
            assert_eq!(result.result().as_deref(), Some(expected.as_str()), "{}", fields[0]);
        }
    }

    #[test]
    fn execute_lua_preserves_output_on_error_and_denies_host_access() {
        let result = execute_lua("print(42); error('expected failure')");
        assert_eq!(result.result(), Some("42\n".into()));
        assert!(result.error().unwrap().contains("expected failure"));
        for source in ["os.execute('true')", "io.open('/etc/passwd')"] {
            assert!(execute_lua(source).error().is_some(), "{source}");
        }
        let denied = execute_lua("local f, err = package.loadlib('/tmp/library.so', 'entry'); print(f == nil, err ~= nil)");
        assert_eq!(denied.result(), Some("true\ttrue\n".into()));
    }

    #[test]
    fn lua_project_rejects_escaping_duplicate_and_ambiguous_paths() {
        for names in [vec!["main.lua", "../escape.lua"], vec!["main.lua", "main.lua"],
            vec!["main.lua", "math/base.lua", "math.base.lua"]] {
            let names = names.into_iter().map(String::from).collect::<Vec<_>>();
            let contents = vec!["print(42)".to_string(); names.len()];
            assert!(execute_lua_project("main.lua".into(), names, contents).error().is_some());
        }
    }

    #[test]
    fn execute_lua_project_loads_an_in_memory_module() {
        let result = execute_lua_project(
            "main.lua".to_string(),
            vec!["main.lua".to_string(), "math/base.lua".to_string()],
            vec![
                "local base = require('math.base'); print(base.answer)".to_string(),
                "return { answer = 42 }".to_string(),
            ],
        );
        assert_eq!(result.result(), Some("42\n".to_string()));
        assert_eq!(result.error(), None);
    }

    #[test]
    fn execute_reports_a_compile_error_without_panicking() {
        let result = execute("function main(): i64 return \"oops\" end");
        assert_eq!(result.result(), None);
        assert!(result.error().is_some());
    }

    #[test]
    fn execute_reports_a_raised_runtime_error() {
        let result = execute(
            "function main(): i64
                 local a: i64 = 1
                 local b: i64 = 0
                 return a / b
             end",
        );
        assert_eq!(result.result(), None);
        assert!(result.error().is_some());
    }

    #[test]
    fn execute_project_resolves_nested_in_memory_sol_imports() {
        let result = execute_project(
            "main.sol".to_string(),
            vec!["main.sol".to_string(), "math/base.sol".to_string()],
            vec![
                "import math.base\nfunction main(): i64 return math.base.answer() end".to_string(),
                "export function answer(): i64 return 42 end".to_string(),
            ],
        );
        assert_eq!(result.result(), Some("42".to_string()));
        assert_eq!(result.error(), None);
    }

    #[test]
    fn execute_project_rejects_paths_outside_its_in_memory_root() {
        let result = execute_project(
            "../main.sol".to_string(),
            vec!["../main.sol".to_string()],
            vec!["function main(): i64 return 42 end".to_string()],
        );
        assert_eq!(result.result(), None);
        assert!(result.error().unwrap().contains("must not escape"));
    }

    #[test]
    fn debug_project_session_runs_and_breaks_inside_an_imported_module() {
        let session = WasmDebugSession::launch_project(
            "main.sol".to_string(),
            vec!["main.sol".to_string(), "math/base.sol".to_string()],
            vec![
                "import math.base\nfunction main(): i64 return math.base.answer() end".to_string(),
                "export function answer(): i64 return 42 end".to_string(),
            ],
        )
        .expect("in-memory project compiles");
        let breakpoint = session.set_breakpoint("math.base.answer", 1);
        assert!(breakpoint.verified());
        assert_eq!(session.run().result(), Some("42".to_string()));
        assert!(session.first_breakpoint_hit().is_some());
    }

    #[test]
    fn debug_session_runs_and_reports_breakpoints_and_locals() {
        let session = WasmDebugSession::launch(
            "function main(): i64
                 local total: i64 = 0
                 local i: i64 = 0
                 while i < 3 do
                     total = total + i
                     i = i + 1
                 end
                 return total
             end",
        )
        .expect("fixture compiles and builds a session");

        let bp = session.set_breakpoint("main", 5);
        assert!(
            bp.verified(),
            "breakpoint on an executable line should verify"
        );

        let run_result = session.run();
        assert_eq!(run_result.result(), Some("3".to_string()));
        assert_eq!(run_result.error(), None);

        assert!(session.trace_length() > 0);
        assert!(session.trace_step(0).is_some());
        assert!(session.trace_step(session.trace_length() + 1000).is_none());

        let first_hit = session
            .first_breakpoint_hit()
            .expect("the loop body line should be hit at least once");
        let locals = session
            .locals_at(first_hit)
            .expect("a valid trace index always has locals");
        assert!(!locals.is_empty());
        assert!(locals.iter().any(|l| l.type_name() == "i64"));

        // Stepping/continuing should stay within trace bounds and never panic.
        let _ = session.step_into(first_hit);
        let _ = session.step_over(first_hit);
        let _ = session.step_out(first_hit);
        let _ = session.continue_to_breakpoint(first_hit);

        let eval = session.evaluate(first_hit, "1 + 1");
        assert!(
            eval.ok(),
            "evaluating a literal expression should succeed: {}",
            eval.display()
        );
        assert_eq!(eval.display(), "2");

        let stats = session.memory_stats();
        assert!(stats.live_bytes() >= 0.0);
        session.force_gc();

        let threads = session.threads();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].status(), "running");
    }

    #[test]
    fn wasm_breakpoint_lifecycle_and_filters_are_exposed() {
        let source = "function main(): i64
                 local i: i64 = 0
                 while i < 4 do
                     i = i + 1
                 end
                 return i
             end";
        let session = WasmDebugSession::launch(source).expect("fixture compiles");
        let breakpoint = session.set_breakpoint("main", 4);
        assert!(breakpoint.verified());
        assert!(breakpoint.id() > 0);
        assert!(session.set_breakpoint_condition(breakpoint.id(), Some("local0 == 2".to_string())));
        assert!(session.set_breakpoint_hit_condition(breakpoint.id(), Some(2)));
        session.run();
        assert!(session.first_breakpoint_hit().is_some());
        assert!(session.remove_breakpoint(breakpoint.id()));
        assert!(!session.remove_breakpoint(breakpoint.id()));
    }

    #[test]
    fn continue_burst_pages_through_the_trace_and_reports_exhaustion() {
        let session = WasmDebugSession::launch(
            "function main(): i64
                 local total: i64 = 0
                 local i: i64 = 0
                 while i < 50 do
                     total = total + i
                     i = i + 1
                 end
                 return total
             end",
        )
        .expect("fixture compiles");
        session.run();
        let total = session.trace_length();
        assert!(total > 0);

        // A tiny window should not claim exhaustion (unless the whole trace
        // really is that short).
        let first_burst = session.continue_burst(0, 1);
        assert!(!first_burst.stopped(), "no breakpoint was set");
        if total > 2 {
            assert!(!first_burst.exhausted());
        }

        // A window covering the whole trace must report exhaustion.
        let full_burst = session.continue_burst(0, total + 10);
        assert!(full_burst.exhausted());
        assert_eq!(full_burst.index(), total - 1);
    }

    #[test]
    fn expand_walks_an_array_reference() {
        let session = WasmDebugSession::launch(
            "function main(): i64
                 local xs = new_array_i64(3)
                 xs[0] = 10
                 xs[1] = 20
                 xs[2] = 30
                 return xs[0]
             end",
        )
        .expect("fixture compiles");
        session.run();
        let locals = session
            .locals_at(session.trace_length() - 1)
            .expect("last step has locals");
        let array_local = locals
            .iter()
            .find(|l| l.value().is_reference())
            .expect("the array local should render as a reference");
        let entries = session
            .expand(
                array_local.type_id(),
                array_local.value().reference().unwrap(),
            )
            .expect("an array reference should expand");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].value().scalar(), Some("10".to_string()));
        assert_eq!(entries[1].value().scalar(), Some("20".to_string()));
        assert_eq!(entries[2].value().scalar(), Some("30".to_string()));
    }

    #[test]
    fn profile_and_record_timeline_do_not_panic_and_see_main() {
        let session = WasmDebugSession::launch("function main(): i64 return 42 end")
            .expect("fixture compiles");
        let stats = session.profile();
        assert!(stats.iter().any(|s| s.function_name() == "main"));

        let timeline = session.record_timeline();
        assert!(!timeline.is_empty());
        assert!(timeline.iter().any(|e| e.function_name() == "main"));
    }
}
