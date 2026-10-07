//! The bytecode trampoline's own data types: paused-call continuations
//! (`Pending`, `*Resolution`), the explicit call-frame stack (`LuaFrame`,
//! `Frame`, `NativeCont`, `XCallStage`), reentrant-native resume state
//! (`SortState`/`SortOutcome`, `GsubState`/`GsubOutcome`), and the
//! trampoline step/drive/call outcomes (`StepResult`, `DriveOutcome`,
//! `CallStep`). See `dispatch` for the logic that drives these.

use std::rc::Rc;

use crate::lua_bytecode::Proto;
use sol_core::{FrameHeader, ValueCount};

use super::*;

/// What a paused `LuaFrame` still needs to do once the call it issued
/// produces a result, before resuming dispatch at the instruction after the
/// one that issued it. `Call` mirrors `Instr::Call`'s own register layout;
/// the rest are metamethod-dispatch continuations - `Instr::GetField`/
/// `GetIndex`/`SetField`/`SetIndex`/`Neg`/`BitNot`/`Binary`/`Len`/`TForCall`
/// used to call `__index`/`__newindex`/`__unm`/`__bnot`/arithmetic-comparison-
/// concat-bitwise metamethods/`__len` via the blocking `LuaRuntime::call`
/// bridge; now each records just enough to finish the instruction once the
/// (possibly `PushClosure`d) metamethod call's result comes back.
#[derive(Clone)]
pub(super) enum Pending {
    None,
    Call {
        base: usize,
        results: ValueCount,
    },
    TailCall,
    /// `__index` resolved to a function call; store its single result at
    /// `dest`.
    Index {
        dest: usize,
    },
    /// `__newindex` resolved to a function call; no register write.
    SetIndex,
    /// `__len` resolved to a function call; store its single result at
    /// `dest`.
    Len {
        dest: usize,
    },
    /// `__unm`/`__bnot` resolved to a function call; store its single result
    /// at `dest`.
    Unary {
        dest: usize,
    },
    /// A binary-operator metamethod resolved to a function call; store the
    /// (possibly boolean-converted, per `continuation`) result at `dest`.
    Binary {
        dest: usize,
        continuation: BinaryContinuation,
    },
    /// The generic-`for` iterator function call; write up to `nvars` results
    /// starting at `base + 3` (fresh cell identity per Lua's per-iteration
    /// loop-variable semantics, mirroring `ForPrep`/`ForLoop`).
    TForCall {
        base: usize,
        nvars: usize,
    },
}

/// How to turn a binary-metamethod call's raw result into the operator's
/// actual value - see `LuaRuntime::binary_resolve`.
#[derive(Clone, Copy)]
pub(super) enum BinaryContinuation {
    /// Arithmetic, concat, and bitwise metamethods: use the result as-is.
    Raw,
    /// `__eq`/`__lt`/`__le`: wrap the result's truthiness in a `Bool`.
    Bool,
    /// The `__lt`-via-swapped-args fallback for `Le`/`Ge`: wrap the negated
    /// truthiness in a `Bool`.
    BoolNegated,
}

/// Result of `LuaRuntime::index_resolve`: either the value was resolved
/// without needing a Lua call (raw hit, or a `Nil`/chain-exhausted miss), or
/// resolving it further requires calling a function-valued `__index`.
pub(super) enum IndexResolution {
    Value(LuaValue),
    Call {
        method: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// Result of `LuaRuntime::set_index_resolve` - see `IndexResolution`.
pub(super) enum SetIndexResolution {
    Done,
    Call {
        method: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// Result of `LuaRuntime::len_resolve` - see `IndexResolution`.
pub(super) enum LenResolution {
    Value(LuaValue),
    Call {
        method: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// Result of `LuaRuntime::unary_resolve` - see `IndexResolution`.
pub(super) enum UnaryResolution {
    Value(LuaValue),
    Call {
        method: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// Result of `LuaRuntime::binary_resolve` - see `IndexResolution`; `Call`
/// additionally carries how to turn the call's result into the operator's
/// value (`BinaryContinuation`).
pub(super) enum BinaryResolution {
    Value(LuaValue),
    Call {
        method: LuaValue,
        args: Vec<LuaValue>,
        continuation: BinaryContinuation,
    },
}

/// Heap-resident state for one in-progress bytecode `Proto` call - the
/// explicit-stack replacement for a native `run_proto` call frame. Unlike a
/// native Rust call frame, a `LuaFrame` can sit paused on `LuaRuntime::frames`
/// indefinitely (across separate `dispatch_step` calls) without holding any
/// Rust call stack, which is what lets `Instr::Call` chains be driven
/// iteratively instead of recursively.
#[derive(Clone)]
pub(super) struct LuaFrame {
    pub(super) debug_identity: u64,
    pub(super) header: FrameHeader,
    /// The exact closure invoked for this activation. Multiple closures can
    /// share a prototype, so `debug.getinfo(level, "f")` cannot reconstruct
    /// this identity from `proto` and `upvals` after the call begins.
    pub(super) closure: ClosureRef,
    pub(super) proto: Rc<Proto>,
    pub(super) upvals: Rc<[std::cell::Cell<sol_core::ObjectId>]>,
    pub(super) globals: Globals,
    pub(super) regs: Vec<LuaValue>,
    pub(super) cells: Cells,
    pub(super) varargs: Vec<LuaValue>,
    /// How many `__call` metamethod hops `step_result_for_call` resolved to
    /// reach this frame's closure (0 for a direct call against an actual
    /// `LuaValue::Closure`, matching real Lua's `ci->callstatus`'s
    /// `CIST_CCMT` bits - see `MAX_CALL_CHAIN`). This is exactly what
    /// `debug.getinfo(level, "t").extraargs` reports for this frame in real
    /// Lua 5.5: despite the name, `lua_Debug.extraargs` is *not* a vararg
    /// count (that's `ci->u.l.nextraargs`, used internally by `OP_VARARG`
    /// and never exposed through `debug.getinfo`) - it is
    /// `(ci->callstatus & MAX_CCMT) >> CIST_CCMT`, i.e. this hop count,
    /// confirmed against the real `lua5.5.1` oracle and `ldebug.c`'s
    /// `auxgetinfo`'s `'t'` case.
    pub(super) call_chain_hops: usize,
    /// This activation replaced its caller through `Instr::TailCall`.
    /// Lua preserves that fact for `debug.getinfo(..., "t")` even though the
    /// physical caller frame is no longer present.
    pub(super) is_tail_call: bool,
    pub(super) pending: Pending,
    /// Lua 5.4+ `<close>` support: values pushed by `Instr::MarkClose`, one
    /// per currently-open to-be-closed local (or a generic-for's implicit
    /// 4th iterator value), in declaration order. `Instr::CloseSlots` pops
    /// from the end (LIFO) on normal scope exit; an error or an unwind past
    /// this frame closes whatever remains the same way (innermost first).
    pub(super) to_close: Vec<LuaValue>,
    /// Set when this frame was pushed to run a `__index`/`__newindex`
    /// metamethod call (`"index"`/`"newindex"`) - see `LuaRuntime`'s
    /// `pending_frame_label` field. Lets an error raised directly inside the
    /// metamethod's own body be annotated "in metamethod '...'".
    pub(super) entry_label: Option<&'static str>,
    /// The `pc` (`hook_last_pc`) and source line (`hook_last_line`) a
    /// `debug.sethook` `"line"` hook last fired for on this frame, `-1`
    /// before the first instruction. Real Lua's line-hook firing rule
    /// (`luaG_traceexec`) fires again whenever the line actually changes, or
    /// whenever `pc` is at or before this - a loop jumping back to re-execute
    /// an earlier (possibly identical) line - which is why this is tracked
    /// per-frame rather than as one global "last line" (each call frame has
    /// its own independent notion of "have I already hooked this line").
    pub(super) hook_last_pc: i64,
    pub(super) hook_last_line: i64,
    pub(super) c_hook_last_pc: i64,
    pub(super) c_hook_last_line: i64,
    pub(super) debugger_last_pc: Option<u32>,
    pub(super) debugger_last_line: Option<u32>,
}

/// One entry of the explicit call stack. `Native` holds a reentrant native
/// builtin's resume state (so far: `pcall`/`xpcall`, `pairs`'s `__pairs`
/// fallback, `dofile`, `table.sort`'s comparator - including its default
/// `__lt`-metamethod path - `string.gsub`'s function/closure-replacement
/// path, and a coroutine's own top-level body invocation, see
/// `NativeCont::Once` and `LuaRuntime::resume_coroutine`) that has issued a
/// Lua call through the same trampoline as ordinary calls instead of
/// blocking through native Rust recursion. The blocking `LuaRuntime::call`
/// bridge remains only for calls made directly by native Rust code outside
/// this trampoline (e.g. a `__tostring`/`__gc` metamethod call, or a leaf
/// native that calls back into `self.call()` itself) - every builtin,
/// including coroutines, is fully trampolined when reached the ordinary way.
pub(super) enum Frame {
    Lua(LuaFrame),
    Native(NativeCont),
}

/// Values exposed by `debug.getinfo(..., "r")` and `debug.getlocal` during a
/// call/return hook. `first` is Lua's one-based transfer slot, not a register
/// index; native functions may return an existing argument slot (`select`).
#[derive(Clone)]
pub(super) struct HookTransfer {
    pub(super) first: usize,
    pub(super) values: Vec<LuaValue>,
    /// Empty for Lua call parameters, whose normal lexical names remain visible.
    pub(super) temporary_name: &'static [u8],
}

/// Resume state for a reentrant native builtin whose Lua call has been
/// pushed onto the frame-stack trampoline (see `LuaRuntime::push_native_call`).
/// A marker frame like this sits below the call it issued; `LuaRuntime::drive`
/// resumes it (wrapping the call's result appropriately) once that call
/// completes, and `LuaRuntime::unwind_error_to_marker` searches for one of
/// these (specifically `Pcall` or `Xpcall(XCallStage::Function { .. })`) to
/// convert an error raised anywhere inside the call's dynamic extent into
/// `pcall`/`xpcall`'s normal `(false, ...)` return instead of propagating it
/// further, exactly like real Lua's protected-call boundary. `Sort`/`Gsub`
/// markers are never matched by that search - an error raised inside a
/// `table.sort` comparator or a `string.gsub` replacement is not caught by
/// the sort/gsub itself, only by an enclosing `pcall`/`xpcall`, exactly like
/// real Lua - so an error unwinding past one is simply drained along with
/// everything else between the raise point and the matched marker.
pub(super) enum NativeCont {
    /// `pcall(f, ...)`.
    Pcall,
    /// `xpcall(f, handler)`.
    Xpcall(XCallStage),
    /// A native builtin that just needs to make one call and hand its
    /// result back verbatim, with no wrapping and no protected-call
    /// boundary of its own - `pairs`'s `__pairs` fallback (calls `method`
    /// once), `dofile` (calls the compiled chunk once), and a coroutine's
    /// own top-level body invocation (`LuaRuntime::resume_coroutine` pushes
    /// this, uncharged, as the permanent bottom of a fresh coroutine's frame
    /// stack before resolving its body above it - see `LuaCoroutine::frames`).
    /// An error from the call is not caught here; it propagates past this
    /// marker exactly like an error from any other unprotected call would -
    /// for a coroutine body this is exactly right, since an uncaught error
    /// inside a coroutine must propagate out of `resume`/`wrap`, not be
    /// silently swallowed by its own top-level marker.
    Once,
    /// In-memory require loader; cache publication and cleanup happen when
    /// the module's live Lua frame returns or is unwound.
    Require { name: Vec<u8>, key: LuaValue },
    /// `table.sort`'s in-progress TimSort - see `SortState`/
    /// `LuaRuntime::sort_step`. Resumed with the just-completed comparison
    /// call's boolean result (`less`).
    Sort(SortState),
    /// `string.gsub`'s in-progress function/closure-replacement scan - see
    /// `GsubState`/`LuaRuntime::gsub_step`. Resumed with the just-completed
    /// replacement call's single return value.
    Gsub(GsubState),
}

/// Which of `xpcall`'s two calls a `NativeCont::Xpcall` marker is waiting on.
pub(super) enum XCallStage {
    /// Waiting on `f`. Carries `handler` so an error raised by `f` (or
    /// anything `f` calls) can be redirected into calling `handler` instead
    /// of propagating past this marker. `entry_retry_depth` is
    /// `xcall_retry_depth` as of this marker's creation - real Lua's
    /// `luaD_pcall` saves `nCcalls` the same way (`oldnCcalls`) and restores
    /// it, unconditionally, whenever this protected call's error is finally
    /// caught (see `unwind_error_to_marker`'s `Xpcall` arms and `drive`'s own
    /// success arms for the two places that restore point is reached) -
    /// otherwise every message-handler retry below would permanently inflate
    /// the budget for calls made after this `xpcall` returns.
    Function {
        handler: LuaValue,
        entry_retry_depth: usize,
    },
    /// Waiting on `handler`, called because `f` (or a previous retry of
    /// `handler` itself) raised an error. Real Lua's `luaG_errormsg`
    /// unconditionally re-invokes `L->errfunc` for an error raised while
    /// already running it, bounded only by a hard cutoff
    /// (`lstate.c`'s `luaE_checkcstack`, `LUAI_MAXCCALLS/10*11`) past which
    /// it gives up with `"error in error handling"` instead of retrying -
    /// see `unwind_error_to_marker`'s matching arm. Carries `handler` again
    /// so a retry has it to call, and the same `entry_retry_depth` as the
    /// originating `Function` stage, threaded through unchanged by every
    /// retry (it is a single save point for this whole `xpcall`, not
    /// re-captured per retry).
    Handler {
        handler: LuaValue,
        entry_retry_depth: usize,
    },
}

/// Resume state for an in-progress `table.sort` TimSort, driven one
/// comparison at a time by `LuaRuntime::sort_step` instead of blocking on
/// native Rust recursion for each comparator call (or, in the no-explicit-
/// comparator case, each `__lt` metamethod call). The algorithm state lives
/// in the dedicated `tim_sort` module and can sit on `LuaRuntime::frames`
/// between comparisons.
pub(super) struct SortState {
    /// The original table argument (a plain table or a `__index`/
    /// `__newindex`/`__len` proxy) - kept as the argument `LuaValue`, not a
    /// raw `RcRef<LuaTable>`, so the write-back on completion can go
    /// through `LuaRuntime::index_set` and respect a proxy's metamethods
    /// instead of splicing a raw table's array part directly.
    pub(super) table: LuaValue,
    pub(super) comparator: Option<LuaValue>,
    pub(super) sorter: super::tim_sort::TimSort<LuaValue>,
}

/// What `LuaRuntime::sort_step` produced for the state it just advanced.
pub(super) enum SortOutcome {
    /// The sort finished; `values` has already been written back into the
    /// table's array part. Always empty (`table.sort` returns nothing) -
    /// carried as a `Vec` only so callers can treat this uniformly with
    /// every other call-result path.
    Done(Vec<LuaValue>),
    /// The next comparison needs a Lua call (an explicit comparator, or a
    /// `__lt` metamethod the default comparator resolved to) before the sort
    /// can continue.
    NeedsCall {
        callee: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// Resume state for an in-progress `string.gsub` scan against a callable
/// (`Closure`/`NativeFunction`/`GMatchIterator`/`CoroutineWrapper`)
/// replacement, driven one match at a time by `LuaRuntime::gsub_step`
/// instead of blocking on native Rust recursion for each replacement call.
/// Mirrors the original blocking scan loop's `pos`/`count`/`output` locals
/// exactly; `pending` additionally carries the in-flight match's
/// `(start, end)` byte range between issuing the replacement call and
/// resuming with its result, since the original loop had no need to
/// remember that across a (previously synchronous) call.
pub(super) struct GsubState {
    pub(super) source: CanonicalString,
    pub(super) body: Vec<u8>,
    pub(super) anchored: bool,
    pub(super) max: usize,
    pub(super) repl: LuaValue,
    pub(super) pos: usize,
    pub(super) count: usize,
    pub(super) changed: bool,
    pub(super) output: Vec<u8>,
    pub(super) last_match_end: Option<usize>,
    pub(super) pending: Option<(usize, usize)>,
}

/// What `LuaRuntime::gsub_step` produced for the state it just advanced.
pub(super) enum GsubOutcome {
    /// The scan finished; `output`/`count` are `string.gsub`'s two return
    /// values (still needing conversion to `LuaValue`s at the call site).
    Done(LuaValue, usize),
    /// A match was found and needs a replacement call before the scan can
    /// continue.
    NeedsCall {
        callee: LuaValue,
        args: Vec<LuaValue>,
    },
}

/// What `LuaRuntime::dispatch_step` produced for the frame it just ran.
pub(super) enum StepResult {
    DebugSemantic(u8, Vec<LuaValue>),
    DebugResume(DebugResumeRequest),
    /// Embedding debugger suspension before the next instruction. Unlike a
    /// Lua yield, no pending call results are created or consumed.
    DebugPause,
    /// The frame returned; these are its final result values.
    Done(Vec<LuaValue>),
    /// The frame issued `Instr::Call` against a `LuaValue::Closure` callee -
    /// push a fresh `LuaFrame` for it onto `LuaRuntime::frames` and drive
    /// that instead, with no new native Rust call frame.
    PushClosure {
        closure: ClosureRef,
        proto: Rc<Proto>,
        upvals: Rc<[std::cell::Cell<sol_core::ObjectId>]>,
        globals: Globals,
        args: Vec<LuaValue>,
        /// See `LuaFrame::call_chain_hops`.
        call_chain_hops: usize,
    },
    /// Proper tail call against a Lua closure. The driver replaces the
    /// current frame without increasing semantic call depth.
    TailClosure {
        closure: ClosureRef,
        proto: Rc<Proto>,
        upvals: Rc<[std::cell::Cell<sol_core::ObjectId>]>,
        globals: Globals,
        args: Vec<LuaValue>,
        /// See `LuaFrame::call_chain_hops`.
        call_chain_hops: usize,
    },
    /// The frame issued `Instr::Call` against a non-`Closure` callee (native
    /// function, bridge, coroutine wrapper, `__call` metamethod, ...) -
    /// dispatch it through the existing blocking `LuaRuntime::call`.
    CallLeaf {
        callee: LuaValue,
        args: Vec<LuaValue>,
    },
    /// The frame called a reentrant native builtin that itself needs to make
    /// a further Lua call (so far: `pcall`/`xpcall`, `table.sort`,
    /// `string.gsub`) - push `cont` as a `Frame::Native` marker below a call
    /// to `callee`, so an error raised anywhere inside that call's dynamic
    /// extent (not just by `callee` itself) can be caught at the marker
    /// instead of propagating further.
    CallNative {
        cont: NativeCont,
        callee: LuaValue,
        args: Vec<LuaValue>,
    },
    /// `step_result_for_call` fully resolved the call without needing to
    /// push any frame or make any Lua call at all - e.g. `table.sort` with
    /// no metamethods anywhere in its comparisons, or `string.gsub` against
    /// a non-callable replacement. Delivered back to whoever issued the call
    /// exactly as if a `CallLeaf` had just returned these values; unlike
    /// every other variant here, this one never touches `LuaRuntime::frames`.
    Resolved(Vec<LuaValue>),
    /// The frame called `coroutine.yield`. Unlike every other variant, this
    /// does not represent a call to dispatch or a result to deliver - it
    /// must escape the entire enclosing `LuaRuntime::drive` invocation
    /// (`DriveOutcome::Yielded`), leaving every frame above `base_depth`
    /// exactly as it is (still charged, not finished) so a later `resume`
    /// can continue from exactly this point. See `step_result_for_call` and
    /// `LuaRuntime::drive`.
    Yield(Vec<LuaValue>),
}

/// What driving a frame stack (`LuaRuntime::drive`) produced: either the
/// base frame ran all the way to completion (`Done`), or execution hit
/// `coroutine.yield` somewhere above `base_depth` and escaped instead
/// (`Yielded`) - everything above `base_depth` is left exactly as it was,
/// still charged in `call_depth`, ready for a later `drive` call (the next
/// `resume`) to continue. `LuaRuntime::call_closure` (the blocking-bridge
/// entry point) treats `Yielded` as a genuine error ("attempt to yield
/// across a C-call boundary"), matching real Lua; `LuaRuntime::resume_coroutine`
/// is the only caller that treats it as a normal, expected outcome.
pub(super) type DriveOutcome = sol_core::CallOutcome<LuaValue, LuaError>;

pub(super) struct DebugResumeRequest {
    pub thread: ThreadRef,
    pub args: Vec<LuaValue>,
    pub wrapped: bool,
}

/// What resolving a call (`LuaRuntime::resolve_call`/`push_native_call`)
/// produced, for callers that need to distinguish "nothing is ready yet, a
/// child frame/marker was pushed" from "this resolved synchronously to a
/// result" from "this call was actually `coroutine.yield` escaping" -
/// `Pending` alone would be ambiguous between the first and third cases
/// (both leave nothing usable in `incoming` right now), and conflating the
/// second and third would deliver a yielded value to whatever pushed the
/// call as if it had returned normally.
pub(super) enum CallStep {
    /// A child `Frame::Lua`/`Frame::Native` was pushed above whatever the
    /// caller already pushed; nothing is resolved yet - the trampoline's own
    /// loop will pick it up on its next iteration.
    Pending,
    /// The call resolved to a result with no further Lua reentry needed.
    Done(Vec<LuaValue>),
    /// The call was actually `coroutine.yield`; propagate this straight up
    /// as a `DriveOutcome::Yielded` from the nearest enclosing `drive`.
    Yielded(Vec<LuaValue>),
}
