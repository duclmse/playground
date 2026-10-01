//! Garbage collection: allocation-budget charging (`charge_allocation`/
//! `tick`) and the `collectgarbage()`/`__gc` bridge onto `sol_core::Heap`'s
//! own tracing collector (`collect_major_with_roots`/`register_finalizer`/
//! `finish_finalizer`) - see
//! docs/features/table-closure-coroutine-cutover.md §3. The old `Rc`-
//! refcounting trial-deletion cycle collector this file used to contain is
//! gone now that `sol_core::Heap` traces real reachability itself. Every
//! `charge_allocation`/`stress_collect_if_enabled`/`tick` call site outside
//! this file has been audited against `frame_roots`'s walk (task #12; see
//! docs/features/table-closure-coroutine-cutover.md §10): `tick`'s own
//! placement at the top of `dispatch_step`'s per-instruction loop already is
//! a safepoint by this module's own definition, and no other site holds a
//! live `TableRef`/`ClosureRef`/`ThreadRef` in an unrooted Rust local across
//! its own charge/collection point - none needed to move.
//!
//! `frame_roots` below roots every currently-registered coroutine's own
//! `frames`/`body`, but (task #13; see
//! docs/features/table-closure-coroutine-cutover.md §11) only the active
//! resume chain (`main_coroutine`/`coroutine_stack`) does so
//! unconditionally - every other registered coroutine is fed to
//! `sol_core::Heap::collect_major_with_conditional_roots` as a *conditional*
//! root, keyed by its own `ThreadObject` id, so its frame contents only
//! count once that id is independently reachable (task #12's empirical
//! stress testing had found the unconditional-only version of this walk
//! necessary first, before #13's conditional hook existed to do better; see
//! §10 for that interim state). This closes the last gap §6 named: a
//! coroutine kept "alive" only by a reference cycle routed through its own
//! frames is no longer rooted forever - it collects correctly once nothing
//! external points at it.

use std::collections::HashMap;

use sol_core::{ObjectId, Value};

use super::frame::*;
use super::*;

/// How many dispatched instructions (`tick`) trigger one automatic (non-
/// `gc_stress`) collection pass. Unchanged pacing from the old trial-
/// deletion collector's own constant of the same name - only the collector
/// underneath has changed.
const AUTO_GC_INSTRUCTION_INTERVAL: u64 = 65_536;

impl LuaRuntime {
    /// Enables/disables GC stress mode (see the `gc_stress` field doc) for
    /// this runtime. Off by default for every constructor; opt in
    /// explicitly (tests) or via `SOL_LUA_GC_STRESS=1` (the `sol` CLI, see
    /// `main.rs`).
    pub fn set_gc_stress(&mut self, enabled: bool) {
        self.gc_stress = enabled;
    }

    /// Runs the same work `collectgarbage("collect")` does - shared by the
    /// explicit `collectgarbage` native and, when `gc_stress` is enabled, by
    /// every allocation point and every dispatched instruction. `active_frame`
    /// is the frame currently being dispatched (see `tick`'s doc comment for
    /// why this can't just be read off `self.frames`), if any - `None` at
    /// every call site that isn't itself inside `dispatch_step`.
    fn stress_collect_if_enabled(&mut self, active_frame: Option<&LuaFrame>) {
        if self.gc_stress {
            self.collect_garbage_with(active_frame);
        }
    }

    /// `dispatch_step`'s own instruction loop holds `frame` as a plain local
    /// (popped off `self.frames` for the duration of the call, since
    /// mutating it in place while also calling back into `&mut self` for
    /// register/upvalue helpers would double-borrow) - so while a dispatch
    /// step is in flight, `frame_roots`'s walk of `self.frames` alone would
    /// miss every register, captured-local cell, and upvalue this exact
    /// frame is the only reference to. `frame` here is that popped frame;
    /// threading it through to `collect_garbage_with` keeps it rooted for
    /// every collection triggered from inside this loop, matching the
    /// `Rc`-refcounting liveness this frame used to get for free before the
    /// tables/closures/coroutines cutover (see
    /// docs/features/table-closure-coroutine-cutover.md §8 step 4).
    pub(super) fn tick(&mut self, frame: &LuaFrame) -> LuaResult<()> {
        if self.instructions_remaining == 0 {
            return Err(LuaError::new("Lua instruction budget exhausted"));
        }
        self.instructions_remaining -= 1;
        self.stress_collect_if_enabled(Some(frame));
        self.maybe_auto_collect(Some(frame));
        Ok(())
    }

    /// The production (non-`gc_stress`) default's automatic collection
    /// trigger - see `instructions_since_gc`'s field doc and
    /// `AUTO_GC_INSTRUCTION_INTERVAL` for the rationale and pacing. A no-op
    /// under `gc_stress` (already collects unconditionally via
    /// `stress_collect_if_enabled`) or while `collectgarbage("stop")` has
    /// disabled the collector.
    fn maybe_auto_collect(&mut self, active_frame: Option<&LuaFrame>) {
        if self.gc_stress || !self.gc_running {
            return;
        }
        self.instructions_since_gc += 1;
        if self.instructions_since_gc < AUTO_GC_INSTRUCTION_INTERVAL {
            return;
        }
        self.instructions_since_gc = 0;
        self.collect_garbage_with(active_frame);
    }

    /// `active_frame`: same meaning as `tick`'s own parameter - `Some` from
    /// the two call sites inside `dispatch_step` (`NewLocal`/`TForCall`),
    /// where a `gc_stress` collection would otherwise see the currently
    /// dispatching frame's registers/cells/upvalues as unreachable; `None`
    /// everywhere else, where the relevant frame (if any) is already back on
    /// `self.frames` (see `tick`'s doc comment).
    pub(super) fn charge_allocation(
        &mut self,
        bytes: usize,
        active_frame: Option<&LuaFrame>,
    ) -> LuaResult<()> {
        self.stress_collect_if_enabled(active_frame);
        if bytes > self.allocation_remaining {
            return Err(LuaError::new("Lua allocation budget exhausted"));
        }
        self.allocation_remaining -= bytes;
        Ok(())
    }

    /// `collectgarbage("count")`: a live snapshot of the canonical heap's
    /// currently retained bytes (see `sol_core::Heap::live_bytes`'s own
    /// doc), not a cumulative allocation counter.
    pub(super) fn live_heap_bytes(&self) -> usize {
        self.canonical_heap.borrow().live_bytes()
    }

    /// `collectgarbage("collect"/"step")`'s actual work: a full
    /// `sol_core::Heap::collect_major_with_roots` pass rooted at every
    /// `Value` reachable only through a `LuaRuntime`/`LuaFrame` Rust field
    /// (`frame_roots`), then running any `__gc` finalizer the collection
    /// queued. A finalizer error is discarded rather than propagated -
    /// `collectgarbage()` itself must not fail because of a broken `__gc`,
    /// matching the old trial-deletion collector's same behavior.
    pub(super) fn collect_garbage(&mut self) {
        self.collect_garbage_with(None);
    }

    /// `collect_garbage`, plus one extra frame's roots - see `tick`'s doc
    /// comment for why a collection triggered from inside `dispatch_step`
    /// needs this.
    fn collect_garbage_with(&mut self, active_frame: Option<&LuaFrame>) {
        let (roots, conditional_roots) = self.frame_roots(active_frame);
        // `live_bytes` covers the *whole* heap (globals, metatables, interned
        // strings, the registry, ...), not just what `charge_allocation` ever
        // charged against `allocation_budget` - resetting `allocation_remaining`
        // from an absolute `allocation_budget - live_bytes` would immediately
        // exhaust any small test/embedding budget against that fixed
        // overhead alone. Instead credit back exactly the *delta* this one
        // collection reclaimed (bounded so it can never exceed the original
        // budget), matching what the deleted trial-deletion collector's own
        // credit-back did: give back what was actually freed, not resync
        // against a metric `charge_allocation` never charged from in the
        // first place.
        let live_before = self.canonical_heap.borrow().live_bytes();
        let collection = self
            .canonical_heap
            .borrow_mut()
            .collect_major_with_conditional_roots(&roots, &conditional_roots);
        let live_after = self.canonical_heap.borrow().live_bytes();
        let reclaimed_bytes = live_before.saturating_sub(live_after);
        self.allocation_remaining = self
            .allocation_remaining
            .saturating_add(reclaimed_bytes)
            .min(self.allocation_budget);
        self.run_gc_finalizers(collection.finalizers, active_frame);
        self.prune_dead_coroutines();
    }

    /// A registered-but-no-longer-reachable coroutine's `ThreadObject` gets
    /// physically swept by the collection above like any other dead heap
    /// object, but `coroutine_registry`'s own entry for it is a side-table
    /// keyed by that same id, not a heap edge - nothing prunes it on its own,
    /// so it would otherwise sit in the map forever (`CoroutineRegistry`'s
    /// own doc comment already says the two are meant to go together; its
    /// `remove` was, until now, only ever called by a unit test).
    /// `depth_charged` needs no attention here: `resume_coroutine`'s
    /// `Yielded` arm already gives back a suspended coroutine's own charge to
    /// `call_depth` the moment control returns to its resumer (parking the
    /// amount in `co.depth_charged` purely so the *next* resume can restore
    /// it), so an unreachable coroutine's stored charge was never still
    /// outstanding against the shared counter in the first place.
    fn prune_dead_coroutines(&mut self) {
        let dead: Vec<ObjectId> = {
            let heap = self.canonical_heap.borrow();
            self.coroutine_registry
                .borrow()
                .entries()
                .filter(|(id, _)| !heap.contains(*id))
                .map(|(id, _)| id)
                .collect()
        };
        let mut registry = self.coroutine_registry.borrow_mut();
        for id in dead {
            registry.remove(ThreadRef::new(id));
        }
    }

    /// `collectgarbage("step", size)`'s actual work. Under `"generational"`
    /// mode (`self.gc_mode`), this is one complete, cheap young-generation-
    /// only pass via `sol_core::Heap::collect_minor_with_conditional_roots` -
    /// real Lua's own generational minor collection, run to completion in a
    /// single call rather than bounded/resumed, matching that function's own
    /// doc comment on why a minor cycle doesn't need incremental stepping.
    /// Otherwise (the `"incremental"` default), `size` bounds major-
    /// collection work via `sol_core::Heap::step_major_with_conditional_roots`,
    /// resuming whatever incremental cycle (if any) a prior `step_garbage`
    /// call left in progress. Returns whether this call finished the cycle -
    /// real Lua's own `collectgarbage("step", ...)` return value (a minor
    /// collection always finishes in the one call that ran it) - crediting
    /// back the reclaimed byte delta and running any `__gc` finalizers the
    /// same way `collect_garbage_with` does, once the cycle actually
    /// finishes (an in-progress major cycle never has any: see
    /// `step_major_with_conditional_roots`'s own doc comment).
    pub(super) fn step_garbage(&mut self, size: usize, active_frame: Option<&LuaFrame>) -> bool {
        let (roots, conditional_roots) = self.frame_roots(active_frame);
        let live_before = self.canonical_heap.borrow().live_bytes();
        let (finished, collection) = if self.gc_mode == "generational" {
            let collection = self
                .canonical_heap
                .borrow_mut()
                .collect_minor_with_conditional_roots(&roots, &conditional_roots);
            (true, collection)
        } else {
            self.canonical_heap
                .borrow_mut()
                .step_major_with_conditional_roots(size, &roots, &conditional_roots)
        };
        let live_after = self.canonical_heap.borrow().live_bytes();
        let reclaimed_bytes = live_before.saturating_sub(live_after);
        self.allocation_remaining = self
            .allocation_remaining
            .saturating_add(reclaimed_bytes)
            .min(self.allocation_budget);
        self.run_gc_finalizers(collection.finalizers, active_frame);
        if finished {
            self.prune_dead_coroutines();
        }
        finished
    }

    /// Every `self.call(finalizer, ...)` below drives its own nested
    /// dispatch loop, whose collections only know about *its* active frame
    /// and `self.frames` - neither includes `active_frame` here, which (if
    /// `Some`) is this same outer, still-in-flight call's own popped frame
    /// (see `pinned_roots`'s doc comment). Pin it for the whole
    /// finalizer-invocation loop, not just the roots snapshot the caller
    /// already took, so it survives any collection nested inside a
    /// finalizer. Shared by `collect_garbage_with` and `step_garbage`, whose
    /// only difference is how the just-finished `Collection`'s finalizer
    /// list was produced (a one-shot pass vs. one that finished on this
    /// particular step call).
    fn run_gc_finalizers(&mut self, finalizers: Vec<ObjectId>, active_frame: Option<&LuaFrame>) {
        if finalizers.is_empty() {
            return;
        }
        let mut pinned = Vec::new();
        if let Some(frame) = active_frame {
            self.push_lua_frame_roots(frame, &mut pinned);
        }
        self.pinned_roots.push(pinned);
        // Guard against a `__gc` finalizer that calls `collectgarbage()`
        // itself - see `gc_finalizing`'s field doc. Saved/restored rather
        // than unconditionally reset to `false` afterward in case this call
        // is itself already nested inside another finalizer invocation (a
        // finalizer's own call chain triggering an unrelated, later
        // collection that queues more finalizers of its own).
        let was_finalizing = self.gc_finalizing;
        self.gc_finalizing = true;
        for id in finalizers {
            // Only tables register a finalizer today (`table_set_metatable`
            // is the sole `register_finalizer` call site) - `CanonicalUserdata`
            // is a separate, permanently-self-rooted tier (see its own doc
            // comment in `value.rs`) that a collection sweep never reclaims
            // through this queue in the first place, so no `Userdata` case
            // belongs here.
            let Ok(LuaValue::Table(table)) = self.decode_value(Value::object(id)) else {
                let _ = self.canonical_heap.borrow_mut().finish_finalizer(id);
                continue;
            };
            if let Some(finalizer) = self.table_finalizer(table) {
                // This is a blocking runtime-to-Lua call rather than a
                // bytecode `Call` step, so tag the frame before
                // `call_closure` installs it.  `debug.getinfo(1)` in a
                // finalizer must identify the `__gc` metamethod itself.
                self.pending_frame_label = Some("__gc");
                let _ = self.call(finalizer, vec![LuaValue::Table(table)]);
                // A host-provided finalizer may be native and therefore not
                // enter `call_closure`; do not let its label escape to a
                // later, unrelated direct Lua call.
                self.pending_frame_label = None;
            }
            let _ = self.canonical_heap.borrow_mut().finish_finalizer(id);
        }
        self.gc_finalizing = was_finalizing;
        self.pinned_roots.pop();
    }

    /// Every `Value` reachable only through a `LuaRuntime`/`LuaFrame` Rust
    /// field rather than already-heap-linked storage - the `frame_roots`
    /// argument `collect_major_with_conditional_roots` needs per
    /// docs/features/table-closure-coroutine-cutover.md §3/§11. Anything
    /// reachable *from* one of these (e.g. `string`/`math`/`table` hanging
    /// off the globals table) needs no separate entry: the collector traces
    /// outward from every root it's given. The second element is the
    /// `conditional_roots` map: every registered coroutine NOT on the active
    /// resume chain has its frame contents listed there instead, keyed by
    /// its own `ThreadObject` id, so they only count once that id is
    /// independently reachable (see this function's coroutine-registry loop
    /// below and §11).
    fn frame_roots(
        &self,
        active_frame: Option<&LuaFrame>,
    ) -> (Vec<Value>, HashMap<ObjectId, Vec<Value>>) {
        let mut roots = vec![self.encode_value(&self.globals.as_value()).unwrap_or(Value::NIL)];
        // `require`/`package.searchpath` close over these two directly
        // (see `package_table`'s field doc), independent of whatever the
        // reassignable `package` global currently points to - and likewise
        // for the metatables/default file handles below, which dispatch and
        // the `io`/string-method natives consult directly rather than via a
        // global lookup.
        roots.push(Value::object(self.package_loaded.object_id()));
        roots.push(Value::object(self.package_table.object_id()));
        roots.push(Value::object(self.string_metatable.object_id()));
        for metatable in [
            self.number_metatable,
            self.boolean_metatable,
            self.nil_metatable,
        ]
        .into_iter()
        .flatten()
        {
            roots.push(Value::object(metatable.object_id()));
        }
        roots.push(Value::object(self.io_stdout.object_id()));
        roots.push(Value::object(self.io_stderr.object_id()));
        roots.push(
            self.encode_value(&self.default_output.borrow())
                .unwrap_or(Value::NIL),
        );
        // The genuinely-live nested-resume chain: `main_coroutine` plus every
        // coroutine currently suspended mid-`resume` waiting on a nested
        // `resume` further down `coroutine_stack`. These are real,
        // unconditional GC roots regardless of what points at them in Lua -
        // a coroutine actively on this chain is live by definition, the same
        // way a normal call stack's frames are (not in scope for task #13 to
        // change; see docs/features/table-closure-coroutine-cutover.md §11).
        // A coroutine currently being resumed has its `frames` swapped out
        // into `self.frames` for the duration (see `resume_coroutine`), so
        // walking both `self.frames` below and this loop never double-roots
        // or misses it. An *intermediate* coroutine on this chain (suspended
        // mid-`resume` waiting on a nested `resume` further down) has its own
        // real frame contents sitting in `resume_coroutine`'s own
        // `caller_frames` Rust local instead - invisible to both `frames`
        // here and to `self.frames` - so `resume_coroutine` pins them onto
        // `pinned_roots` for the nested call's duration (see its own comment
        // and §11); this loop only needs to cover `main_coroutine` and
        // whichever coroutine is currently innermost (already `self.frames`).
        for &thread in std::iter::once(&self.main_coroutine).chain(&self.coroutine_stack) {
            // The active thread object itself is live too. In particular,
            // debug's `_HOOKKEY` registry table has weak thread keys: its
            // callback entry must not disappear merely because the main
            // thread has no ordinary Lua value pointing back to itself.
            roots.push(Value::object(thread.object_id()));
            let coroutine = self.coroutine(thread);
            if let Some(body) = coroutine.body.borrow().as_ref() {
                roots.push(self.encode_value(body).unwrap_or(Value::NIL));
            }
            // A debug hook is an executable Lua value retained by the
            // coroutine's Rust-side `HookState`, not by a heap edge. Keep it
            // alive just like a suspended coroutine body: a collection may
            // run after `debug.sethook` returns but before the next event,
            // and the installed hook is still observable then.
            if let Some(hook) = coroutine.hook.borrow().as_ref() {
                roots.push(self.encode_value(&hook.callback).unwrap_or(Value::NIL));
            }
            self.push_frame_stack_roots(&coroutine.frames.borrow(), &mut roots);
        }
        // Every OTHER registered coroutine - merely created or suspended
        // between resumes, not on the active chain above - only counts as a
        // root *conditionally*, once its own `ThreadObject` id is
        // independently reachable (task #13; see §11). A coroutine reachable
        // only via a Lua local holding its `LuaValue::Thread` is already
        // covered by the ordinary root-tracing below (`push_lua_frame_roots`
        // walks live registers), which marks its `ThreadObject` id and so
        // correctly unlocks this entry; a coroutine kept "alive" only by a
        // reference cycle routed through its own frames is correctly never
        // marked, and so correctly collected instead of leaking forever.
        let mut conditional_roots = HashMap::new();
        let active_chain: std::collections::HashSet<ThreadRef> = std::iter::once(self.main_coroutine)
            .chain(self.coroutine_stack.iter().copied())
            .collect();
        for (id, coroutine) in self.coroutine_registry.borrow().entries() {
            if active_chain.contains(&ThreadRef::new(id)) {
                continue;
            }
            let mut extra = Vec::new();
            // Before its very first `resume`, a coroutine's body closure sits
            // only in this `RefCell`, not yet part of `frames` (see
            // `resume_coroutine`'s doc comment on why `body` is `.take()`n
            // exactly once) - unrooted here, it could otherwise be swept
            // between `coroutine.create` and the first `resume` that would
            // have called it.
            if let Some(body) = coroutine.body.borrow().as_ref() {
                extra.push(self.encode_value(body).unwrap_or(Value::NIL));
            }
            if let Some(hook) = coroutine.hook.borrow().as_ref() {
                extra.push(self.encode_value(&hook.callback).unwrap_or(Value::NIL));
            }
            self.push_frame_stack_roots(&coroutine.frames.borrow(), &mut extra);
            conditional_roots.insert(id, extra);
        }
        self.push_frame_stack_roots(&self.frames, &mut roots);
        // The frame `dispatch_step` popped off `self.frames` for the
        // duration of the instruction loop currently running - see `tick`'s
        // doc comment.
        if let Some(frame) = active_frame {
            self.push_lua_frame_roots(frame, &mut roots);
        }
        // Any outer, still-in-flight popped frame(s) pinned around a
        // reentrant finalizer call - see `pinned_roots`'s own doc comment.
        for segment in &self.pinned_roots {
            roots.extend(segment.iter().copied());
        }
        (roots, conditional_roots)
    }

    pub(super) fn push_frame_stack_roots(&self, frames: &[Frame], roots: &mut Vec<Value>) {
        for frame in frames {
            match frame {
                Frame::Lua(lua_frame) => self.push_lua_frame_roots(lua_frame, roots),
                Frame::Native(cont) => self.push_native_cont_roots(cont, roots),
            }
        }
    }

    pub(super) fn push_lua_frame_roots(&self, frame: &LuaFrame, roots: &mut Vec<Value>) {
        roots.push(Value::object(frame.closure.object_id()));
        for value in &frame.regs {
            roots.push(self.encode_value(value).unwrap_or(Value::NIL));
        }
        for cell in frame.cells.iter().flatten() {
            roots.push(Value::object(*cell));
        }
        for upvalue in &frame.upvals {
            roots.push(Value::object(*upvalue));
        }
        for value in &frame.varargs {
            roots.push(self.encode_value(value).unwrap_or(Value::NIL));
        }
        for value in &frame.to_close {
            roots.push(self.encode_value(value).unwrap_or(Value::NIL));
        }
    }

    fn push_native_cont_roots(&self, cont: &NativeCont, roots: &mut Vec<Value>) {
        match cont {
            NativeCont::Sort(state) => {
                roots.push(self.encode_value(&state.table).unwrap_or(Value::NIL));
                if let Some(comparator) = &state.comparator {
                    roots.push(self.encode_value(comparator).unwrap_or(Value::NIL));
                }
                // Every element being sorted is always present in
                // `sorter.values()` even mid-merge (a merge step clones
                // values out of their original slots into a scratch buffer
                // and writes results back progressively - it never removes
                // an element from `values` without another copy of it still
                // sitting in `values` somewhere), so the in-progress merge
                // buffer itself needs no separate root.
                for value in state.sorter.values() {
                    roots.push(self.encode_value(value).unwrap_or(Value::NIL));
                }
            }
            NativeCont::Gsub(state) => {
                roots.push(self.encode_value(&state.repl).unwrap_or(Value::NIL));
            }
            NativeCont::Pcall | NativeCont::Xpcall(_) | NativeCont::Once => {}
        }
    }
}
