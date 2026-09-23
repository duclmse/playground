//! Lua coroutines: `LuaCoroutine`/`CoroutineStatus`, and the
//! `LuaRuntime` methods that create/resume/wrap them. Coroutines are driven
//! by the same `drive`/`step_result_for_call` trampoline as ordinary calls
//! (see `dispatch`), never a stackful fiber/OS thread - see `LuaCoroutine`'s
//! own doc comment.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;

use super::frame::*;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoroutineStatus {
    Suspended,
    Running,
    Normal,
    Dead,
}

impl CoroutineStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Suspended => "suspended",
            Self::Running => "running",
            Self::Normal => "normal",
            Self::Dead => "dead",
        }
    }
}

/// Which events a `debug.sethook` mask string (`"c"`/`"l"`/`"r"`/`"count"`
/// via a non-zero `count` argument) subscribes to - real Lua's
/// `lua_sethook`'s `LUA_MASKCALL`/`LUA_MASKLINE`/`LUA_MASKRET`/`LUA_MASKCOUNT`
/// bits.
#[derive(Clone, Copy, Default)]
pub(super) struct HookMask {
    pub(super) call: bool,
    pub(super) line: bool,
    pub(super) ret: bool,
    pub(super) count: bool,
}

/// One coroutine's installed `debug.sethook` callback - see `LuaCoroutine::hook`
/// and `LuaRuntime::fire_hook`. `count_remaining` is only meaningful when
/// `mask.count` is set: it counts bytecode instructions down from `count` to
/// 0 (real Lua's `L->hookcount`), independent of which Lua frame is
/// currently executing (unlike the per-frame `line`/`pc` tracking, which
/// lives on `LuaFrame` itself since each call frame tracks its own source
/// position).
pub(super) struct HookState {
    pub(super) callback: LuaValue,
    pub(super) mask: HookMask,
    pub(super) count: i64,
    pub(super) count_remaining: Cell<i64>,
}

/// A Lua coroutine: its own explicit call-frame stack (see `Frame`), driven
/// by exactly the same `LuaRuntime::drive`/`step_result_for_call` trampoline
/// as ordinary calls and every other builtin - not a stackful fiber/OS
/// thread. `resume` swaps `LuaRuntime::frames` for `frames` and drives it
/// until it yields or the body returns; `yield` suspends by escaping the
/// current `drive` invocation (`DriveOutcome::Yielded`) with every frame on
/// `frames` left completely intact, so a later `resume` can continue exactly
/// where it left off. Because this never switches native/OS stacks, it works
/// identically on `wasm32`, which has no fiber/thread-suspension primitive -
/// see Phase 5 in `docs/features/lua-superset-plan.md`.
///
/// Known, deliberate limitations: coroutines are not tracked by
/// `collect_cycles` (see `track_table`/`track_closure`) - a reference cycle
/// routed through a coroutine (e.g. a table holding a coroutine whose
/// function upvalue-captures that same table) leaks, the same conservative
/// class of gap as any other untracked type. Threads also can't yet be used
/// as table keys.
pub struct LuaCoroutine {
    pub(super) status: Cell<CoroutineStatus>,
    /// The callable to invoke on the very first `resume` - `Some` until
    /// then, `.take()`n at that point. Every resume after the first drives
    /// `frames` directly instead (see `LuaRuntime::resume_coroutine`).
    pub(super) body: RefCell<Option<LuaValue>>,
    /// This coroutine's own explicit Lua-call frame stack (see `Frame`),
    /// separate from `LuaRuntime::frames`. The very first resume pushes an
    /// uncharged `Frame::Native(NativeCont::Once)` marker here (mirroring
    /// `call_closure`'s uncharged base frame) before resolving `body` above
    /// it, so this is empty only before the coroutine has ever been resumed.
    /// Once started, it always holds at least that marker, even while
    /// suspended, which is also what makes "empty" an unambiguous
    /// never-started signal. `resume_coroutine` swaps this in for
    /// `LuaRuntime::frames` for the duration of each `resume()` call.
    pub(super) frames: RefCell<Vec<Frame>>,
    /// `LuaRuntime::call_depth` units charged by this coroutine's call chain
    /// that are still outstanding - i.e. correspond to frames still sitting
    /// on `frames` (paused across a yield, or mid-drive on the resuming
    /// call's own Rust stack). A plain call's equivalent bookkeeping is a
    /// `&mut usize` local (`drive`'s `depth_charged`) whose lifetime matches
    /// one Rust call-stack frame; a coroutine's must instead survive across
    /// separate top-level `resume_coroutine` invocations (no Rust stack
    /// frame survives between a yield and the next resume), so it lives
    /// here instead - loaded at the start of `resume_coroutine` and stored
    /// back at the end, whether this resume yielded, completed, or errored.
    pub(super) depth_charged: Cell<usize>,
    /// The error value this coroutine died with, if its body raised an
    /// uncaught error rather than returning normally. `coroutine.close`
    /// surfaces this once (`(false, error)`) on a dead coroutine, then
    /// clears it so a second `close` call reports `(true, nil)` like real
    /// Lua's idempotent close-of-an-already-closed-thread behavior.
    pub(super) dead_error: RefCell<Option<LuaValue>>,
    /// This coroutine's own `debug.sethook` state, `None` when no hook is
    /// installed. `Rc` so `LuaRuntime::active_hook` can hold a cheap clone of
    /// whichever coroutine is currently running without keeping this
    /// `RefCell` borrowed - see `active_hook`'s field doc and
    /// `resume_coroutine`'s swap-in/out of it.
    pub(super) hook: RefCell<Option<Rc<HookState>>>,
}

impl fmt::Debug for LuaCoroutine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LuaCoroutine({})", self.status.get().as_str())
    }
}

impl LuaRuntime {
    pub fn create_global_coroutine(&mut self, name: &str) -> LuaResult<Rc<LuaCoroutine>> {
        self.new_coroutine(self.globals.get(name))
    }

    pub fn resume_coroutine_outcome(
        &mut self,
        coroutine: &Rc<LuaCoroutine>,
        args: Vec<LuaValue>,
    ) -> sol_core::CallOutcome<LuaValue, LuaError> {
        match self.resume_coroutine(coroutine, args) {
            Ok(values) if coroutine.status.get() == CoroutineStatus::Suspended => {
                sol_core::CallOutcome::Yielded(values)
            }
            Ok(values) => sol_core::CallOutcome::Returned(values),
            Err(error) => sol_core::CallOutcome::Raised(error),
        }
    }

    pub(super) fn expect_coroutine(value: &LuaValue) -> LuaResult<Rc<LuaCoroutine>> {
        match value {
            LuaValue::Thread(co) => Ok(co.clone()),
            other => Err(LuaError::new(format!(
                "bad argument (coroutine expected, got {})",
                other.type_name()
            ))),
        }
    }

    /// Builds a new coroutine around `f` without starting it. `f` is invoked
    /// lazily on the very first `resume`; see `LuaCoroutine`'s doc comment
    /// for the overall design.
    pub(super) fn new_coroutine(&mut self, f: LuaValue) -> LuaResult<Rc<LuaCoroutine>> {
        self.charge_allocation(std::mem::size_of::<LuaCoroutine>())?;
        Ok(Rc::new(LuaCoroutine {
            status: Cell::new(CoroutineStatus::Suspended),
            body: RefCell::new(Some(f)),
            frames: RefCell::new(Vec::new()),
            depth_charged: Cell::new(0),
            dead_error: RefCell::new(None),
            hook: RefCell::new(None),
        }))
    }

    /// Resumes `co` with `args`, driving its own frame stack (see
    /// `LuaCoroutine`) until it yields or its body returns, via exactly the
    /// same `drive`/`step_result_for_call` trampoline used for every other
    /// call in this runtime.
    ///
    /// On the very first resume, `frames` is empty (see the `frames` field
    /// doc on `LuaCoroutine` for why that is an unambiguous "never started"
    /// signal): this pushes an *uncharged* `Frame::Native(NativeCont::Once)`
    /// marker - mirroring `call_closure`'s own uncharged base frame, whose
    /// charge instead lives in the enclosing `call()`'s `call_depth += 1`/
    /// `-= 1` wrapping - and then resolves `co.body` above/through it via
    /// `resolve_call` at `base_depth == 0`. Charging the base frame itself
    /// here (e.g. by calling `resolve_call` with no marker underneath it)
    /// would leak one `call_depth` unit permanently on every completed
    /// coroutine, since `finish_frame`'s "skip the decrement when
    /// `frames.len() == base_depth`" logic assumes the base frame's own push
    /// was never charged in the first place. Using the existing `Once`
    /// primitive for this - rather than a dedicated marker - also uniformly
    /// handles every legal body type (`Closure`, `NativeFunction`, `Native`
    /// bridge) through the same call path with no special-casing, and
    /// correctly threads resume values back even for a body that itself
    /// resolves to a bare `coroutine.yield` (`coroutine.create(coroutine.yield)`)
    /// or a bare `pcall` (`coroutine.create(pcall)`): `resolve_call` pushes
    /// nothing in that case, `Once` persists at the bottom of `frames`, and
    /// the next `resume`'s value is delivered to it exactly as any other
    /// call's return value would be.
    ///
    /// Every subsequent resume instead finds `co.body` already consumed and
    /// `frames` already holding that persistent `Once` marker (plus
    /// whatever was paused above it across the yield), and drives directly
    /// with `args` seeded as `initial_incoming` - the frame that issued the
    /// `coroutine.yield` call is still sitting there with its `pending`
    /// field already set from before, so it resumes exactly like any other
    /// completed call would.
    ///
    /// `depth_charged` persists this resume's `call_depth` bookkeeping
    /// across separate top-level `resume_coroutine` invocations, since no
    /// Rust stack frame survives between a yield and the next resume - it is
    /// loaded from `co.depth_charged` at the start and stored back at the
    /// end unconditionally (whether this resume yielded, completed, or
    /// errored).
    pub(super) fn resume_coroutine(
        &mut self,
        co: &Rc<LuaCoroutine>,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        match co.status.get() {
            CoroutineStatus::Dead => return Err(LuaError::new("cannot resume dead coroutine")),
            CoroutineStatus::Running => {
                return Err(LuaError::new("cannot resume non-suspended coroutine"))
            }
            CoroutineStatus::Normal => {
                return Err(LuaError::new("cannot resume non-suspended coroutine"))
            }
            CoroutineStatus::Suspended => {}
        }

        // The caller becomes "normal" (suspended because it resumed
        // something else) for the duration of this resume - including when
        // the caller is the main coroutine itself, which isn't tracked on
        // `coroutine_stack` (see `CoroutineRunning`'s matching fallback) but
        // still needs its `status` updated so e.g. `coroutine.close(main)`
        // from within a nested coroutine reports "normal" rather than
        // leaving `main`'s status stuck at its initial `running`.
        let parent = self
            .coroutine_stack
            .last()
            .cloned()
            .unwrap_or_else(|| self.main_coroutine.clone());
        parent.status.set(CoroutineStatus::Normal);
        co.status.set(CoroutineStatus::Running);
        self.coroutine_stack.push(co.clone());

        // Swap in this coroutine's own frame stack (and outstanding
        // call_depth charge) for the duration of the resume - see the
        // `frames`/`depth_charged` field docs on `LuaCoroutine` for why this
        // is required for correctness, not just an optimization.
        let caller_frames = std::mem::replace(&mut self.frames, co.frames.take());
        // `debug.sethook` is per-coroutine (`LuaCoroutine::hook`) - swap in
        // whichever hook `co` itself has installed for the duration of this
        // resume, same as `frames`, and restore the caller's own hook
        // afterward. See `active_hook`'s field doc on `LuaRuntime`.
        let caller_hook = std::mem::replace(&mut self.active_hook, co.hook.borrow().clone());
        let mut depth_charged = co.depth_charged.take();
        let base_depth = 0;

        let outcome = if self.frames.is_empty() {
            let body = co
                .body
                .borrow_mut()
                .take()
                .expect("resume_coroutine: body missing on a coroutine with empty frames");
            self.frames.push(Frame::Native(NativeCont::Once));
            match self.resolve_call(body, args, base_depth, &mut depth_charged) {
                // `Pending`/`Done` both still need the `Once` marker just
                // pushed to be popped and finished (`finish_frame`'s
                // `frames.len() == base_depth` check is what recognizes the
                // coroutine as complete) - feeding `Done`'s values back into
                // `drive` as `initial_incoming` lets the very same
                // `NativeCont::Once` handling in `drive`'s loop do that,
                // instead of duplicating it here. Only `Yielded` skips
                // `drive` entirely: it leaves `Once` deliberately unpopped,
                // exactly as it would if reached via the trampoline, so the
                // coroutine's next `resume` finds it still there.
                Ok(CallStep::Pending) => self.drive(base_depth, &mut depth_charged, None),
                Ok(CallStep::Done(values)) => {
                    self.drive(base_depth, &mut depth_charged, Some(values))
                }
                Ok(CallStep::Yielded(values)) => DriveOutcome::Yielded(values),
                Err(error) => DriveOutcome::Raised(error),
            }
        } else {
            // Continuing a coroutine that's suspended mid-`coroutine.yield`
            // call: that call's "call" hook event already fired (manually,
            // in `step_result_for_call`'s `CoroutineYield` intercept, since
            // it never reaches `self.call()`) - fire the matching "return"
            // here, now that this resume's `args` are what `yield` "returns".
            match self.fire_hook("return", None) {
                Ok(()) => self.drive(base_depth, &mut depth_charged, Some(args)),
                Err(error) => DriveOutcome::Raised(error),
            }
        };

        // On error, `drive`/`resolve_call` leave `frames` exactly as it was
        // when the error occurred instead of unwinding it - the same
        // contract `call_closure` relies on. `close_frames_above` both closes
        // any pending `<close>` values in the discarded frames (Lua 5.4+
        // scope-exit semantics) and truncates back to `base_depth` (0);
        // dropping the now-moot depth charge is purely for hygiene (an
        // un-truncated `depth_charged` would never be read again either).
        // Since the coroutine is about to be marked `Dead` and can never be
        // resumed again, a partially-unwound frame stack must never be left
        // somewhere a later resume could find it.
        let outcome = if let DriveOutcome::Raised(error) = outcome {
            self.call_depth -= depth_charged;
            depth_charged = 0;
            if error.uncatchable {
                // `coroutine.close()`'s self-close forced abort: not itself
                // an error (see the `uncatchable` field doc on `LuaError`) -
                // every discarded frame's `<close>` handler runs with a
                // `nil` `err` argument, exactly like a clean, non-error
                // close. Only becomes an error if a handler raises a new one
                // along the way (`what == "error"`); a handler that instead
                // tries to yield hits the same "attempt to yield across a
                // C-call boundary" any `<close>` handler does, since
                // `close_pending` invokes it through the blocking `call`
                // bridge either way.
                match self.close_frames_above_optional(base_depth, None) {
                    Some(error) => DriveOutcome::Raised(error),
                    None => DriveOutcome::Returned(Vec::new()),
                }
            } else {
                let error = self.close_frames_above(base_depth, error);
                DriveOutcome::Raised(error)
            }
        } else {
            outcome
        };

        *co.frames.borrow_mut() = std::mem::replace(&mut self.frames, caller_frames);
        self.active_hook = caller_hook;
        co.depth_charged.set(depth_charged);

        self.coroutine_stack.pop();
        parent.status.set(CoroutineStatus::Running);

        match outcome {
            DriveOutcome::Yielded(values) => {
                co.status.set(CoroutineStatus::Suspended);
                Ok(values)
            }
            DriveOutcome::Returned(values) => {
                co.status.set(CoroutineStatus::Dead);
                Ok(values)
            }
            DriveOutcome::Raised(error) => {
                co.status.set(CoroutineStatus::Dead);
                *co.dead_error.borrow_mut() = Some(error.clone().into_lua_value());
                Err(error)
            }
            DriveOutcome::TailCall(_) => {
                unreachable!("tail calls are consumed inside the dynamic trampoline")
            }
        }
    }

    /// `coroutine.wrap`'s call semantics: propagate an internal error as a
    /// real Lua error rather than the `(false, message)` pair `resume` uses.
    pub(super) fn call_coroutine_wrapper(
        &mut self,
        co: Rc<LuaCoroutine>,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        self.resume_coroutine(&co, args)
    }
}

impl LuaCoroutine {
    pub(super) fn main_thread() -> Rc<Self> {
        Rc::new(Self {
            status: Cell::new(CoroutineStatus::Running),
            body: RefCell::new(None),
            frames: RefCell::new(Vec::new()),
            depth_charged: Cell::new(0),
            dead_error: RefCell::new(None),
            hook: RefCell::new(None),
        })
    }
}
