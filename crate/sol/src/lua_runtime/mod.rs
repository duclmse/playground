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
//! - `diagnostics` - Lua-visible type labels used by errors across the
//!   dispatcher and native libraries.
//! - `gc` - allocation-budget charging, weak-table sweeping, and the
//!   trial-deletion cycle collector.
//! - `natives` - `call_native` and the standard-library function bodies.
//! - `format` - string-formatting helper functions used by `natives`.
//! - `util` - misc free-function helpers (register-file access, numeric
//!   coercion, date/time conversion, ...) shared by `dispatch`/`natives`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::Program;

pub mod c_api;
mod analysis;
mod canonical;
mod clock;
mod codec;
mod coroutine;
mod diagnostics;
pub mod debugger;
mod dispatch;
mod dynjit;
mod format;
mod frame;
mod gc;
mod ic;
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
mod table;
mod tim_sort;
mod util;
mod value;

pub use canonical::CanonicalAdapterError;
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
    /// Depth of calls dispatched synchronously through `call()` - the one
    /// entry point every genuinely Rust-stack-recursive call goes through
    /// (a native builtin, a metamethod, a finalizer, a module loader), as
    /// opposed to a plain bytecode `Instr::Call` to a Lua closure (or an
    /// `xpcall`/`pcall` protected call), which the `drive`/`resolve_call`
    /// trampoline executes iteratively via the explicit `frames` stack with
    /// no added Rust recursion at all. Mirrors real Lua's `nCcalls`
    /// (`ldo.c`): bounded by the much smaller, fixed `MAX_NATIVE_CALL_DEPTH`
    /// rather than the larger, embedder-configurable `max_call_depth`
    /// budget, and reported as `"C stack overflow"` - real Lua's own wording
    /// for exactly this condition - never the general `"stack overflow
    /// (...)"` message. See `call()`'s own check.
    ///
    /// Deliberately *not* charged for a trampolined `xpcall` message-handler
    /// retry (`xcall_retry_depth` tracks that instead): unlike a genuine
    /// nested `call()` invocation, a handler retry does not recurse the Rust
    /// stack at all, so an ordinary leaf native call made from inside the
    /// handler's own body (`type(n)`, say) must not be blocked just because
    /// many retries have accumulated.
    native_call_depth: usize,
    /// Depth of `xpcall` message-handler *retries* specifically - real
    /// Lua's `luaG_errormsg` re-invokes the handler on the handler's own
    /// error, unboundedly, and this is the separate budget that stands in
    /// for `nCcalls` in exactly that one scenario (see `XCallStage`'s doc
    /// and `unwind_error_to_marker`'s `Xpcall` arms). Kept apart from
    /// `native_call_depth` for the reason documented on that field.
    xcall_retry_depth: usize,
    /// Whether `call_depth` has already hit `max_call_depth` once since the
    /// value stack was last shrunk back down. Real Lua's `luaD_growstack`
    /// reallocates the stack to a larger `ERRORSTACKSIZE` the first time it
    /// overflows, specifically to give a message handler room to run in -
    /// but that larger allocation persists (no synchronous shrink) until
    /// `luaD_pcall` catches a non-`LUA_OK` status, at which point it
    /// unconditionally calls `luaD_shrinkstack`. So a *second* overflow
    /// while still sitting at that inflated size (e.g. a message handler
    /// that itself calls something deep enough to overflow again) hits
    /// `luaD_growstack`'s immediate-`LUA_ERRERR` path instead of raising
    /// another catchable `"stack overflow"` and invoking the handler again -
    /// see every `call_depth >= max_call_depth` check site - while an
    /// unrelated, later deep recursion, reached only after some enclosing
    /// `pcall`/`xpcall` already caught and cleared this flag (see
    /// `reset_call_depth_overflow`, called at every point one terminally
    /// resolves a caught error), still gets its own fresh, catchable first
    /// overflow. `release_call_depth` clears it too, as a fallback, once
    /// `call_depth` drains all the way back to `0` outside any protected
    /// call at all.
    call_depth_overflowed_once: bool,
    allocation_remaining: usize,
    /// The fixed cap `allocation_remaining` is reset toward after every
    /// collection (`gc.rs`'s `collect_garbage_with`), crediting back
    /// whatever `sol_core::Heap::live_bytes` no longer counts as retained -
    /// see docs/features/table-closure-coroutine-cutover.md §10 (task #12).
    allocation_budget: usize,
    capabilities: Capabilities,
    module_sources: HashMap<Vec<u8>, Vec<u8>>,
    module_configs: HashMap<Vec<u8>, crate::parser::LanguageConfig>,
    semantic_functions: HashMap<sol_core::NativeCallableId, u8>,
    debug_semantic_request: Option<(u8, Vec<LuaValue>)>,
    next_debug_frame: u64,
    loading_modules: HashSet<Vec<u8>>,
    package_loaded: TableRef,
    /// The original `package` table object created in `install_base`,
    /// captured directly (like `package_loaded` above) rather than looked up
    /// through the current `package` global on every call. Real Lua's own
    /// `require`/`package.searchpath` close over the table that existed when
    /// `luaopen_package` ran, as C upvalues - so even if Lua code later does
    /// `package = {}` (reassigning the *global name*, as
    /// `lua-5.5.1-tests/attrib.lua`'s "testing preload" section does),
    /// `require` must keep consulting the original table's `preload`/`path`/
    /// `cpath` fields, not whatever the global `package` now points to.
    package_table: TableRef,
    start_time: clock::Timer,
    /// Every closure's `Globals` (Sol's module-isolation/constants
    /// bookkeeping, which has no equivalent field on canonical
    /// `HeapObject::Closure`'s `ClosureObject{prototype, upvalues,
    /// environment}` - `_ENV` there is just an ordinary captured upvalue
    /// slot), keyed by the closure's own canonical `ObjectId`. Populated by
    /// `Instr::NewClosure` at creation time, consulted whenever a
    /// `LuaValue::Closure` is invoked. Same side-table pattern as
    /// `canonical::PrototypeRegistry`: every entry here is treated as
    /// reachable while present (unlike `CoroutineRegistry`, whose entries are
    /// only *conditionally* rooted as of task #13 - see its own doc comment
    /// and docs/features/table-closure-coroutine-cutover.md §11), and a
    /// closure's entry is removed only when its own canonical identity is
    /// (see docs/features/table-closure-coroutine-cutover.md §8 step 4).
    closure_globals: RefCell<HashMap<sol_core::ObjectId, Globals>>,
    /// The currently active chain of resumed coroutines, innermost last.
    /// Empty means the main chunk (not inside any coroutine) is running.
    /// Doubles as: (a) whether `coroutine.yield` is even legal right now
    /// (`step_result_for_call` errors if this is empty instead of producing
    /// `StepResult::Yield`), and (b) `coroutine.running`/`isyieldable`'s
    /// source of truth.
    coroutine_stack: Vec<ThreadRef>,
    /// Stable identity for the main Lua thread. It is not placed on
    /// `coroutine_stack` (the empty stack still means yielding is illegal),
    /// but `coroutine.running()` returns this handle with `is_main = true`.
    main_coroutine: ThreadRef,
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
    /// and triggers a pass every `AUTO_GC_INSTRUCTION_INTERVAL` instructions.
    /// Instruction count, rather than real Lua's allocated-byte debt, is the
    /// pacing signal because it's the resource this runtime already meters
    /// on every dispatched instruction, so no extra accounting is needed
    /// for a production-cheap approximation of "collection happens without
    /// being asked".
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
    debug_slice_remaining: Option<u64>,
    debug_drive_nesting: usize,
    debug_recording: Option<analysis::Recording>,
    debug_module_names: HashMap<Vec<u8>, Vec<u8>>,
    debug_resume_request: Option<frame::DebugResumeRequest>,
    debug_pause_requested: bool,
    debug_incoming_roots: Vec<sol_core::Value>,
    /// Extra GC roots pinned for the duration of a reentrant call driven from
    /// inside `collect_garbage_with` (currently: `__gc` finalizer
    /// invocation). A finalizer's own `self.call` drives a fresh nested
    /// dispatch loop with its own `active_frame`, but that nested loop's
    /// `frame_roots` walk of `self.frames` does not include whatever frame
    /// the *outer*, still-in-flight `dispatch_step` call already popped into
    /// a Rust local (see `tick`'s doc comment) - it is neither on
    /// `self.frames` nor threaded through `self.call`. Pushing that frame's
    /// roots here before invoking a finalizer, and popping them back off
    /// after, keeps values reachable only through it (e.g. an enclosing
    /// local a `__gc` closure's upvalue is the sole other reference to)
    /// alive across any collection nested inside the finalizer call. A
    /// `Vec<Vec<_>>` rather than one flat `Vec` so nested finalizer calls
    /// (a finalizer whose own execution triggers another collection with
    /// finalizers of its own) unwind cleanly, each popping only its own
    /// segment.
    pinned_roots: Vec<Vec<sol_core::Value>>,
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
    /// `compile_chunk` (i.e. every `load`ed chunk) or the CLI's own top-level
    /// compile (see `default_chunk_name`), keyed by `Rc<Proto>` pointer
    /// identity - real Lua stores this per-`Proto` uniformly across a whole
    /// chunk (main function and every nested one), set once from the chunk
    /// name given to `load`/`lua_load`. Absent for protos compiled without a
    /// chunk name at all (e.g. embedders calling `run_program` directly);
    /// `debug.getinfo` simply omits the `source` field then, rather than
    /// guessing a default.
    chunk_sources: HashMap<usize, Rc<Vec<u8>>>,
    /// Chunk name applied to every top-level function `load_in_globals`
    /// compiles, `@`-prefixed like a `load`/`loadfile` filename source (see
    /// `chunk_sources`). Set once via `set_chunk_name` by the CLI right after
    /// constructing the runtime, so the program's own top-level compile
    /// registers a `chunk_sources` entry the same way `load`/`loadfile` do -
    /// otherwise `debug.getinfo(1).source` on the running main chunk is nil.
    default_chunk_name: Option<Rc<Vec<u8>>>,
    /// `string.dump`'s process-local prototype registry. Sol's dump envelope
    /// starts with the canonical Lua 5.5 binary header and validates every
    /// byte of it, but its body is still an opaque key into this map rather
    /// than a portable serialization of `Proto` instructions. That permits
    /// faithful header/malformed-chunk behavior and same-process
    /// round-tripping while a real bytecode serializer remains outstanding.
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
    /// Canonical userdata identities used for host file handles. The payload
    /// lives in host-side I/O registries, while Lua sees a real full userdata
    /// instead of a table-shaped approximation.
    file_userdata: HashSet<sol_core::ObjectId>,
    c_warning_function: Option<c_api::LuaWarnFunction>,
    c_warning_data: *mut std::ffi::c_void,
    c_thread_states: RefCell<HashMap<u64, *mut c_api::lua_State>>,
    c_continuations: RefCell<HashMap<u64, c_api::CContinuation>>,
    c_upvalue_identities: RefCell<HashMap<(sol_core::ObjectId, usize), Box<u8>>>,
    native_libraries: Vec<c_api::NativeLibrary>,
    native_bridge_ids: HashMap<usize, sol_core::NativeCallableId>,
    next_native_function: u32,
    /// Reserved `HeapObject::NativeCallable` provider id for `LightUserdata`
    /// encoding (see `docs/features/table-closure-coroutine-cutover.md` §2).
    /// Minted once via `Heap::reserve_native_provider` at construction time -
    /// unlike `NativeFunction`/`GMatchIterator`, which reuse
    /// `canonical::LEGACY_STATE_PROVIDER`/provider `0`, nothing pre-flip ever
    /// put `LightUserdata` on `NativeCallable`, so it needs a genuinely fresh
    /// provider rather than a precedented shared one.
    light_userdata_provider: u32,
    /// Memoizes `NativeFunction` discriminant -> canonical `NativeCallable`
    /// object id (provider `0`, `function` = the discriminant), so
    /// `t[print] = 1; print(t[print])` round-trips to the same object
    /// identity instead of allocating a fresh one on every encode.
    native_function_objects: RefCell<HashMap<u32, sol_core::ObjectId>>,
    /// Reserves one real `HeapObject::NativeCallable` provider per distinct
    /// `RegisteredNative`'s legacy `NativeCallableId::provider`, for this
    /// `LuaRuntime`'s whole lifetime - legacy provider -> reserved real
    /// provider, and its inverse for decoding a `NativeCallable` object back
    /// into a `RegisteredNative`.
    registered_native_providers: RefCell<HashMap<u32, u32>>,
    registered_native_providers_reverse: RefCell<HashMap<u32, u32>>,
    /// Memoizes `NativeCallableId` -> canonical object id so the same
    /// registered native always encodes to the same identity.
    registered_native_objects: RefCell<HashMap<sol_core::NativeCallableId, sol_core::ObjectId>>,
    /// `string.dump`-style prototype identity used by `HeapObject::Closure`'s
    /// `prototype: u32` field - see `canonical::PrototypeRegistry`.
    prototype_registry: RefCell<canonical::PrototypeRegistry>,
    /// Owns each live `LuaCoroutine`'s executable state, addressed by its
    /// canonical `ThreadObject`'s `ObjectId` - see `canonical::CoroutineRegistry`.
    coroutine_registry: RefCell<canonical::CoroutineRegistry>,
    /// The single shared metatable every string value indexes through
    /// (`{ __index = string }`), matching real Lua's `G(L)->strmt`. Strings
    /// have no per-value metatable slot of their own - `getmetatable("")`
    /// returns this same table for every string, and mutating it (e.g.
    /// `getmetatable(""):__band = fn`, used by `bwcoercion.lua`-style shims
    /// to add metamethods to all strings) is visible to every subsequent
    /// string operation, since `index`/`metamethod` resolve strings through
    /// this table rather than a hardcoded reference straight to `string`.
    string_metatable: TableRef,
    /// The shared metatable for every number (`Integer`/`Float` alike, as
    /// real Lua uses one `LUA_TNUMBER` slot for both subtypes), settable
    /// only through `debug.setmetatable` since ordinary `setmetatable`
    /// rejects non-table values. `None` until first set, unlike
    /// `string_metatable` which always exists (strings get method dispatch
    /// by default; numbers don't).
    number_metatable: Option<TableRef>,
    /// The shared metatable for every boolean, mirroring `number_metatable`
    /// but for real Lua's single `LUA_TBOOLEAN` basic-type slot.
    boolean_metatable: Option<TableRef>,
    /// The shared metatable for `nil`, mirroring `number_metatable` but for
    /// real Lua's single `LUA_TNIL` basic-type slot.
    nil_metatable: Option<TableRef>,
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
    /// Set for the duration of `run_gc_finalizers`'s `__gc` invocation loop.
    /// Real Lua's collector is not reentrant: a finalizer that itself calls
    /// `collectgarbage("collect"/"step")` must not trigger a nested
    /// collection pass (`lua-5.5.1-tests/gc.lua`'s "check that the collector
    /// is not reentrant in incremental mode" test asserts the reentrant call
    /// returns `false` instead of doing any work).
    gc_finalizing: bool,
    /// Bookkeeping-only `collectgarbage("param", "pause"|"stepmul", ...)`
    /// values. Sol has no incremental step-size heuristics to tune, so
    /// these are stored and returned as-is purely so scripts that read back
    /// what they just set observe consistent values.
    gc_pause: i64,
    gc_stepmul: i64,
    /// Cumulative collection counters, exposed read-only via
    /// `debug.gcstats()` - see `gc::GcStats`'s own doc comment.
    gc_stats: gc::GcStats,
    /// Cumulative U8 inline-cache hit/miss/eviction counters, exposed
    /// read-only via `debug.icstats()` - see `ic::IcStats`'s own doc comment.
    ic_stats: std::cell::Cell<ic::IcStats>,
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
    io_stdout: TableRef,
    /// The handle `io.stderr` exposes - structurally identical to
    /// `io_stdout` (a lightweight `write`/`close`-method table, not a real
    /// open file), but `write_values_to_target` special-cases this table's
    /// identity to write straight to the process's real stderr stream
    /// instead of buffering into `self.output` (which is stdout-only
    /// captured output later returned by `take_output`). Real Lua programs
    /// (e.g. `lua-5.5.1-tests/heavy.lua`'s progress-reporting
    /// `io.stderr:write(...)` calls) expect stderr writes to be genuinely
    /// separate from stdout, not interleaved into it.
    io_stderr: TableRef,
    /// The handle `io.write`/`io.output()` (no arguments) currently target -
    /// `io_stdout` by default, or a table returned by `io.output(path)` once
    /// redirected to a real file (see `open_files`). Real Lua's default
    /// output file object; unlike `io_stdout`, this can change over the
    /// program's lifetime.
    default_output: RefCell<LuaValue>,
    /// Real, host-backed files opened by `io.output(path)` (there is no
    /// general `io.open` yet - only the default-output redirection
    /// `lua-5.5.1-tests/attrib.lua`'s `createfiles`/`removefiles` helpers
    /// need), keyed by the object identity (`TableRef::object_id().raw()`)
    /// of the lightweight table handle returned to Lua code for that file.
    /// `LuaValue` has no variant for an open file descriptor (see its doc
    /// comment - the bridge only carries pointer-free/scalar data), so the
    /// real `std::fs::File` lives only here, off to the side, and the handle
    /// table Lua code holds is just a `write`/`close`-method dispatch
    /// target that looks itself up in this map by object identity.
    open_files: RefCell<HashMap<u64, Rc<RefCell<std::fs::File>>>>,
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
    /// Absolute index in `frames` where the active Lua hook callback is
    /// installed. The hook dispatcher calls it directly, so it has no
    /// bytecode call site from which `debug.getinfo` could infer its name.
    hook_callback_frame: Option<usize>,
    /// Traceback line for a bytecode frame temporarily removed from
    /// `frames` while its line/count hook executes.
    hook_interrupted_frame: Option<String>,
    /// Active Lua frame omitted from `frames` while its line/count hook runs.
    hook_interrupted_info: Option<(Rc<crate::lua_bytecode::Proto>, ClosureRef, i64)>,
    hook_interrupted_locals: Option<frame::LuaFrame>,
    hook_transfer: Option<frame::HookTransfer>,
    /// Callee of a call hook fired from the blocking native call bridge.
    /// Such leaf calls have no `Frame::Native` entry on the explicit stack.
    hook_event_callee: Option<LuaValue>,
    /// U9 baseline-JIT state (see `dynjit`'s own module doc). Lazily built
    /// on the first call whose `Proto::call_count` crosses
    /// `dynjit::promote_threshold()` - not eagerly in the infallible
    /// `with_budgets` constructor, since most runtimes (short scripts, most
    /// of this crate's own tests) never reach that threshold and so never
    /// need to pay for a `cranelift_jit::JITModule`. See `DynJitState`.
    #[cfg_attr(not(feature = "jit"), allow(dead_code))]
    dynjit: dynjit::DynJitState,
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

/// Browser embedding counterpart to [`run_source`]. Modules are supplied by
/// the host and registered as exact `require` names; no filesystem or native
/// loader is enabled.
pub fn run_source_with_modules(
    source: &[u8],
    modules: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
) -> LuaResult<LuaRun> {
    let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).map_err(LuaError::new)?)
        .map_err(LuaError::new)?;
    let mut runtime = LuaRuntime::with_limits(10_000_000, 1_000);
    for (name, module_source) in modules {
        runtime.add_module(name, module_source);
    }
    match runtime.run(&program) {
        Ok(value) => Ok(LuaRun { value, output: runtime.take_output() }),
        Err(mut error) => {
            error.output = runtime.take_output();
            Err(error)
        }
    }
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
/// GC stress mode (see `LuaRuntime::set_gc_stress`) for the run. `chunk_name`,
/// when given, is registered as the top-level chunk's `debug.getinfo` source
/// (see `LuaRuntime::set_chunk_name`) - the `sol` CLI passes its `@`-prefixed
/// script path here so the running main chunk reports `source`/`short_src`
/// the same way a `load`ed chunk does.
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
    chunk_name: Option<Vec<u8>>,
) -> LuaResult<LuaRun> {
    let mut runtime = LuaRuntime::with_capabilities_and_budgets(
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
    );
    runtime.set_gc_stress(gc_stress);
    if let Some(chunk_name) = chunk_name {
        runtime.set_chunk_name(chunk_name);
    }
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
    chunk_name: Option<Vec<u8>>,
) -> LuaResult<LuaRun> {
    let mut runtime = LuaRuntime::with_capabilities_and_budgets(
        capabilities,
        instruction_budget,
        max_call_depth,
        allocation_budget,
    );
    runtime.set_gc_stress(gc_stress);
    if let Some(chunk_name) = chunk_name {
        runtime.set_chunk_name(chunk_name);
    }
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
