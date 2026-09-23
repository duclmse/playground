// JS-facing value types (wasm_bindgen structs, following the ExecuteResult/
// DebugEvent pattern already established in lib.rs/debug_events.rs) and the
// small internal-only types the rest of `session` drives execution/
// inspection with. Fields here are `pub(super)` (not fully `pub`) - visible
// throughout `session` and its submodules, but not outside this crate's
// `session` module, since construction/mutation of these happens directly
// (not through constructor methods) from sibling files like
// `execution.rs`/`inspector.rs`/`evaluate.rs`.

use vm::{Context, StashedExecutor, Thread};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct Variable {
    pub(super) name: std::string::String,
    pub(super) value_type: std::string::String,
    pub(super) display: std::string::String,
    pub(super) expandable: bool,
    pub(super) reference: Option<u32>,
}

#[wasm_bindgen]
impl Variable {
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> std::string::String {
        self.name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn value_type(&self) -> std::string::String {
        self.value_type.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn display(&self) -> std::string::String {
        self.display.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn expandable(&self) -> bool {
        self.expandable
    }
    #[wasm_bindgen(getter)]
    pub fn reference(&self) -> Option<u32> {
        self.reference
    }
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct StackFrame {
    pub(super) index: u32,
    pub(super) name: std::string::String,
    pub(super) source: std::string::String,
    pub(super) line: Option<u32>,
    pub(super) function_type: std::string::String,
}

#[wasm_bindgen]
impl StackFrame {
    #[wasm_bindgen(getter)]
    pub fn index(&self) -> u32 {
        self.index
    }
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> std::string::String {
        self.name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn source(&self) -> std::string::String {
        self.source.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> Option<u32> {
        self.line
    }
    #[wasm_bindgen(getter)]
    pub fn function_type(&self) -> std::string::String {
        self.function_type.clone()
    }
}

/// Phase 8 (docs/debug-protocol.md#advanced-coroutines-phase-8): one entry
/// of `DebugSession::get_threads`.
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub(super) id: u32,
    pub(super) status: std::string::String,
}

#[wasm_bindgen]
impl ThreadInfo {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> std::string::String {
        self.status.clone()
    }
}

/// Resolves `thread_id` (an index into `Executor::debug_thread_stack`,
/// bottom-to-top - `0` is always the main thread) against the executor's
/// *current* thread nesting. Shared by every inspector method that takes a
/// `thread_id` - `get_stack_trace`, `get_locals`, and (via
/// `compile_and_run_eval`/`find_named_register`) `evaluate`/`set_variable`.
pub(super) fn thread_by_id<'gc>(
    ctx: Context<'gc>,
    executor: &StashedExecutor,
    thread_id: u32,
) -> Option<Thread<'gc>> {
    ctx.fetch(executor)
        .debug_thread_stack()?
        .get(thread_id as usize)
        .copied()
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct StopEvent {
    pub(super) reason: std::string::String,
    pub(super) line: Option<u32>,
    pub(super) message: Option<std::string::String>,
}

#[wasm_bindgen]
impl StopEvent {
    #[wasm_bindgen(getter)]
    pub fn reason(&self) -> std::string::String {
        self.reason.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> Option<u32> {
        self.line
    }
    #[wasm_bindgen(getter)]
    pub fn message(&self) -> Option<std::string::String> {
        self.message.clone()
    }
}

impl StopEvent {
    pub(super) fn new(reason: &str, line: Option<u32>) -> Self {
        StopEvent {
            reason: reason.to_string(),
            line,
            message: None,
        }
    }
    pub(super) fn exception(message: std::string::String, line: Option<u32>) -> Self {
        StopEvent {
            reason: "exception".to_string(),
            line,
            message: Some(message),
        }
    }
}

/// A `continue_burst` outcome (docs/phase-4-8-implementation.md's `pause()`
/// design): either a real stop happened (`stopped: true`, `stop` set, same
/// as `continue_()` would have returned), or the burst's instruction budget
/// ran out first with nothing having happened yet (`stopped: false`,
/// `source`/`line` report wherever execution currently sits, for a caller
/// that wants to show *something* while deciding whether to request another
/// burst or leave the program paused there).
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct BurstResult {
    pub(super) stopped: bool,
    pub(super) stop: Option<StopEvent>,
    pub(super) source: Option<std::string::String>,
    pub(super) line: Option<u32>,
}

#[wasm_bindgen]
impl BurstResult {
    #[wasm_bindgen(getter)]
    pub fn stopped(&self) -> bool {
        self.stopped
    }
    #[wasm_bindgen(getter)]
    pub fn stop(&self) -> Option<StopEvent> {
        self.stop.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn source(&self) -> Option<std::string::String> {
        self.source.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> Option<u32> {
        self.line
    }
}

/// [`super::DebugSession::drive_with_budget`]'s result before it's
/// translated into either a plain [`StopEvent`] (`drive()`, unlimited
/// budget) or a [`BurstResult`] (`continue_burst`, a caller-chosen budget).
pub(super) enum DriveOutcome {
    Stopped(StopEvent),
    BudgetExhausted {
        source: std::string::String,
        line: Option<u32>,
    },
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct EvalResult {
    pub(super) ok: bool,
    pub(super) display: std::string::String,
}

#[wasm_bindgen]
impl EvalResult {
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.ok
    }
    /// The value's display string on success, or the error message on
    /// failure - matching how `execute()`/`ExecuteResult` already report
    /// errors as display strings elsewhere in this crate.
    #[wasm_bindgen(getter)]
    pub fn display(&self) -> std::string::String {
        self.display.clone()
    }
}

/// Live allocation/GC-pressure stats for a `DebugSession`'s program, read
/// straight from `gc-arena`'s `Metrics` (via `vm::Lua::gc_metrics()`) - see
/// `memory.rs`'s doc comment for what is and isn't available at this layer
/// (byte totals only; no per-type or per-object counts, no history).
#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub(super) total_allocation: f64,
    pub(super) gc_allocation: f64,
    pub(super) external_allocation: f64,
    pub(super) allocation_debt: f64,
}

#[wasm_bindgen]
impl MemoryStats {
    /// Total bytes attributed to this Lua state (`gc_allocation +
    /// external_allocation`) - the same number `collectgarbage("count")`
    /// reports, in bytes rather than KB.
    #[wasm_bindgen(getter)]
    pub fn total_allocation(&self) -> f64 {
        self.total_allocation
    }
    /// Bytes backing `Gc`-allocated values themselves.
    #[wasm_bindgen(getter)]
    pub fn gc_allocation(&self) -> f64 {
        self.gc_allocation
    }
    /// Bytes tables/strings/etc. report as data they own outside their own
    /// `Gc` allocation (e.g. a table's backing array/hashbrown storage).
    #[wasm_bindgen(getter)]
    pub fn external_allocation(&self) -> f64 {
        self.external_allocation
    }
    /// `gc-arena`'s incremental-collector pacing debt - not a byte count,
    /// but indicates how much GC pressure has built up since the last cycle.
    #[wasm_bindgen(getter)]
    pub fn allocation_debt(&self) -> f64 {
        self.allocation_debt
    }
}

// ---------------------------------------------------------------------
// Breakpoints (docs/debug-protocol.md#breakpoints). `condition`/
// `hit_condition`/`log_message` are Phase 8 fields on the same shape the
// spec already defines for Phase 4 - reused, not bolted on separately.

#[derive(Clone)]
pub(super) struct BreakpointState {
    pub(super) id: u32,
    // Matches piccolo's chunk-naming convention (see risks.md §6): the
    // entry file is named exactly as passed to `launch`/`launch_project`'s
    // `chunk_name`/`entry`, and `require()`d files are named `"<name>.lua"`/
    // `"<name>"` per `install_require`. A breakpoint set without knowing
    // this - e.g. a UI that only has "line 5" and not "which open file" -
    // is a bug in the caller, not something this type should paper over by
    // matching line numbers across every file in the project.
    pub(super) source_id: std::string::String,
    pub(super) line: u32,
    pub(super) condition: Option<std::string::String>,
    pub(super) hit_condition: Option<u32>,
    pub(super) log_message: Option<std::string::String>,
    pub(super) hits: u32,
}

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub(super) id: u32,
    pub(super) source_id: std::string::String,
    pub(super) line: u32,
    pub(super) verified: bool,
}

#[wasm_bindgen]
impl Breakpoint {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }
    #[wasm_bindgen(getter)]
    pub fn source_id(&self) -> std::string::String {
        self.source_id.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> u32 {
        self.line
    }
    #[wasm_bindgen(getter)]
    pub fn verified(&self) -> bool {
        self.verified
    }
}

// ---------------------------------------------------------------------
// Internal stepping state - what "line changed"/"call happened"/"returned"
// means is always relative to a snapshot taken right before the operation
// started (docs/debug-protocol.md#stepping-algorithms).

#[derive(Clone)]
pub(super) struct FrameSnapshot {
    pub(super) lua_depth: usize,
    pub(super) line: Option<u32>,
    // Needed so `drive()`'s "only re-check once the position actually
    // changes" dedup (see its doc comment) compares *position*, not just
    // line number - two different chunks can legitimately share a line
    // number (e.g. both happen to have their interesting statement on line
    // 3), and treating those as "the same position" was a real bug this
    // field fixes (see `breakpoints_are_scoped_to_their_own_file_in_a_multi_file_project`).
    pub(super) source: std::string::String,
}

pub(super) enum StepMode {
    Continue(FrameSnapshot),
    Over(FrameSnapshot),
    Into(FrameSnapshot),
    Out(FrameSnapshot),
}

impl StepMode {
    pub(super) fn start(&self) -> FrameSnapshot {
        match self {
            StepMode::Continue(s) | StepMode::Over(s) | StepMode::Into(s) | StepMode::Out(s) => {
                s.clone()
            }
        }
    }
}
