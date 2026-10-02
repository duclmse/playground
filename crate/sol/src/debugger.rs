// U12 item 3: native, non-wasm Tier-0 debugger engine spike. See
// `docs/features/milestones/u12-wasm-playground.md`'s Work item 3 section
// for the full design rationale, the file:line evidence behind every
// decision below, and - just as importantly - the explicitly-scoped-out
// parts. This module has no Cranelift/`jit`-feature dependency (verified by
// `cargo check --no-default-features --target wasm32-unknown-unknown`) and
// knows nothing about wasm-bindgen or the browser wire protocol documented
// at `apps/web/src/debug-protocol.ts` - it only proves out the underlying
// native engine pieces that protocol will eventually have to call into.
//
// Design summary (see the milestone doc for the full writeup): Tier-0
// bytecode interpretation is deterministic and side-effect-free with
// respect to anything this spike's fixtures observe, so instead of
// implementing true interactive mid-execution suspend/resume (which would
// need either a coroutine/fiber runtime or rewriting `interp::Runtime` into
// an externally-driven state machine - a much larger change than an "engine
// spike" calls for), `DebugSession::run` executes the call once to
// completion, recording one `TraceStep` per bytecode instruction (deliverable
// 2's per-instruction hook). Breakpoint hits, stepping, and stack-frame
// inspection are then answered by indexing into that recorded trace, not by
// actually pausing a live interpreter. This is the single biggest
// intentional scope cut in this item - see "not done here" in the milestone
// doc.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::gc;
use crate::interp::Hooks;
use crate::tier0;
use crate::types::{self, StructLayout, TFunction, TProgram, Type};

// ---------------------------------------------------------------------
// Deliverable 3: breakpoint matching (sourceId+line -> verified/pending)
// ---------------------------------------------------------------------

/// One requested breakpoint. This spike compiles one function per "source"
/// (no multi-file project support here - see `modules.rs` for that, out of
/// scope), so `function_name` stands in for the wire protocol's `sourceId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBreakpoint {
    pub function_name: String,
    pub line: u32,
}

/// Deliverable 3's verdict. `verified: true` iff some bytecode instruction in
/// the compiled function actually maps to `line`, via the real
/// per-instruction `SourceMap` deliverable 1 built (`bccompile.rs`'s
/// `compile_function`) - a line with no mapped instruction (e.g. a blank
/// line, a comment, or a line whose statement optimize.rs folded away)
/// reports `verified: false`, exactly as item 3's spec requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBreakpoint {
    pub function_name: String,
    pub line: u32,
    pub verified: bool,
    /// The first bytecode pc whose mapped line equals `line`, if verified.
    pub pc: Option<u32>,
}

/// Scans `source_map` for the first pc whose mapped line equals `line`. A
/// linear scan is fine here: a function's instruction count is small enough
/// (Tier-0's register file is 8-bit, see `tier0.rs`) that this never shows up
/// as a hot path, and a sorted/binary-searchable map would assume
/// monotonically increasing lines, which isn't guaranteed (e.g. `for`
/// desugaring in `typeck.rs` can emit statements whose lines don't strictly
/// increase pc-over-pc).
pub fn verify_breakpoint(
    function_name: &str,
    line: u32,
    source_map: &sol_core::SourceMap,
) -> VerifiedBreakpoint {
    for pc in 0..source_map.len() as u32 {
        if let Some(loc) = source_map.location(pc) {
            if loc.line == line {
                return VerifiedBreakpoint {
                    function_name: function_name.to_string(),
                    line,
                    verified: true,
                    pc: Some(pc),
                };
            }
        }
    }
    VerifiedBreakpoint {
        function_name: function_name.to_string(),
        line,
        verified: false,
        pc: None,
    }
}

// ---------------------------------------------------------------------
// Deliverable 4: typed value display + lazy/paginated table expansion
// ---------------------------------------------------------------------

/// A local/upvalue's rendered form: either a self-contained scalar display
/// string, or a `reference`-style opaque handle (the raw pointer/payload
/// word itself - stable for the lifetime of the paused frame it came from)
/// a client can later ask `ValueRenderer::expand` to page through.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayValue {
    Scalar(String),
    Reference { reference: u64, summary: String },
}

/// Renders a raw register word as a `Value -> displayable string` using
/// `sol_core`/`value.rs`'s actual runtime representation (not a
/// reimplementation of it) - see each match arm's comment for the exact
/// layout it reads, cross-referenced against `interp.rs`'s own
/// `Op::Index`/`Op::GetField`/`Op::Box` handling of the same layouts.
pub struct ValueRenderer<'a> {
    pub structs: &'a HashMap<String, StructLayout>,
}

/// A short type-name string, used both as `Reference::summary`'s type tag
/// and for nested element/field types during `expand`.
pub fn type_name(ty: &Type) -> String {
    match ty {
        Type::I64 => "i64".to_string(),
        Type::F64 => "f64".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Nil => "nil".to_string(),
        Type::String => "string".to_string(),
        Type::Array(inner) => format!("Array<{}>", type_name(inner)),
        Type::Map(key, value) => format!("Map<{}, {}>", type_name(key), type_name(value)),
        Type::Function { .. } => "function".to_string(),
        Type::Struct(name) => name.clone(),
        Type::Any => "any".to_string(),
    }
}

impl<'a> ValueRenderer<'a> {
    pub fn render(&self, ty: &Type, raw: u64) -> DisplayValue {
        match ty {
            // Scalars: `interp.rs`'s register file is untagged - the static
            // type alone says how to reinterpret the raw `u64` word.
            Type::I64 => DisplayValue::Scalar((raw as i64).to_string()),
            Type::F64 => DisplayValue::Scalar(format!("{}", f64::from_bits(raw))),
            Type::Bool => DisplayValue::Scalar((raw != 0).to_string()),
            Type::Nil => DisplayValue::Scalar("nil".to_string()),
            // `strings.rs`: `[length: u64][bytes...]`, one pointer in SSA/a
            // register - see `strings::bytes`'s doc comment for the same
            // layout this reads.
            Type::String => {
                if raw == 0 {
                    DisplayValue::Scalar("nil".to_string())
                } else {
                    let bytes = unsafe { crate::strings::bytes(raw as *const u8) };
                    DisplayValue::Scalar(format!("{:?}", String::from_utf8_lossy(bytes)))
                }
            }
            Type::Array(inner) => DisplayValue::Reference {
                reference: raw,
                summary: format!("Array<{}>", type_name(inner)),
            },
            Type::Map(key, value) => DisplayValue::Reference {
                reference: raw,
                summary: format!("Map<{}, {}>", type_name(key), type_name(value)),
            },
            Type::Struct(name) => DisplayValue::Reference {
                reference: raw,
                summary: name.clone(),
            },
            Type::Function { .. } => DisplayValue::Reference {
                reference: raw,
                summary: "function".to_string(),
            },
            // Not done here: unboxing an `Any`-typed local's real tag back
            // into a type name needs a whole-program tag registry (the
            // reverse of `value::tag_for`), which this spike does not build
            // - see this module's doc comment and the milestone doc's "not
            // done here" list.
            Type::Any => DisplayValue::Reference {
                reference: raw,
                summary: "any".to_string(),
            },
        }
    }

    /// Lazily expands a `reference`-style handle into its named/indexed
    /// children - deliverable 4's "paginated expansion for tables."
    ///
    /// `Array`, `Struct`, and (U12 item 4) `Map` are supported: all three are
    /// simple, pointer-stable C layouts this spike can read directly
    /// (`Array`: `interp.rs`'s `Op::Index` reads `{len: i64, data: *const
    /// u8}` then `data[index*8..]`; `Struct`: `Op::GetField` reads
    /// `*(base as *const u64).add(field_index)`, no header; `Map`: U12 item
    /// 4 adds `runtime.rs`'s `MapI64Header::entries`, a `pub(crate)` reader
    /// over the same occupied-bitmap-filtered open-addressed table
    /// `sol_map_get_i64`/`sol_map_set_i64` already read/write, closing the
    /// gap item 3 left open - see this module's doc comment history and
    /// `docs/features/milestones/u12-wasm-playground.md`'s Work item 4
    /// section). Each entry's label is the key rendered via this same
    /// renderer (per `typeck.rs`'s `lower_type`, a `Map`'s key type is
    /// always `I64` in the current M10 slice, so this always renders as a
    /// plain decimal integer in practice, but goes through `render` rather
    /// than hand-formatting to stay correct if that restriction loosens).
    /// `Any`'s reverse tag-to-type lookup still needs the whole-program tag
    /// registry noted above - still explicitly not done here.
    pub fn expand(&self, ty: &Type, reference: u64) -> Option<Vec<(String, Type, u64)>> {
        if reference == 0 {
            return Some(Vec::new());
        }
        match ty {
            Type::Array(inner) => {
                let header = reference as *const i64;
                let (len, data) = unsafe { (*header as u64, *(header.add(1)) as *const u8) };
                Some(
                    (0..len)
                        .map(|index| {
                            let word =
                                unsafe { *(data.add(index as usize * 8) as *const u64) };
                            (index.to_string(), (**inner).clone(), word)
                        })
                        .collect(),
                )
            }
            Type::Struct(name) => {
                let layout = self.structs.get(name)?;
                Some(
                    layout
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(field_index, (field_name, field_ty))| {
                            let word =
                                unsafe { *((reference as *const u64).add(field_index)) };
                            (field_name.clone(), field_ty.clone(), word)
                        })
                        .collect(),
                )
            }
            Type::Map(key, value) => {
                let header = reference as *const crate::runtime::MapI64Header;
                let raw_entries = unsafe { crate::runtime::MapI64Header::entries(header) };
                Some(
                    raw_entries
                        .into_iter()
                        .map(|(k, v)| {
                            let label = match self.render(key, k as u64) {
                                DisplayValue::Scalar(s) => s,
                                DisplayValue::Reference { summary, .. } => summary,
                            };
                            (label, (**value).clone(), v as u64)
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------
// U12 item 4, deliverable 1: frame-scoped expression evaluation
// ---------------------------------------------------------------------
//
// Architectural call this deliverable has to make honestly: item 3's
// `DebugSession` has no live paused frame to evaluate against at all - "pause"
// there just means "an index into an already-fully-recorded trace" (see this
// module's top doc comment). So "frame-scoped" here cannot mean "run the
// expression inside the paused interpreter, sharing its live register file
// and heap" (the milestone plan's own wording assumed a true pause/resume
// engine, which item 3 explicitly did not build). What it *can* mean, and
// what `evaluate` below actually implements, is: take the recorded
// `TraceStep::regs` snapshot at the requested trace index as a fixed,
// frozen set of inputs, and interpret the expression as a pure function of
// those inputs alone, via a second, independent, throwaway `tier0::Engine`.
// This deliberately does *not* create a second GC-root domain in the sense
// the plan worried about (there is no live heap to double-root - a snapshot
// register word for a non-scalar local is just copied as an opaque `u64`,
// never dereferenced by eval unless the expression itself indexes into it),
// but it also means an eval expression cannot observe or cause any
// side effect on the real run (no `debugSetVariable` - see below) and cannot
// call back into the real program's other functions (the synthetic program
// contains only the one function being evaluated).
//
// `debugSetVariable` is explicitly out of scope, not silently dropped: a
// `TraceStep`'s `regs` are a copy already computed by the one completed run
// this session recorded (`TraceRecorder::on_instruction` pushes a snapshot,
// it does not hold a mutable live view - see its definition below), so
// mutating one recorded step's copy cannot retroactively change what later
// steps (already recorded, from the original unmodified execution) computed.
// Supporting "set then re-run from here" would require the true
// externally-driven re-entrant interpreter the milestone plan and item 3
// both flagged as out of scope for this spike - a real, larger feature, not
// a gap to paper over with a snapshot mutation that wouldn't actually affect
// subsequent steps.

/// Sol source-syntax spelling of `ty`, usable as a parameter/return type
/// annotation in a freshly synthesized program - `None` for every type
/// `evaluate` does not expose to eval expressions at all (see
/// `evaluate`'s doc comment: only scalar locals are exposed as synthetic
/// parameters, and only a scalar result type is accepted back).
fn eval_type_annotation(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::I64 => Some("i64"),
        Type::F64 => Some("f64"),
        Type::Bool => Some("bool"),
        Type::Nil => Some("nil"),
        Type::String => Some("string"),
        Type::Array(_) | Type::Map(_, _) | Type::Function { .. } | Type::Struct(_) | Type::Any => {
            None
        }
    }
}

/// `evaluate`'s synthetic parameter name for local `id` - matches
/// `locals_at`'s own "no source name, id-only" convention (`TStmt::Local`
/// carries no name - see `locals_at`'s doc comment), so an expression typed
/// against a `LocalView::local_id` an earlier `locals_at` call reported uses
/// this exact spelling.
fn eval_param_name(id: usize) -> String {
    format!("local{id}")
}

/// Finds `block`'s first `return <expr>`'s real (pre-coercion) type, by a
/// small recursive walk that also looks inside `If` arms (defensive: a
/// single `return <expr>` function body never actually needs the `If` case,
/// but `check_stmt` is free to wrap statements in ways that don't change
/// this). See `evaluate`'s doc comment for why discovering this type is a
/// whole compile pass of its own: `typeck.rs::coerce`'s only way to go from
/// a concrete type to `any` is to wrap the real expression in `TExprKind::Box`
/// (unless the expression was already `any`, which cannot happen here since
/// `evaluate` never exposes an `Any`-typed local as a synthetic parameter -
/// see `eval_type_annotation`), so peeling that one `Box` node back off
/// recovers the expression's true static type.
fn eval_find_return_type(block: &types::TBlock) -> Option<Type> {
    for (_, stmt) in block {
        match stmt {
            types::TStmt::Return { value: Some(expr) } => {
                return Some(match &expr.kind {
                    types::TExprKind::Box(inner) => inner.ty.clone(),
                    _ => expr.ty.clone(),
                });
            }
            types::TStmt::If {
                then_block,
                else_block,
                ..
            } => {
                if let Some(ty) = eval_find_return_type(then_block) {
                    return Some(ty);
                }
                if let Some(ty) = eval_find_return_type(else_block) {
                    return Some(ty);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses+typechecks `source` through the same front-end pipeline
/// `lib.rs::compile_program_with_config` runs for Sol source
/// (`aliases::expand` -> `closures::lower` -> `typeck::check` ->
/// `verify::verify` -> `optimize::optimize` -> `verify::verify` ->
/// `escape::scalar_replace` -> `verify::verify`), minus `lib.rs`'s own
/// "a function named `main`, with zero parameters, with a CLI-printable
/// return type" requirement (`compile()`/`compile_program_with_config`
/// enforce that on the whole program via `lib.rs`'s wrapper - see
/// `evaluate`'s doc comment for why that makes `lib.rs`'s convenience entry
/// points unusable for synthesizing an arbitrarily-named, parameterized eval
/// function). Called directly against `parser`/`typeck`/etc., not through
/// `lib.rs`, for exactly that reason.
fn eval_compile(source: &str) -> Result<TProgram, String> {
    let tokens =
        crate::lexer::lex_bytes(source.as_bytes()).map_err(|e| format!("eval: lex error: {e}"))?;
    let mut program =
        crate::parser::parse(tokens).map_err(|e| format!("eval: parse error: {e}"))?;
    crate::aliases::expand(&mut program)
        .map_err(|e| format!("eval: alias expansion error: {e}"))?;
    crate::closures::lower(&mut program)
        .map_err(|e| format!("eval: closure lowering error: {e}"))?;
    let mut tprogram =
        crate::typeck::check(&program, false).map_err(|e| format!("eval: type error: {e}"))?;
    crate::verify::verify(&tprogram).map_err(|e| format!("eval: verify error: {e}"))?;
    crate::optimize::optimize(&mut tprogram);
    crate::verify::verify(&tprogram)
        .map_err(|e| format!("eval: verify error (post-optimize): {e}"))?;
    crate::escape::scalar_replace(&mut tprogram);
    crate::verify::verify(&tprogram)
        .map_err(|e| format!("eval: verify error (post-escape): {e}"))?;
    Ok(tprogram)
}

// ---------------------------------------------------------------------
// Deliverables 2/5: per-instruction trace recording + stepping
// ---------------------------------------------------------------------

/// One recorded instruction visit - deliverable 2's per-instruction hook
/// output, captured so `DebugSession` can step through a full run after the
/// fact (see this module's doc comment for why).
#[derive(Debug, Clone)]
pub struct TraceStep {
    pub func_id: u8,
    pub pc: u32,
    pub line: u32,
    /// Call depth at this instant: starts at 1 for the session's own
    /// top-level call (incremented by `on_call_enter`, decremented by
    /// `on_call_exit` - see `interp::Runtime::call_outcome`), so a step over
    /// a nested non-tail call sees a strictly greater depth for every
    /// instruction inside it. Tail calls reuse the caller's depth (they
    /// reuse its frame - `interp::Runtime::dispatch`'s loop never recurses
    /// for a tail call), matching real tail-call semantics.
    pub depth: u32,
    /// The whole register file at this exact instant, copied (not a live
    /// view - each step must see its own point-in-time values).
    pub regs: Vec<u64>,
}

#[derive(Default)]
pub struct TraceRecorder {
    steps: RefCell<Vec<TraceStep>>,
    depth: Cell<u32>,
}

impl TraceRecorder {
    fn reset(&self) {
        self.steps.borrow_mut().clear();
        self.depth.set(0);
    }
}

impl Hooks for TraceRecorder {
    fn on_call_enter(&self, _func_id: u8, _args: &[u64]) {
        self.depth.set(self.depth.get() + 1);
    }

    fn on_call_exit(&self, _func_id: u8, _result: u64) {
        self.depth.set(self.depth.get().saturating_sub(1));
    }

    fn on_instruction(&self, func_id: u8, pc: u32, line: u32, regs: &[u64]) {
        self.steps.borrow_mut().push(TraceStep {
            func_id,
            pc,
            line,
            depth: self.depth.get(),
            regs: regs.to_vec(),
        });
    }
}

/// Deliverable 5: a standalone, testable Rust API wrapping `tier0::Engine`
/// with breakpoints, a recorded trace, locals rendering, and stepping. No
/// wasm-bindgen, no browser, no worker - a native Rust type exercised by a
/// native `#[cfg(test)]` harness (see this file's test module) and the
/// differential tests in `crate/sol/tests/debugger.rs` (deliverable 6).
pub struct DebugSession {
    program: TProgram,
    engine: tier0::Engine<TraceRecorder>,
    /// `(func_id, pc)` pairs for every breakpoint that verified - internal
    /// matching key against `TraceStep`; `VerifiedBreakpoint` (the public
    /// report) carries the function name instead, since `func_id` is this
    /// session's own internal numbering.
    breakpoints: RefCell<Vec<(u8, u32, VerifiedBreakpoint)>>,
}

impl DebugSession {
    pub fn new(program: TProgram) -> Result<Self, String> {
        let engine = tier0::Engine::new(program.clone(), TraceRecorder::default())?;
        Ok(Self {
            program,
            engine,
            breakpoints: RefCell::new(Vec::new()),
        })
    }

    /// `name`'s numeric `func_id`, as recorded in every `TraceStep`. Exposed
    /// so a caller (e.g. a differential test disambiguating a nested-call
    /// trace by which function each step belongs to) can match `trace()`
    /// entries against a specific function without reaching into the
    /// session's private `tier0::Engine`.
    pub fn function_id(&self, name: &str) -> Option<u8> {
        self.engine.function_id(name)
    }

    /// Deliverable 3. Verifies against the exact `SourceMap` this session's
    /// engine is executing (via `tier0::Engine::function_bytecode`), so a
    /// verified pc is guaranteed consistent with what `trace()` later
    /// records.
    pub fn set_breakpoint(&self, function_name: &str, line: u32) -> VerifiedBreakpoint {
        let verified = match self.engine.function_bytecode(function_name) {
            Some(bf) => verify_breakpoint(function_name, line, &bf.source_map),
            None => VerifiedBreakpoint {
                function_name: function_name.to_string(),
                line,
                verified: false,
                pc: None,
            },
        };
        if let (true, Some(func_id), Some(pc)) = (
            verified.verified,
            self.engine.function_id(function_name),
            verified.pc,
        ) {
            self.breakpoints
                .borrow_mut()
                .push((func_id, pc, verified.clone()));
        }
        verified
    }

    /// Runs `function_name(args)` to completion, recording a full
    /// instruction-level trace. Call once per session (a fresh `DebugSession`
    /// per run keeps traces from mixing); `trace()`/`locals_at()`/`step_*()`
    /// all read back from the trace this produces.
    pub fn run(&self, function_name: &str, args: &[u64]) -> sol_core::CallOutcome<u64, String> {
        self.engine.hooks().reset();
        self.engine.call_outcome(function_name, args)
    }

    pub fn trace(&self) -> Vec<TraceStep> {
        self.engine.hooks().steps.borrow().clone()
    }

    /// Deliverable 6: the first trace index whose `(func_id, pc)` matches a
    /// verified breakpoint - "continue" (from the very start) to the first
    /// breakpoint hit.
    pub fn first_breakpoint_hit(&self) -> Option<usize> {
        let trace = self.trace();
        let breakpoints = self.breakpoints.borrow();
        trace.iter().position(|step| {
            breakpoints
                .iter()
                .any(|&(func_id, pc, _)| func_id == step.func_id && pc == step.pc)
        })
    }

    /// "Continue" from a paused trace index to the next breakpoint hit
    /// strictly after it.
    pub fn continue_to_breakpoint(&self, from: usize) -> Option<usize> {
        let trace = self.trace();
        let breakpoints = self.breakpoints.borrow();
        trace.iter().enumerate().skip(from + 1).find_map(|(i, step)| {
            breakpoints
                .iter()
                .any(|&(func_id, pc, _)| func_id == step.func_id && pc == step.pc)
                .then_some(i)
        })
    }

    /// Step into: the next instruction whose line differs from `from`'s,
    /// whatever call depth it's at - so stepping into a call that
    /// immediately starts a new line stops right there.
    pub fn step_into(&self, from: usize) -> Option<usize> {
        let trace = self.trace();
        let current = trace.get(from)?;
        trace
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, step)| step.line != current.line || step.func_id != current.func_id)
            .map(|(i, _)| i)
    }

    /// Step over: the next instruction at a call depth no deeper than
    /// `from`'s, with a different line - skips entirely over any nested
    /// (non-tail) call's own instructions.
    pub fn step_over(&self, from: usize) -> Option<usize> {
        let trace = self.trace();
        let current = trace.get(from)?;
        trace
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, step)| {
                step.depth <= current.depth
                    && (step.line != current.line || step.func_id != current.func_id)
            })
            .map(|(i, _)| i)
    }

    /// Step out: the next instruction at a strictly shallower call depth
    /// than `from`'s - i.e. finishes the current (non-tail) call and lands
    /// on the caller's next instruction.
    pub fn step_out(&self, from: usize) -> Option<usize> {
        let trace = self.trace();
        let current = trace.get(from)?;
        trace
            .iter()
            .enumerate()
            .skip(from + 1)
            .find(|(_, step)| step.depth < current.depth)
            .map(|(i, _)| i)
    }

    /// Deliverable 4: every named local visible at trace index `at`,
    /// rendered via `ValueRenderer`. Locals carry no source-level name past
    /// typed lowering (`TStmt::Local` only has a numeric `LocalId` - see
    /// `types.rs`; there is no name table), so each is labeled by its id;
    /// params are flagged separately. The typed tier has no separate
    /// "upvalue" concept to report alongside these: non-escaping closures
    /// are lambda-lifted away and escaping captures become ordinary
    /// struct-typed locals before this point (`escape.rs`), so by the time a
    /// function reaches Tier-0 bytecode, captured state already *is* a
    /// local - see the milestone doc's Work item 3 section for this finding
    /// in full.
    pub fn locals_at(&self, at: usize) -> Option<Vec<LocalView>> {
        let trace = self.trace();
        let step = trace.get(at)?;
        let function = self.function_by_id(step.func_id)?;
        let types = types::collect_local_types(function);
        let is_param: std::collections::HashSet<usize> =
            function.params.iter().map(|(id, _)| *id).collect();
        let renderer = ValueRenderer {
            structs: &self.program.structs,
        };
        Some(
            (0..function.local_count)
                .map(|id| {
                    let ty = types[id].clone();
                    let raw = step.regs.get(id).copied().unwrap_or(0);
                    LocalView {
                        local_id: id,
                        is_param: is_param.contains(&id),
                        ty: ty.clone(),
                        type_name: type_name(&ty),
                        value: renderer.render(&ty, raw),
                    }
                })
                .collect(),
        )
    }

    /// Deliverable 4's "lazy/paginated expansion" entry point for a
    /// previously-rendered `reference` handle.
    pub fn expand(&self, ty: &Type, reference: u64) -> Option<Vec<(String, Type, u64)>> {
        let renderer = ValueRenderer {
            structs: &self.program.structs,
        };
        renderer.expand(ty, reference)
    }

    /// U12 item 4, deliverable 1: evaluates a standalone Sol expression
    /// against the scalar locals recorded at trace index `at` - see this
    /// module's "deliverable 1" section comment above for the full
    /// architectural rationale (what "frame-scoped" can mean here, and why
    /// `debugSetVariable` is out of scope).
    ///
    /// Implementation: a two-pass synthetic-program compile. Every
    /// scalar-typed local (`I64`/`F64`/`Bool`/`Nil`/`String` -
    /// `eval_type_annotation`) visible in the paused function becomes a
    /// same-named, same-typed parameter (`local{id}`) of a throwaway
    /// `__sol_eval` function whose body is just `return <expr_source>`;
    /// non-scalar locals (`Array`/`Map`/`Struct`/`Function`/`Any`) are not
    /// exposed at all - referencing one by name simply fails to typecheck
    /// as an undefined variable, a real but bounded, explicitly-documented
    /// gap (exposing them would additionally require reconstructing the
    /// real program's struct declarations, and/or a reverse `Any`-tag
    /// registry, inside the throwaway program - out of scope here, same as
    /// item 3's `ValueRenderer` leaving `Any` unboxing undone). Pass 1
    /// declares the synthetic function's return type as `any` to discover
    /// the expression's real type (`eval_find_return_type` peels the
    /// `Box` node `typeck.rs::coerce` always inserts to get there); pass 2
    /// recompiles with that real type declared directly, so the interpreter
    /// never has to unbox an `any` at all. The recorded register words for
    /// the chosen locals are then passed as the call's raw `u64` arguments
    /// to a fresh, one-off `tier0::Engine` - entirely independent of this
    /// session's own `engine`/heap (see the architecture note above: this is
    /// why it does not create a second GC-root domain in the sense the
    /// milestone plan worried about).
    ///
    /// Known hazard, inherited rather than specially handled: like any
    /// Tier-0 execution, a trapping operation (e.g. integer division by
    /// zero) calls `trap()` (`std::process::abort()` - see `gc.rs`/`interp.rs`
    /// doc comments elsewhere), which aborts the whole process, not just this
    /// call. `evaluate` does not sandbox against this; doing so (e.g. running
    /// the synthetic engine in a subprocess) is a real, separate feature, not
    /// a gap specific to eval - the exact same hazard already exists for
    /// `DebugSession::run` itself.
    pub fn evaluate(&self, at: usize, expr_source: &str) -> Result<DisplayValue, String> {
        let trace = self.trace();
        let step = trace
            .get(at)
            .ok_or_else(|| format!("eval: no trace step at index {at}"))?;
        let function = self
            .function_by_id(step.func_id)
            .ok_or("eval: trace step references an unknown function")?;
        let local_types = types::collect_local_types(function);

        let params: Vec<(usize, Type)> = local_types
            .iter()
            .enumerate()
            .filter(|(_, ty)| eval_type_annotation(ty).is_some())
            .map(|(id, ty)| (id, ty.clone()))
            .collect();
        let param_list = params
            .iter()
            .map(|(id, ty)| {
                format!(
                    "{}: {}",
                    eval_param_name(*id),
                    eval_type_annotation(ty).expect("filtered above")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        let synthesize = |return_ty: &str| -> String {
            format!("function __sol_eval({param_list}): {return_ty}\n    return {expr_source}\nend")
        };

        // Pass 1: discover the expression's real (pre-`any`-coercion) type.
        let probe = eval_compile(&synthesize("any"))?;
        let probe_fn = probe
            .functions
            .iter()
            .find(|f| f.name == "__sol_eval")
            .expect("eval_compile's source always declares __sol_eval");
        let real_type = eval_find_return_type(&probe_fn.body)
            .ok_or("eval: expression produced no return value")?;
        let return_annotation = eval_type_annotation(&real_type).ok_or_else(|| {
            format!(
                "eval: expression result type `{}` is not a supported eval result type (only i64/f64/bool/nil/string are)",
                type_name(&real_type)
            )
        })?;

        // Pass 2: recompile with the real type declared directly.
        let program = eval_compile(&synthesize(return_annotation))?;
        let engine: tier0::Engine =
            tier0::Engine::new(program, ()).map_err(|e| format!("eval: tier0 engine build failed: {e}"))?;

        let args: Vec<u64> = params
            .iter()
            .map(|(id, _)| step.regs.get(*id).copied().unwrap_or(0))
            .collect();

        match engine.call_outcome("__sol_eval", &args) {
            sol_core::CallOutcome::Returned(values) => {
                let raw = values.into_iter().next().unwrap_or(0);
                let renderer = ValueRenderer {
                    structs: &self.program.structs,
                };
                Ok(renderer.render(&real_type, raw))
            }
            sol_core::CallOutcome::Raised(error) => Err(format!("eval: expression raised: {error}")),
            other => Err(format!("eval: unexpected call outcome: {other:?}")),
        }
    }

    /// U12 item 4, deliverable 3: `gc::live_bytes()`/`gc::live_blocks()`,
    /// exposed through `DebugSession`'s own public API rather than making a
    /// caller reach into `gc` directly.
    ///
    /// **Honesty note, verified against `gc.rs` directly**: these two
    /// numbers are meaningful and safe to call in a jit-free
    /// `DebugSession` context - they are simple bookkeeping sums over the
    /// heap's bump-arena chunks (`live_bytes`: payload bytes carved out;
    /// `live_blocks`: allocation-record count), unrelated to reachability,
    /// so they never depend on `STACK_BASE`/conservative stack scanning at
    /// all. See `force_gc`'s doc comment for the very different story for
    /// `collect()` itself.
    pub fn memory_stats(&self) -> MemoryStats {
        MemoryStats {
            live_bytes: gc::live_bytes(),
            live_blocks: gc::live_blocks(),
        }
    }

    /// U12 item 4, deliverable 3: forces a GC cycle via `gc::collect()`.
    ///
    /// **Honesty note, verified against `gc.rs` directly**: `collect()` is
    /// *safe* to call here (it is a plain, non-panicking function call -
    /// `DebugSession` never crashes from calling it), but it is a **complete
    /// no-op** in this context specifically. `gc::collect_heap`/
    /// `collect_minor` both check `STACK_BASE == 0` and return immediately,
    /// before even scanning `EXTRA_ROOTS` (the interpreter's own
    /// register-file GC roots - see `gc.rs`'s `RootGuard`), and
    /// `STACK_BASE` is only ever initialized by `gc::init_stack_base()`,
    /// which only the native JIT/AOT entry path calls (`main.rs`/`aot.rs`) -
    /// `tier0::Engine`/`interp::Runtime` (what every `DebugSession` actually
    /// runs on) never calls it. So calling `force_gc` from a `DebugSession`
    /// reclaims nothing, ever, including genuinely unreachable garbage -
    /// `memory_stats()` before and after a `force_gc()` call will report the
    /// same numbers. This is a real, pre-existing property of the jit-free
    /// execution path, not something item 4 introduces or could paper over
    /// without giving Tier-0 its own stack-scanning root set (a real,
    /// separate feature - out of scope here).
    pub fn force_gc(&self) {
        gc::collect();
    }

    fn function_by_id(&self, func_id: u8) -> Option<&TFunction> {
        self.program
            .functions
            .iter()
            .find(|f| self.engine.function_id(&f.name) == Some(func_id))
    }

    // -------------------------------------------------------------------
    // U12 item 5: profiling, execution timeline, and the coroutine/thread
    // scope boundary. See
    // `docs/features/milestones/u12-wasm-playground.md`'s Work item 5
    // section for the full design rationale and the coroutine-scope finding
    // this documents rather than papers over.
    // -------------------------------------------------------------------

    /// U12 item 5: `debugGetThreads`-equivalent. **Always reports exactly
    /// one thread, the main thread - this is not real multi-thread/coroutine
    /// support, and is not meant to look like it.**
    ///
    /// Checked directly before writing this (not assumed): grepping
    /// `coroutine|Coroutine` across `crate/sol/src/*.rs` outside
    /// `lua_runtime/*`/`lua_bytecode/*` turns up only `interp.rs:795-863` (a
    /// mixed-module test helper that calls into the separate *dynamic*
    /// `.lua` tier's own coroutine machinery via the FFI bridge -
    /// `create_global_coroutine`/`resume_coroutine_outcome` - not a
    /// typed-tier primitive) and a `main.rs` comment/error message about
    /// yielding outside a coroutine, also part of the dynamic/mixed-module
    /// path. The typed `.sol` language and its Tier-0 bytecode interpreter
    /// (`interp.rs`'s dispatch loop, `bccompile.rs`, `types.rs`) have **no
    /// coroutine/thread concept of their own at all** - "coroutine" in this
    /// codebase is exclusively a dynamic-`.lua`-tier concept
    /// (`lua_runtime/*`), confirmed separately by
    /// `docs/phase-4-8-implementation.md`'s own coroutine-debugging examples
    /// all being `.lua` syntax (`coroutine.create`/`coroutine.resume`).
    /// `DebugSession` only ever runs a typed `TProgram` through
    /// `tier0::Engine` (item 3's own scope boundary, unchanged by this item)
    /// - it never touches `lua_runtime`'s dynamic interpreter or its
    /// coroutine machinery at all.
    ///
    /// So `debugGetThreads`/per-thread stack scoping has no applicable
    /// target in this spike: there is nothing to "confirm is walkable
    /// per-thread," because the typed tier this `DebugSession` wraps is
    /// single-threaded *by construction* - `tier0::Engine::call`/
    /// `call_outcome` is one synchronous Rust call, with no suspended
    /// coroutine state to enumerate, ever. Real per-thread stack scoping is
    /// simply not implemented here, not silently dropped - adding it for a
    /// tier that cannot suspend a thread at all would mean inventing a stub
    /// to satisfy the plan's wording, which this item deliberately does not
    /// do.
    ///
    /// This accessor exists anyway, as a small, honestly-scoped addition: a
    /// constant, single-element "main thread" report, for wire-protocol
    /// *shape* compatibility with `apps/web/src/debug-protocol.ts`'s
    /// `ThreadInfo` (`id`, `status`) if a later item wants one - not a
    /// disguised stub pretending to support suspended or multiple threads.
    pub fn threads(&self) -> Vec<ThreadInfo> {
        vec![ThreadInfo {
            id: 0,
            status: "running".to_string(),
        }]
    }

    /// U12 item 5: one-shot profiling run. Resets and re-runs
    /// `function_name(args)` (same recording mechanism as `run`), then
    /// aggregates the resulting trace into per-function `FunctionStats` -
    /// reusing item 3's `TraceStep`/`Hooks` recording machinery in "record
    /// everything, don't pause" mode, per the plan's own suggested approach,
    /// rather than building a second instrumentation path. Like `run`, call
    /// once per session - a fresh `DebugSession` per profiling run keeps this
    /// run's trace from mixing with a separate stepping/eval session's own.
    ///
    /// Attribution algorithm: walks the trace once, reconstructing the call
    /// stack (`Vec<func_id>`, one entry per active depth) purely from each
    /// step's own `depth`/`func_id` fields (no second recorded pass): a step
    /// whose depth is one deeper than the stack's current height pushes a
    /// new frame (and charges one `calls` to that function - this includes
    /// the session's own top-level call, which always gets exactly one); a
    /// step whose depth is shallower truncates the stack back down (no new
    /// call charged - this is just continuing an already-counted ancestor
    /// frame after a nested call returned); a step at the *same* depth as
    /// the stack's current top but with a *different* `func_id` is a tail
    /// call (`TraceStep::depth`'s own doc comment: tail calls reuse the
    /// caller's depth) - the old frame is replaced in place and the new
    /// function is charged one `calls`. Every step then adds one to
    /// `self_instructions` for its own `func_id` alone, and one to
    /// `total_instructions` for *every* function currently on the
    /// reconstructed stack (itself plus every live ancestor). Consequences
    /// worth stating plainly: a function never called gets no entry at all;
    /// a leaf function's `total_instructions` always equals its
    /// `self_instructions`; the top-level function's `total_instructions`
    /// always equals the whole trace's length; and summing
    /// `self_instructions` across every returned entry always equals the
    /// trace's length too (every instruction belongs to exactly one frame's
    /// self time) - all four are exercised directly, not just asserted in
    /// prose, by `tests/debugger.rs`'s profiling test.
    pub fn profile(&self, function_name: &str, args: &[u64]) -> Vec<FunctionStats> {
        self.run(function_name, args);
        let trace = self.trace();

        let mut stack: Vec<u8> = Vec::new();
        let mut calls: HashMap<u8, u64> = HashMap::new();
        let mut self_instructions: HashMap<u8, u64> = HashMap::new();
        let mut total_instructions: HashMap<u8, u64> = HashMap::new();

        for step in &trace {
            let depth = step.depth as usize;
            if depth == 0 {
                // Defensive: every recorded instruction is inside some call
                // (depth starts at 1 for the session's own top-level call -
                // see `TraceStep::depth`'s doc comment), so this never
                // actually fires.
                continue;
            }
            if stack.len() < depth {
                stack.push(step.func_id);
                *calls.entry(step.func_id).or_insert(0) += 1;
            } else {
                if stack.len() > depth {
                    stack.truncate(depth);
                }
                if stack[depth - 1] != step.func_id {
                    stack[depth - 1] = step.func_id;
                    *calls.entry(step.func_id).or_insert(0) += 1;
                }
            }
            *self_instructions.entry(step.func_id).or_insert(0) += 1;
            for &func_id in &stack {
                *total_instructions.entry(func_id).or_insert(0) += 1;
            }
        }

        calls
            .into_iter()
            .map(|(func_id, call_count)| FunctionStats {
                function_name: self
                    .function_by_id(func_id)
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| format!("<unknown func_id {func_id}>")),
                calls: call_count,
                self_instructions: *self_instructions.get(&func_id).unwrap_or(&0),
                total_instructions: *total_instructions.get(&func_id).unwrap_or(&0),
            })
            .collect()
    }

    /// U12 item 5: one-shot timeline recording. Resets and re-runs
    /// `function_name(args)` (same recording mechanism as `run`/`profile`),
    /// then derives a chronological call-enter/call-exit (plus tail-call)
    /// event list from the resulting trace's `depth`/`func_id` transitions -
    /// again, no second instrumentation path, the exact same recorded trace
    /// `profile` reads. See `TimelineEventKind`'s doc comment for what each
    /// variant means and `TimelineEvent::step_index`'s doc comment for the
    /// one synthetic event this emits (the top-level call's own closing
    /// `CallExit`, which has no following instruction to observe it at).
    pub fn record_timeline(&self, function_name: &str, args: &[u64]) -> Vec<TimelineEvent> {
        self.run(function_name, args);
        let trace = self.trace();
        if trace.is_empty() {
            return Vec::new();
        }

        let name_of = |func_id: u8| {
            self.function_by_id(func_id)
                .map(|f| f.name.clone())
                .unwrap_or_else(|| format!("<unknown func_id {func_id}>"))
        };

        let mut events = vec![TimelineEvent {
            kind: TimelineEventKind::CallEnter,
            function_name: name_of(trace[0].func_id),
            depth: trace[0].depth,
            step_index: 0,
        }];

        for i in 1..trace.len() {
            let prev = &trace[i - 1];
            let cur = &trace[i];
            if cur.depth > prev.depth {
                events.push(TimelineEvent {
                    kind: TimelineEventKind::CallEnter,
                    function_name: name_of(cur.func_id),
                    depth: cur.depth,
                    step_index: i,
                });
            } else if cur.depth < prev.depth {
                events.push(TimelineEvent {
                    kind: TimelineEventKind::CallExit,
                    function_name: name_of(prev.func_id),
                    depth: prev.depth,
                    step_index: i,
                });
            } else if cur.func_id != prev.func_id {
                events.push(TimelineEvent {
                    kind: TimelineEventKind::TailCall,
                    function_name: name_of(cur.func_id),
                    depth: cur.depth,
                    step_index: i,
                });
            }
        }

        let last = trace.last().expect("checked non-empty above");
        events.push(TimelineEvent {
            kind: TimelineEventKind::CallExit,
            function_name: name_of(last.func_id),
            depth: last.depth,
            step_index: trace.len(),
        });

        events
    }
}

/// U12 item 5: one function's aggregated profiling stats - loosely shaped
/// after `apps/web/src/debug-protocol.ts`'s `FunctionStatsInfo` (`functionId`,
/// `calls`, `totalInstructions`, `selfInstructions`), read for field-naming
/// guidance only (this is still a native Rust spike - no wasm-bindgen
/// bindings, no `apps/web` changes, per item 3/4's own scope boundary, kept
/// here too). See `DebugSession::profile`'s doc comment for exactly how
/// `self_instructions`/`total_instructions` are attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionStats {
    pub function_name: String,
    pub calls: u64,
    pub self_instructions: u64,
    pub total_instructions: u64,
}

/// U12 item 5: one execution-timeline event - loosely shaped after
/// `apps/web/src/debug-protocol.ts`'s `TimelineEventInfo` (`eventType`,
/// `line`, `duration`, ...), read for field-naming guidance only. This
/// item's scope is call-enter/call-exit "at minimum" (per the task's own
/// wording); `TailCall` is included too since it falls directly out of the
/// same depth/`func_id` walk at no extra cost, not a separate line-level
/// "line stepped" event - a caller wanting per-line detail already has
/// `trace()` itself, which already carries a per-instruction `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineEventKind {
    CallEnter,
    CallExit,
    /// Same call depth, but `func_id` changed - `interp::Runtime::dispatch`
    /// reuses the caller's frame for a tail call (see `TraceStep::depth`'s
    /// doc comment), so a tail call never shows up as a `CallEnter`/
    /// `CallExit` pair of its own.
    TailCall,
}

/// U12 item 5: one event in `DebugSession::record_timeline`'s output. See
/// `TimelineEventKind`'s doc comment for what `kind` means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    pub kind: TimelineEventKind,
    pub function_name: String,
    pub depth: u32,
    /// The trace index this event was derived from observing: for
    /// `CallEnter`/`TailCall`, the first instruction of the (new) frame; for
    /// `CallExit`, the instruction immediately *after* the one that
    /// returned - except the top-level call's own closing `CallExit`, which
    /// has no following instruction at all (the run is over), so it reports
    /// `trace().len()`: one index past the end, not a real trace entry.
    pub step_index: usize,
}

/// U12 item 5: `debugGetThreads`-equivalent wire-shape, loosely matching
/// `apps/web/src/debug-protocol.ts`'s `ThreadInfo` (`id`, `status`). See
/// `DebugSession::threads`'s doc comment for why this always reports exactly
/// one, constant entry rather than real multi-thread/coroutine support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInfo {
    pub id: u32,
    pub status: String,
}

/// U12 item 4, deliverable 3: `DebugSession::memory_stats`'s report. See
/// that method's doc comment for exactly what these numbers do (and do not)
/// mean in a jit-free `DebugSession` context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStats {
    pub live_bytes: usize,
    pub live_blocks: usize,
}

/// One rendered local, as reported by `DebugSession::locals_at`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalView {
    pub local_id: usize,
    pub is_param: bool,
    pub ty: Type,
    pub type_name: String,
    pub value: DisplayValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> TProgram {
        let (program, _return_type) = crate::compile(source).expect("fixture compiles");
        program
    }

    #[test]
    fn a_line_with_no_mapped_instruction_is_not_verified() {
        let program = compile(
            "function main(): i64
                 local a: i64 = 1

                 local b: i64 = 2
                 return a + b
             end",
        );
        let session = DebugSession::new(program).expect("session builds");
        // Line 3 is blank - no statement starts there, so no instruction
        // maps to it.
        let verdict = session.set_breakpoint("main", 3);
        assert!(!verdict.verified, "blank line must not verify: {verdict:?}");
        assert_eq!(verdict.pc, None);

        let verdict = session.set_breakpoint("main", 2);
        assert!(verdict.verified, "a real statement's line must verify");
        assert!(verdict.pc.is_some());
    }

    #[test]
    fn distinct_statement_lines_map_to_distinct_pcs() {
        let program = compile(
            "function main(): i64
                 local a: i64 = 1
                 local b: i64 = 2
                 return a + b
             end",
        );
        let bf = tier0::Engine::<()>::new(program, ())
            .expect("engine builds")
            .function_bytecode("main")
            .expect("main compiled");
        let line2_pc = (0..bf.source_map.len() as u32)
            .find(|&pc| bf.source_map.location(pc).unwrap().line == 2)
            .expect("line 2 maps to some pc");
        let line3_pc = (0..bf.source_map.len() as u32)
            .find(|&pc| bf.source_map.location(pc).unwrap().line == 3)
            .expect("line 3 maps to some pc");
        assert_ne!(
            line2_pc, line3_pc,
            "distinct source lines must map to distinct pcs, not the old single_line stub"
        );
    }

    #[test]
    fn threads_always_reports_exactly_one_main_thread() {
        let program = compile("function main(): i64 return 1 end");
        let session = DebugSession::new(program).expect("session builds");
        let threads = session.threads();
        assert_eq!(
            threads.len(),
            1,
            "the typed tier is single-threaded by construction: {threads:?}"
        );
        assert_eq!(threads[0].id, 0);
    }
}
