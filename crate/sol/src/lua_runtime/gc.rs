//! Garbage collection: allocation-budget charging (`charge_allocation`/
//! `tick`) and the `collectgarbage()`/`__gc` bridge onto `sol_core::Heap`'s
//! own tracing collector (`collect_major_with_roots`/`register_finalizer`/
//! `finish_finalizer`) - see
//! docs/features/table-closure-coroutine-cutover.md §3. The old `Rc`-
//! refcounting trial-deletion cycle collector this file used to contain is
//! gone now that `sol_core::Heap` traces real reachability itself; moving
//! the *trigger* sites (currently still the old per-instruction/per-
//! allocation heuristic below) to real dispatch-loop/native-trampoline
//! safepoints is task #12's scope, not this one's.
//!
//! Known gap, deferred to task #13 ("sol-core: conditional/deferred-root GC
//! hook for coroutine frames"): `frame_roots` below only walks the
//! currently-active dispatch/resume chain (`LuaRuntime::frames` plus
//! `main_coroutine`/`coroutine_stack`). A coroutine that is reachable from a
//! live `LuaValue::Thread` but is not currently part of that resume chain
//! has its own saved `LuaCoroutine::frames` left unrooted here - the same
//! gap `canonical::CoroutineRegistry`'s own doc comment already calls out as
//! not yet closed.

use sol_core::Value;

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
        let roots = self.frame_roots(active_frame);
        let collection = self
            .canonical_heap
            .borrow_mut()
            .collect_major_with_roots(&roots);
        if collection.finalizers.is_empty() {
            return;
        }
        // Every `self.call(finalizer, ...)` below drives its own nested
        // dispatch loop, whose collections only know about *its* active
        // frame and `self.frames` - neither includes `active_frame` here,
        // which (if `Some`) is this same outer, still-in-flight call's own
        // popped frame (see `pinned_roots`'s doc comment). Pin it for the
        // whole finalizer-invocation loop, not just the roots snapshot
        // above, so it survives any collection nested inside a finalizer.
        let mut pinned = Vec::new();
        if let Some(frame) = active_frame {
            self.push_lua_frame_roots(frame, &mut pinned);
        }
        self.pinned_roots.push(pinned);
        for id in collection.finalizers {
            // Only tables register a finalizer today (`table_set_metatable`
            // is the sole `register_finalizer` call site) - `CanonicalUserdata`
            // is a separate, permanently-self-rooted tier (see its own doc
            // comment in `value.rs`) that a `collect_major_with_roots` sweep
            // never reclaims through this queue in the first place, so no
            // `Userdata` case belongs here.
            let Ok(LuaValue::Table(table)) = self.decode_value(Value::object(id)) else {
                let _ = self.canonical_heap.borrow_mut().finish_finalizer(id);
                continue;
            };
            if let Some(finalizer) = self.table_finalizer(table) {
                let _ = self.call(finalizer, vec![LuaValue::Table(table)]);
            }
            let _ = self.canonical_heap.borrow_mut().finish_finalizer(id);
        }
        self.pinned_roots.pop();
    }

    /// Every `Value` reachable only through a `LuaRuntime`/`LuaFrame` Rust
    /// field rather than already-heap-linked storage - the `frame_roots`
    /// argument `collect_major_with_roots` needs per
    /// docs/features/table-closure-coroutine-cutover.md §3. Anything
    /// reachable *from* one of these (e.g. `string`/`math`/`table` hanging
    /// off the globals table) needs no separate entry: the collector traces
    /// outward from every root it's given.
    fn frame_roots(&self, active_frame: Option<&LuaFrame>) -> Vec<Value> {
        let mut roots = Vec::new();
        roots.push(self.encode_value(&self.globals.as_value()).unwrap_or(Value::NIL));
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
        // The active resume chain - see this module's own doc comment for
        // the not-currently-resumed-coroutine gap this leaves (task #13).
        for thread in std::iter::once(self.main_coroutine).chain(self.coroutine_stack.iter().copied()) {
            let coroutine = self.coroutine(thread);
            self.push_frame_stack_roots(&coroutine.frames.borrow(), &mut roots);
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
        roots
    }

    fn push_frame_stack_roots(&self, frames: &[Frame], roots: &mut Vec<Value>) {
        for frame in frames {
            match frame {
                Frame::Lua(lua_frame) => self.push_lua_frame_roots(lua_frame, roots),
                Frame::Native(cont) => self.push_native_cont_roots(cont, roots),
            }
        }
    }

    fn push_lua_frame_roots(&self, frame: &LuaFrame, roots: &mut Vec<Value>) {
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
