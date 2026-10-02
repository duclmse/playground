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
    /// Only `Array` and `Struct` are supported: both are simple, pointer-
    /// stable C layouts this spike can read directly (`Array`: `interp.rs`'s
    /// `Op::Index` reads `{len: i64, data: *const u8}` then
    /// `data[index*8..]`; `Struct`: `Op::GetField` reads
    /// `*(base as *const u64).add(field_index)`, no header). `Map`'s
    /// internal hash-table layout is `runtime.rs`'s private implementation
    /// detail (no public C-layout contract to read from outside it), and
    /// `Any`'s reverse tag-to-type lookup needs the registry noted above -
    /// both are explicitly not done here.
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
            _ => None,
        }
    }
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

    fn function_by_id(&self, func_id: u8) -> Option<&TFunction> {
        self.program
            .functions
            .iter()
            .find(|f| self.engine.function_id(&f.name) == Some(func_id))
    }
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
}
