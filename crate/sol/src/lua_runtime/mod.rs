//! Dynamic Lua compatibility runtime.
//!
//! This deliberately does not reuse `dynamic.rs`: that module is Sol's small
//! scalar `any` box.  Lua values carry reference identity, tables, closures,
//! and byte strings, and therefore stay behind the `.lua` boundary.
//!
//! Split across sibling files for maintainability; every file here is a
//! private descendant module of `lua_runtime` (not part of the crate's
//! public module tree on its own), so cross-file access to originally
//! module-private items uses `pub(super)` rather than full `pub`, and this
//! module re-exports everything that was reachable at `lua_runtime::*`
//! before the split:
//!
//! - `value` - `LuaValue` and the types it is built from (tables, closures,
//!   native bridges, keys, errors, global scopes).
//! - `frame` - the bytecode trampoline's data types (no logic).
//! - `dispatch` - the trampoline/bytecode-dispatch core (`call`/`drive`/
//!   `dispatch_step`) and index/arithmetic/metamethod resolution.
//! - `coroutine` - `LuaCoroutine`/`CoroutineStatus` and the coroutine
//!   `LuaRuntime` methods (driven by the same trampoline as ordinary calls).
//! - `init` - `LuaRuntime` construction/configuration and the top-level
//!   `run*` entry points.
//! - `gc` - allocation-budget charging, weak-table sweeping, and the
//!   trial-deletion cycle collector.
//! - `natives` - `call_native` and the standard-library function bodies.
//! - `format` - string-formatting helper functions used by `natives`.
//! - `util` - misc free-function helpers (register-file access, numeric
//!   coercion, date/time conversion, ...) shared by `dispatch`/`natives`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use crate::ast::Program;

pub mod c_api;
mod canonical;
mod coroutine;
mod dispatch;
mod format;
mod frame;
mod gc;
mod init;
mod natives;
mod natives_core;
mod natives_coroutine;
mod natives_debug;
mod natives_load;
mod natives_math;
mod natives_os_io;
mod natives_string;
mod natives_table;
mod natives_utf8;
mod tim_sort;
mod util;
mod value;

pub use canonical::{CanonicalAdapter, CanonicalAdapterError};
pub use coroutine::{CoroutineStatus, LuaCoroutine};
use coroutine::{HookMask, HookState};
pub use sol_core::Capabilities;
pub use value::*;

/// Naming compatibility for embedders written before runtime capabilities
/// moved into `sol-core`. Both names denote the same canonical profile; code
/// constructing the former coarse `os`/`io` fields must select granular fields.
pub type LuaCapabilities = Capabilities;

use frame::Frame;

pub type LuaResult<T> = Result<T, LuaError>;

pub struct LuaRuntime {
    /// Canonical identity/reachability domain used by production managed
    /// objects as they migrate out of the legacy `Rc` graph.
    canonical_heap: RcRef<sol_core::Heap>,
    /// Embedding registry is itself a permanently rooted canonical table.
    c_registry: sol_core::ObjectId,
    _c_registry_root: sol_core::RootId,
    c_next_reference: i64,
    globals: Globals,
    output: Vec<u8>,
    instructions_remaining: u64,
    call_depth: usize,
    max_call_depth: usize,
    allocation_remaining: usize,
    allocation_budget: usize,
    capabilities: Capabilities,
    module_sources: HashMap<Vec<u8>, Vec<u8>>,
    loading_modules: HashSet<Vec<u8>>,
    package_loaded: RcRef<LuaTable>,
    /// The original `package` table object created in `install_base`,
    /// captured directly (like `package_loaded` above) rather than looked up
    /// through the current `package` global on every call. Real Lua's own
    /// `require`/`package.searchpath` close over the table that existed when
    /// `luaopen_package` ran, as C upvalues - so even if Lua code later does
    /// `package = {}` (reassigning the *global name*, as
    /// `lua-5.5.1-tests/attrib.lua`'s "testing preload" section does),
    /// `require` must keep consulting the original table's `preload`/`path`/
    /// `cpath` fields, not whatever the global `package` now points to.
    package_table: RcRef<LuaTable>,
    start_time: std::time::Instant,
    /// Tables ever given a metatable with a weak `__mode`, registered by
    /// `setmetatable` (see `table_weak_mode`). `Weak` so the registry itself
    /// never keeps a table alive; `collectgarbage("collect"/"step")` sweeps
    /// this list (`sweep_weak_tables`) rather than the whole value graph -
    /// there is no general heap/root scan today (see Phase 4b in
    /// `docs/features/lua-superset-plan.md`), so only tables that opted into
    /// weak mode are ever examined.
    weak_tables: Vec<WeakRef<LuaTable>>,
    /// Every ordinary (non-library-bootstrap) table and closure ever
    /// created, registered by `track_table`/`track_closure` at creation
    /// time. `Weak` so tracking never keeps anything alive by itself.
    /// `collect_cycles` (run from `collectgarbage("collect"/"step")`) uses
    /// this as the "candidate" set for a CPython-style trial-deletion cycle
    /// collector layered on top of ordinary `Rc` refcounting: it never
    /// needs to enumerate program roots (globals, live VM registers,
    /// upvalue cells) directly, because any strong reference reaching a
    /// candidate from outside this registry shows up automatically as a
    /// non-zero residual once every *inter-candidate* reference has been
    /// subtracted from each candidate's real `Rc::strong_count`.
    gc_tables: RefCell<Vec<WeakRef<LuaTable>>>,
    gc_closures: RefCell<Vec<Weak<LuaClosure>>>,
    /// The currently active chain of resumed coroutines, innermost last.
    /// Empty means the main chunk (not inside any coroutine) is running.
    /// Doubles as: (a) whether `coroutine.yield` is even legal right now
    /// (`step_result_for_call` errors if this is empty instead of producing
    /// `StepResult::Yield`), and (b) `coroutine.running`/`isyieldable`'s
    /// source of truth.
    coroutine_stack: Vec<Rc<LuaCoroutine>>,
    /// Stable identity for the main Lua thread. It is not placed on
    /// `coroutine_stack` (the empty stack still means yielding is illegal),
    /// but `coroutine.running()` returns this handle with `is_main = true`.
    main_coroutine: Rc<LuaCoroutine>,
    /// Opt-in GC stress mode (see `set_gc_stress`/`SOL_LUA_GC_STRESS`): runs
    /// a full weak-table sweep + cycle-collection pass at every allocation
    /// point (`charge_allocation`) and after every dispatched dynamic
    /// bytecode instruction (`tick`), rather than only when a script calls
    /// `collectgarbage()`. This is the L6 exit-gate stress requirement
    /// (`docs/features/lua-superset-plan.md` Phase 4b) - it exists to catch
    /// premature collection/dangling references under maximal collection
    /// pressure, not for production use (it is far too slow for that).
    gc_stress: bool,
    /// Dispatched instructions (`tick`) since the last automatic collection
    /// pass, when `gc_stress` is off. Real Lua's collector runs
    /// automatically as the program executes, independent of any explicit
    /// `collectgarbage()` call - and weak-table pruning (`sweep_weak_tables`)
    /// only ever happens during a collection pass - so idioms like
    /// `while weak_table[k] do ... end` (`lua-5.5.1-tests/closure.lua` uses
    /// exactly this to force a GC after dropping the last strong reference
    /// to a weak-valued entry) terminate almost immediately in real Lua but
    /// would otherwise spin in this runtime until the instruction budget ran
    /// out, since nothing but an explicit `collectgarbage()` or the far too
    /// slow `gc_stress` mode ever swept a weak table. `tick` increments this
    /// and triggers a pass every `AUTO_GC_INSTRUCTION_INTERVAL` instructions
    /// - instruction count, rather than real Lua's allocated-byte debt, is
    /// the pacing signal because it's the resource this runtime already
    /// meters on every dispatched instruction, so no extra accounting is
    /// needed for a production-cheap approximation of "collection happens
    /// without being asked".
    instructions_since_gc: u64,
    /// Free lists of previously-used register/cell frames, keyed by nothing
    /// (LIFO, just like the Rust call stack `run_proto` recurses on) -
    /// recycled by `take_regs_buffer`/`take_cells_buffer` and
    /// `recycle_frame_buffers` instead of `Vec::new`-ing a fresh frame on
    /// every single Lua function call. A call-heavy dynamic workload used to
    /// pay two heap allocations (and, on return, two frees) per call for
    /// this; recycling turns steady-state recursion/looping calls into
    /// amortized-zero-allocation frame setup. Correctness is unaffected:
    /// each popped buffer is fully re-initialized (cleared, then re-filled
    /// to the new callee's exact shape) before use, exactly as a freshly
    /// allocated one would have been. Recycling only happens on `run_proto`'s
    /// normal-return path (`Instr::Return`); an error exit just drops its
    /// frame instead of pushing it back - errors are the rare path, and
    /// threading recycling through every `?`-propagated exit isn't worth the
    /// control-flow complexity (and was measured to actually regress
    /// call-light, loop-heavy benchmarks when tried via a wrapping closure).
    regs_pool: Vec<Vec<LuaValue>>,
    cells_pool: Vec<Cells>,
    /// Free list of previously-used argument/return-value buffers, recycled
    /// by `take_values_buffer`/`recycle_values_buffer` - the same idea as
    /// `regs_pool`, but for the short-lived `Vec<LuaValue>`s built per call
    /// for a callee's arguments and a callee's results (`Instr::Call`,
    /// `run_proto`'s vararg/param handling), instead of a call's whole
    /// register file.
    values_pool: Vec<Vec<LuaValue>>,
    /// Per-runtime xoshiro256** state used by `math.random`. Keeping this in
    /// the runtime (rather than process-global state) makes independent
    /// embedders reproducible and matches Lua 5.5's per-state generator.
    random_state: [u64; 4],
    /// The explicit, heap-resident call-frame stack that replaces native
    /// Rust recursion for Lua execution. Every frame here (bytecode or
    /// native-continuation) can be paused between dispatch steps and later
    /// resumed - required so a step debugger can suspend execution at any
    /// point, including inside metamethods and reentrant natives like
    /// `table.sort`'s comparator, on targets (wasm32) with no fiber/thread
    /// suspension primitive. See `docs/features/lua-debug-frames.md`.
    frames: Vec<Frame>,
    /// Set immediately before dispatching a `__index`/`__newindex`
    /// metamethod call (`"index"`/`"newindex"`), and consumed by `drive`'s
    /// `StepResult::PushClosure` handling on the very next step to tag the
    /// pushed frame's `entry_label` - lets an error raised inside the
    /// metamethod's own body get annotated "in metamethod '...'" the same
    /// way an ordinary call site gets annotated with its line number. Always
    /// taken (cleared) right after the triggering `dispatch_step` call
    /// returns, so it never leaks onto an unrelated later call.
    pending_frame_label: Option<&'static str>,
    /// The failing call's `LuaError.stack` trail (frame names/lines,
    /// innermost first - see `LuaError::at`), stashed by
    /// `unwind_error_to_marker` right before invoking an `xpcall` message
    /// handler so `debug.traceback` can still report it even though the
    /// erroring frames themselves are already gone from `frames` by then.
    /// Taken (cleared) by `debug.traceback`'s own implementation.
    pending_error_stack: Option<Vec<String>>,
    /// Stable identities assigned to executable prototypes as they enter the
    /// shared semantic frame ABI. Pointer identity is used only as this
    /// legacy-side lookup key; frames themselves carry portable IDs.
    prototype_ids: HashMap<usize, sol_core::FunctionId>,
    /// `debug.getinfo`'s `source` field for every `Proto` compiled through
    /// `compile_chunk` (i.e. every `load`ed chunk), keyed by `Rc<Proto>`
    /// pointer identity - real Lua stores this per-`Proto` uniformly across
    /// a whole chunk (main function and every nested one), set once from the
    /// chunk name given to `load`/`lua_load`. Absent for protos compiled
    /// outside `compile_chunk` (the top-level script `sol run` executes
    /// directly); `debug.getinfo` simply omits the `source` field then,
    /// rather than guessing a default.
    chunk_sources: HashMap<usize, Rc<Vec<u8>>>,
    /// `string.dump`'s output registry: Sol has no portable bytecode-file
    /// format (no `Proto` (de)serializer), so `string.dump` cannot actually
    /// encode a `Proto`'s instructions into its returned byte string the way
    /// real Lua's `lundump.c`/`ldump.c` do. Instead the returned bytes are an
    /// opaque handle (a `0x1B` binary-chunk signature byte, matching real
    /// Lua's `LUA_SIGNATURE[0]` so `load`'s text/binary mode check still
    /// works, followed by this map's key) into this process-local table,
    /// which keeps the dumped `Rc<Proto>` alive so `load(..., "b")` can look
    /// it up and rebuild an equivalent closure directly - sufficient for
    /// same-process round-tripping (the only case the Lua 5.5 test corpus
    /// exercises) but not for a chunk written to a file and loaded by a
    /// different process.
    dumped_protos: HashMap<usize, Rc<crate::lua_bytecode::Proto>>,
    function_registry: sol_core::FunctionRegistry,
    /// Host-owned implementations keyed by portable callable identity. Raw
    /// pointers stay in this side registry and never enter Lua values,
    /// tables, frames, or canonical snapshots.
    native_bridges: HashMap<sol_core::NativeCallableId, Rc<NativeBridge>>,
    c_functions: HashMap<sol_core::NativeCallableId, c_api::LuaCFunction>,
    c_function_ids: HashMap<usize, sol_core::NativeCallableId>,
    c_light_userdata: RefCell<HashMap<usize, sol_core::ObjectId>>,
    c_light_userdata_reverse: RefCell<HashMap<sol_core::ObjectId, usize>>,
    c_warning_function: Option<c_api::LuaWarnFunction>,
    c_warning_data: *mut std::ffi::c_void,
    c_thread_states: RefCell<HashMap<usize, *mut c_api::lua_State>>,
    c_continuations: RefCell<HashMap<usize, c_api::CContinuation>>,
    c_upvalue_identities: RefCell<HashMap<(sol_core::ObjectId, usize), Box<u8>>>,
    native_libraries: Vec<c_api::NativeLibrary>,
    native_bridge_ids: HashMap<usize, sol_core::NativeCallableId>,
    next_native_function: u32,
    /// The single shared metatable every string value indexes through
    /// (`{ __index = string }`), matching real Lua's `G(L)->strmt`. Strings
    /// have no per-value metatable slot of their own - `getmetatable("")`
    /// returns this same table for every string, and mutating it (e.g.
    /// `getmetatable(""):__band = fn`, used by `bwcoercion.lua`-style shims
    /// to add metamethods to all strings) is visible to every subsequent
    /// string operation, since `index`/`metamethod` resolve strings through
    /// this table rather than a hardcoded reference straight to `string`.
    string_metatable: RcRef<LuaTable>,
    /// The shared metatable for every number (`Integer`/`Float` alike, as
    /// real Lua uses one `LUA_TNUMBER` slot for both subtypes), settable
    /// only through `debug.setmetatable` since ordinary `setmetatable`
    /// rejects non-table values. `None` until first set, unlike
    /// `string_metatable` which always exists (strings get method dispatch
    /// by default; numbers don't).
    number_metatable: Option<RcRef<LuaTable>>,
    /// The shared metatable for every boolean, mirroring `number_metatable`
    /// but for real Lua's single `LUA_TBOOLEAN` basic-type slot.
    boolean_metatable: Option<RcRef<LuaTable>>,
    /// The shared metatable for `nil`, mirroring `number_metatable` but for
    /// real Lua's single `LUA_TNIL` basic-type slot.
    nil_metatable: Option<RcRef<LuaTable>>,
    /// Bookkeeping-only GC collector mode, `"incremental"` or
    /// `"generational"` (real Lua's default is `"incremental"`). Sol always
    /// runs the same trial-deletion cycle collection pass regardless of this
    /// value - there is no actual incremental/generational stepping
    /// behavior difference yet - but `collectgarbage("incremental"|
    /// "generational")` must still report and return the *previous* mode
    /// like real Lua's `lua_gc` does, since Lua programs (e.g.
    /// `tests/lua55/.../gc.lua`) observe and assert on it.
    gc_mode: &'static str,
    /// Whether `collectgarbage("stop")`/`("restart")` has toggled collection
    /// off. Sol's collector is fully manual (only ever runs when
    /// `collectgarbage()` is called), so this flag is pure bookkeeping for
    /// `collectgarbage("isrunning")` - it does not actually gate anything.
    gc_running: bool,
    /// Bookkeeping-only `collectgarbage("param", "pause"|"stepmul", ...)`
    /// values. Sol has no incremental step-size heuristics to tune, so
    /// these are stored and returned as-is purely so scripts that read back
    /// what they just set observe consistent values.
    gc_pause: i64,
    gc_stepmul: i64,
    /// Bookkeeping-only `os.setlocale` current-locale name (real Lua
    /// defaults to `"C"` until changed) - see `NativeFunction::OsSetlocale`.
    current_locale: String,
    /// Monotonic per-runtime counter disambiguating same-process
    /// `os.tmpname()` calls (`std::process::id()` alone isn't unique across
    /// repeated calls within one run).
    tmpname_counter: u64,
    /// The handle `io.write(...)` and `io.stdout` both return, so
    /// `io.write("a"):write("b")`/`io.stdout:write("a"):write("b")` chain
    /// like real Lua's default output file object. There is only one
    /// process-wide output sink today (`self.output`, an in-memory buffer
    /// captured deterministically rather than written straight to a real
    /// stdout stream), so this handle is a lightweight table with a single
    /// `write` method rather than a full file-handle implementation
    /// (`close`/`seek`/`lines`/real `io.open` are out of scope here).
    io_stdout: RcRef<LuaTable>,
    /// The handle `io.stderr` exposes - structurally identical to
    /// `io_stdout` (a lightweight `write`/`close`-method table, not a real
    /// open file), but `write_values_to_target` special-cases this table's
    /// identity to write straight to the process's real stderr stream
    /// instead of buffering into `self.output` (which is stdout-only
    /// captured output later returned by `take_output`). Real Lua programs
    /// (e.g. `lua-5.5.1-tests/heavy.lua`'s progress-reporting
    /// `io.stderr:write(...)` calls) expect stderr writes to be genuinely
    /// separate from stdout, not interleaved into it.
    io_stderr: RcRef<LuaTable>,
    /// The handle `io.write`/`io.output()` (no arguments) currently target -
    /// `io_stdout` by default, or a table returned by `io.output(path)` once
    /// redirected to a real file (see `open_files`). Real Lua's default
    /// output file object; unlike `io_stdout`, this can change over the
    /// program's lifetime.
    default_output: RefCell<LuaValue>,
    /// Real, host-backed files opened by `io.output(path)` (there is no
    /// general `io.open` yet - only the default-output redirection
    /// `lua-5.5.1-tests/attrib.lua`'s `createfiles`/`removefiles` helpers
    /// need), keyed by the identity (`Rc::as_ptr`) of the lightweight
    /// `LuaTable` handle returned to Lua code for that file. `LuaValue` has
    /// no variant for an open file descriptor (see its doc comment - the
    /// bridge only carries pointer-free/scalar data), so the real
    /// `std::fs::File` lives only here, off to the side, and the handle
    /// table Lua code holds is just a `write`/`close`-method dispatch
    /// target that looks itself up in this map by pointer identity.
    open_files: RefCell<HashMap<usize, Rc<RefCell<std::fs::File>>>>,
    /// The `debug.sethook` state of whichever coroutine/main is currently
    /// executing, cached here so `dispatch_step`'s per-instruction line/count
    /// check and `call`'s per-call call/return check are a cheap `Option`
    /// read instead of a `coroutine_stack.last()` lookup plus a `RefCell`
    /// borrow on every single instruction. Kept in sync with whichever
    /// `LuaCoroutine::hook` is authoritative: `resume_coroutine` reloads it
    /// (and restores the caller's) across every coroutine switch, and
    /// `debug.sethook` refreshes it immediately when it targets the
    /// currently-running coroutine/main.
    active_hook: Option<Rc<HookState>>,
    /// True for the entire duration of a hook callback's own execution
    /// (including anything that callback itself calls) - real Lua disables
    /// every hook (call/line/return/count) while a hook is already running,
    /// both to match its documented semantics and to prevent a hook that
    /// itself executes Lua code from recursing into itself forever. See
    /// `fire_hook`.
    running_hook: bool,
}

impl Default for LuaRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of running a `.lua` module through the dynamic compatibility tier.
/// Output is kept as bytes so the host does not need to lossy-decode Lua
/// strings before writing them to stdout.
#[derive(Debug)]
pub struct LuaRun {
    pub value: LuaValue,
    pub output: Vec<u8>,
}

pub fn run_source(source: &[u8]) -> LuaResult<LuaRun> {
    let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).map_err(LuaError::new)?)
        .map_err(LuaError::new)?;
    run_program(&program)
}

/// Runs an already-lexed-and-parsed `.lua` program. Lets callers that must
/// first attempt the typed pipeline (which needs its own parse) reuse that
/// parse on fallback instead of lexing/parsing the source a second time.
pub fn run_program(program: &Program) -> LuaResult<LuaRun> {
    run_program_with_natives(
        program,
        &HashSet::new(),
        HashMap::new(),
        Capabilities::default(),
    )
}

/// Like `run_program`, but for a per-function typed/dynamic split
/// (`typeck::check_partitioned`): `native_names` are function names that
/// were compiled through the native pipeline instead and must not also be
/// compiled/bound as bytecode closures here, and `natives` supplies the
/// `LuaValue::Native` bridge binding for each of those actually reachable
/// from dynamic code (a subset of `native_names` - a native function with
/// no dynamic caller needs no binding at all). `capabilities` lets the
/// caller (e.g. the `sol` CLI) opt into host facilities (`os`/`io`/...)
/// instead of the sandboxed-by-default profile `LuaRuntime::new` uses.
pub fn run_program_with_natives(
    program: &Program,
    native_names: &HashSet<String>,
    natives: HashMap<String, LuaValue>,
    capabilities: Capabilities,
) -> LuaResult<LuaRun> {
    let mut runtime = LuaRuntime::with_capabilities(capabilities);
    match runtime.run_with_natives(program, native_names, natives) {
        Ok(value) => Ok(LuaRun {
            value,
            output: runtime.take_output(),
        }),
        Err(mut error) => {
            error.output = runtime.take_output();
            Err(error)
        }
    }
}

/// Like `run_program_with_natives`, but also overrides the resource budgets
/// (see `LuaRuntime::with_capabilities_and_budgets`) and optionally enables
/// GC stress mode (see `LuaRuntime::set_gc_stress`) for the run.
#[allow(clippy::too_many_arguments)]
pub fn run_program_with_natives_and_budgets(
    program: &Program,
    native_names: &HashSet<String>,
    natives: HashMap<String, LuaValue>,
    capabilities: Capabilities,
    instruction_budget: u64,
    max_call_depth: usize,
    allocation_budget: usize,
    gc_stress: bool,
) -> LuaResult<LuaRun> {
    let mut runtime = LuaRuntime::with_capabilities_and_budgets(
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
    );
    runtime.set_gc_stress(gc_stress);
    match runtime.run_with_natives(program, native_names, natives) {
        Ok(value) => Ok(LuaRun {
            value,
            output: runtime.take_output(),
        }),
        Err(mut error) => {
            error.output = runtime.take_output();
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_program_with_natives_budgets_and_plan(
    program: &Program,
    native_names: &HashSet<String>,
    natives: HashMap<String, LuaValue>,
    capabilities: Capabilities,
    instruction_budget: u64,
    max_call_depth: usize,
    allocation_budget: usize,
    gc_stress: bool,
    optimization_plan: &crate::typeck::inference::OptimizationPlan,
) -> LuaResult<LuaRun> {
    let mut runtime = LuaRuntime::with_capabilities_and_budgets(
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
    );
    runtime.set_gc_stress(gc_stress);
    match runtime.run_with_natives_and_plan(program, native_names, natives, optimization_plan) {
        Ok(value) => Ok(LuaRun {
            value,
            output: runtime.take_output(),
        }),
        Err(mut error) => {
            error.output = runtime.take_output();
            Err(error)
        }
    }
}
