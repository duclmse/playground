//! `LuaValue`-level table operations over canonical `TableRef` handles: thin
//! wrappers around `sol_core::Heap`'s raw `table_get`/`table_set`/`table_len`/
//! `table_next` (see `value.rs`'s `TableRef`) that additionally run every key
//! and value through the codec (`codec.rs`), since a `TableRef` only ever
//! stores `sol_core::Value`, never a `LuaValue` directly. These replace the
//! old `impl LuaTable` methods that operated on a table's own private
//! `array`/`hash` fields directly - see
//! docs/features/table-closure-coroutine-cutover.md §8 step 4.
//!
//! All of this is raw (`rawget`/`rawset`-equivalent, no `__index`/`__newindex`
//! metamethod resolution); that stays a `dispatch.rs`/natives-level concern
//! layered on top, exactly as it was over the old `LuaTable::get`/`set`.

use sol_core::{HeapObject, Value};

use crate::lua_bytecode::Proto;

use super::canonical::{GMATCH_ITERATOR_FUNCTION, LEGACY_STATE_PROVIDER};
use super::frame::*;
use super::*;

/// Approximate `size_of::<sol_core::TableObject>()`'s empty-table baseline
/// (an empty `Vec`/`IndexMap` plus the metatable/weak-mode fields) - the old
/// `std::mem::size_of::<LuaTable>()` charge this replaces was itself only
/// ever a rough per-allocation budget estimate, not a byte-exact accounting,
/// so a fixed constant here (rather than measuring the now-gone `LuaTable`)
/// preserves that same "rough charge" intent.
const TABLE_ALLOCATION_CHARGE: usize = 64;

/// Approximate `size_of::<sol_core::ClosureObject>()`'s baseline, same
/// rationale as `TABLE_ALLOCATION_CHARGE` above (replaces the old
/// `std::mem::size_of::<LuaClosure>()` charge).
const CLOSURE_ALLOCATION_CHARGE: usize = 48;

impl LuaRuntime {
    /// Allocates a fresh, empty table and charges it against the allocation
    /// budget. Replaces every old `self.track_table(Rc::new(RefCell::new(LuaTable::default())))`
    /// call site. `active_frame`: same meaning as `charge_allocation`'s own
    /// parameter - `Some` from `Instr::NewTable`'s handler (the sole
    /// `dispatch_step`-internal call site), `None` everywhere else.
    pub(super) fn new_table(&mut self, active_frame: Option<&LuaFrame>) -> LuaResult<TableRef> {
        self.charge_allocation(TABLE_ALLOCATION_CHARGE, active_frame)?;
        Ok(TableRef::alloc(&mut self.canonical_heap.borrow_mut()))
    }

    /// Allocates a closure. `upvalues` are the real, `UpvalSource`-resolved
    /// captured cells (see `Instr::NewClosure`); this additionally appends
    /// one synthetic trailing upvalue cell holding `globals.as_value()` to
    /// serve as `ClosureObject::environment` - canonical `Heap::alloc_closure`
    /// requires `environment < upvalues.len()` unconditionally, but Lua-compat
    /// closures don't treat `_ENV` as an ordinary captured upvalue (real
    /// global access goes through `Instr::GetEnvironment`/`SetEnvironment`
    /// and this runtime's own `Globals`/`closure_globals`, see
    /// `value.rs`'s `Globals` doc comment), so this slot exists purely to
    /// satisfy that invariant and give the environment table a real GC root
    /// through the closure itself, distinct from `closure_globals`'s own
    /// interim-fallback reachability.
    pub(super) fn new_closure(
        &mut self,
        proto: Rc<Proto>,
        mut upvalues: Vec<sol_core::ObjectId>,
        globals: Globals,
        active_frame: Option<&LuaFrame>,
    ) -> LuaResult<ClosureRef> {
        self.charge_allocation(CLOSURE_ALLOCATION_CHARGE, active_frame)?;
        let prototype = self.prototype_registry.borrow_mut().intern(&proto);
        let environment_value = self.encode_value(&globals.as_value())?;
        let environment = upvalues.len();
        {
            let mut heap = self.canonical_heap.borrow_mut();
            let environment_cell = heap.alloc_upvalue(environment_value, None);
            upvalues.push(environment_cell);
        }
        let closure = {
            let mut heap = self.canonical_heap.borrow_mut();
            ClosureRef::alloc(&mut heap, prototype, upvalues, environment)
                .map_err(|error| LuaError::new(format!("internal error: {error}")))?
        };
        self.closure_globals
            .borrow_mut()
            .insert(closure.object_id(), globals);
        Ok(closure)
    }

    /// A closure's own `(prototype, upvalue cell ids, globals)`, resolved
    /// off the canonical heap and `prototype_registry`/`closure_globals`
    /// (see `new_closure`'s own doc comment for how these are populated at
    /// allocation time). Replaces every old `closure.proto`/`closure.upvals`/
    /// `closure.globals` field access - `ClosureRef` itself carries no
    /// fields, only an `ObjectId`.
    pub(super) fn closure_parts(
        &self,
        closure: ClosureRef,
    ) -> LuaResult<(Rc<Proto>, Vec<sol_core::ObjectId>, Globals)> {
        let proto = self.closure_prototype(closure)?;
        let upvalues = self.closure_upvalues(closure)?;
        let globals = self.globals_for_closure(closure);
        Ok((proto, upvalues, globals))
    }

    pub(super) fn closure_prototype(&self, closure: ClosureRef) -> LuaResult<Rc<Proto>> {
        let prototype = {
            let heap = self.canonical_heap.borrow();
            match heap.object(closure.object_id()) {
                Ok(HeapObject::Closure(object)) => object.prototype,
                _ => return Err(LuaError::new("internal error: closure object missing")),
            }
        };
        self.prototype_registry
            .borrow()
            .resolve(prototype)
            .cloned()
            .ok_or_else(|| LuaError::new("internal error: closure prototype not interned"))
    }

    pub(super) fn closure_upvalues(&self, closure: ClosureRef) -> LuaResult<Vec<sol_core::ObjectId>> {
        let heap = self.canonical_heap.borrow();
        match heap.object(closure.object_id()) {
            Ok(HeapObject::Closure(object)) => Ok(object.upvalues.clone()),
            _ => Err(LuaError::new("internal error: closure object missing")),
        }
    }

    /// The upvalue cell id captured at `index`, if `closure` has one there.
    /// Used by `debug.getupvalue`/`setupvalue`/`upvalueid` for per-cell
    /// access rather than the whole list.
    pub(super) fn closure_upvalue_cell(
        &self,
        closure: ClosureRef,
        index: usize,
    ) -> Option<sol_core::ObjectId> {
        let heap = self.canonical_heap.borrow();
        match heap.object(closure.object_id()) {
            Ok(HeapObject::Closure(object)) => object.upvalues.get(index).copied(),
            _ => None,
        }
    }

    pub(super) fn globals_for_closure(&self, closure: ClosureRef) -> Globals {
        self.closure_globals
            .borrow()
            .get(&closure.object_id())
            .cloned()
            .expect("every closure allocated via new_closure has a closure_globals entry")
    }

    /// Rebinds one of `closure`'s upvalue slots to a different upvalue cell
    /// id - `debug.upvaluejoin`'s primitive, aliasing two closures onto the
    /// same shared cell.
    pub(super) fn closure_set_upvalue_cell(
        &self,
        closure: ClosureRef,
        index: usize,
        cell: sol_core::ObjectId,
    ) -> LuaResult<()> {
        self.canonical_heap
            .borrow_mut()
            .set_closure_upvalue(closure.object_id(), index, cell)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))
    }

    /// Reads an upvalue cell's current value, decoded.
    pub(super) fn upvalue_get(&self, cell: sol_core::ObjectId) -> LuaResult<LuaValue> {
        let value = self
            .canonical_heap
            .borrow()
            .upvalue_value(cell)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))?;
        self.decode_value(value)
    }

    /// Writes an upvalue cell's value in place.
    pub(super) fn upvalue_set(&self, cell: sol_core::ObjectId, value: LuaValue) -> LuaResult<()> {
        let encoded = self.encode_value(&value)?;
        self.canonical_heap
            .borrow_mut()
            .set_upvalue(cell, encoded)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))
    }

    pub(super) fn table_get(&self, table: TableRef, key: &LuaValue) -> LuaResult<LuaValue> {
        let key = self.encode_value(key)?;
        let value = {
            let heap = self.canonical_heap.borrow();
            table.get(&heap, key)
        };
        self.decode_value(value)
    }

    pub(super) fn table_set(
        &self,
        table: TableRef,
        key: LuaValue,
        value: LuaValue,
    ) -> LuaResult<()> {
        let key = self.encode_value(&key)?;
        let value = self.encode_value(&value)?;
        let mut heap = self.canonical_heap.borrow_mut();
        table
            .set(&mut heap, key, value)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))
    }

    /// Charges a genuinely new table entry (a key with no previously-live
    /// value on the table) against the allocation budget - matches the old
    /// `Rc`-based collector's `charge_new_table_entry`, minus the credit-back
    /// on reclaim (resetting `allocation_remaining` from reclaimed bytes is
    /// task #12's scope, not this flip's - see `mod.rs`'s `allocation_budget`
    /// field doc). Table *creation* already charges `TABLE_ALLOCATION_CHARGE`
    /// for the fixed header (`new_table`); this covers the otherwise-
    /// unbounded per-field growth a loop like `for i = 1, math.huge do
    /// t[i] = i end` drives, which would otherwise run unmetered until only
    /// the instruction budget stopped it. `active_frame`: same meaning as
    /// `charge_allocation`'s own parameter.
    pub(super) fn charge_new_table_entry(
        &mut self,
        active_frame: Option<&LuaFrame>,
    ) -> LuaResult<()> {
        self.charge_allocation(2 * std::mem::size_of::<LuaValue>(), active_frame)
    }

    pub(super) fn table_len(&self, table: TableRef) -> usize {
        let heap = self.canonical_heap.borrow();
        table.len(&heap)
    }

    /// `next(t, key)`, decoded. `key == LuaValue::Nil` starts iteration.
    pub(super) fn table_next(
        &self,
        table: TableRef,
        key: &LuaValue,
    ) -> LuaResult<Option<(LuaValue, LuaValue)>> {
        let key = self.encode_value(key)?;
        let next = {
            let mut heap = self.canonical_heap.borrow_mut();
            table
                .next(&mut heap, key)
                .map_err(|error| LuaError::new(format!("internal error: {error}")))?
        };
        match next {
            Some((key, value)) => Ok(Some((self.decode_value(key)?, self.decode_value(value)?))),
            None => Ok(None),
        }
    }

    /// Snapshot of every live entry (nil-valued array slots and hash
    /// tombstones filtered out), decoded. Iteration order matches
    /// `table_next`'s: array part in index order, then hash part in
    /// insertion order. Not yet wired into a caller - no `pairs()` fast path
    /// or serialization helper needs a full-table snapshot yet.
    #[allow(dead_code)]
    pub(super) fn table_entries(&self, table: TableRef) -> LuaResult<Vec<(LuaValue, LuaValue)>> {
        let entries = {
            let mut heap = self.canonical_heap.borrow_mut();
            heap.table_entries(table.object_id())
                .map_err(|error| LuaError::new(format!("internal error: {error}")))?
        };
        entries
            .into_iter()
            .map(|(key, value)| Ok((self.decode_value(key)?, self.decode_value(value)?)))
            .collect()
    }

    /// A byte-string key lookup (e.g. `__index`, `__mode`, `__gc`) without
    /// building a `LuaValue::String` first.
    pub(super) fn table_get_str_field(&self, table: TableRef, name: &[u8]) -> Option<LuaValue> {
        let key = {
            let mut heap = self.canonical_heap.borrow_mut();
            Value::object(heap.alloc_string(name))
        };
        let value = {
            let heap = self.canonical_heap.borrow();
            table.get(&heap, key)
        };
        if value == Value::NIL {
            return None;
        }
        self.decode_value(value).ok()
    }

    pub(super) fn table_metatable(&self, table: TableRef) -> Option<TableRef> {
        let heap = self.canonical_heap.borrow();
        match heap.object(table.object_id()).ok()? {
            HeapObject::Table(object) => object.metatable.map(TableRef::new),
            _ => None,
        }
    }

    pub(super) fn table_set_metatable(
        &self,
        table: TableRef,
        metatable: Option<TableRef>,
    ) -> LuaResult<()> {
        {
            let mut heap = self.canonical_heap.borrow_mut();
            heap.set_metatable(table.object_id(), metatable.map(TableRef::object_id))
                .map_err(|error| LuaError::new(format!("internal error: {error}")))?;
        }
        // Registers the table with `sol_core::Heap`'s generic finalizer-queue
        // machinery (see `gc.rs`'s `collect_garbage`) the moment a metatable
        // defining `__gc` is attached, matching real Lua's "the `__gc`
        // metamethod is fixed at the time `setmetatable` is called" semantics
        // - a metatable set *without* `__gc`, or removed later, leaves any
        // already-`Registered` state alone (once registered, always run,
        // matching real Lua's own "`__gc` looked up once" behavior).
        //
        // Registration only needs *presence* of a non-nil `__gc` field here,
        // not callability: real Lua's `luaC_checkfinalizer` gates registration
        // on `fasttm(..., TM_GC)` finding any non-nil metatable value, then
        // looks the field up *again*, fresh, at actual finalization time
        // (`run_gc_finalizers` below does this via its own `table_finalizer`
        // call) - only that later lookup requires callability, so it can
        // silently no-op a non-function `__gc`. A placeholder like
        // `setmetatable(u, {__gc = true})` followed later by
        // `getmetatable(u).__gc = function(...) ... end` (a plain field
        // write, not another `setmetatable` call) must still mark `u`
        // to-be-finalized right away - `lua-5.5.1-tests/gc.lua`'s "__gc x
        // weak tables" section depends on exactly this ordering.
        if metatable
            .and_then(|metatable| self.table_get_str_field(metatable, b"__gc"))
            .is_some()
        {
            let mut heap = self.canonical_heap.borrow_mut();
            let _ = heap.register_finalizer(table.object_id());
        }
        Ok(())
    }

    /// Not yet wired into a caller - no `next()`/generic-for iterator yet
    /// checks this for "table modified during traversal".
    #[allow(dead_code)]
    pub(super) fn table_version(&self, table: TableRef) -> u64 {
        let heap = self.canonical_heap.borrow();
        match heap.object(table.object_id()) {
            Ok(HeapObject::Table(object)) => object.version,
            _ => 0,
        }
    }

    /// Reads `(weak_keys, weak_values)` off a metatable's `__mode` field.
    /// Real Lua matches "k"/"v" as substrings of an arbitrary `__mode`
    /// string (so `"kv"` and `"vk"` both mean both), not an exact match.
    pub(super) fn table_weak_mode(&self, metatable: TableRef) -> (bool, bool) {
        match self.table_get_str_field(metatable, b"__mode") {
            Some(LuaValue::String(mode)) => (
                mode.as_bytes().contains(&b'k'),
                mode.as_bytes().contains(&b'v'),
            ),
            _ => (false, false),
        }
    }

    /// Applies a table's own metatable's `__mode` as its live weak-key/
    /// weak-value flags on the canonical heap, or clears both if it has no
    /// metatable or no `__mode`. `sol_core::Heap`'s tracing collector prunes
    /// weakly-held entries itself during collection - unlike the legacy
    /// `LuaTable` representation, this engine no longer needs its own
    /// `prune_weak_table`/uniquely-held-reference bookkeeping.
    pub(super) fn table_sync_weak_mode(&self, table: TableRef) -> LuaResult<()> {
        let (weak_keys, weak_values) = match self.table_metatable(table) {
            Some(metatable) => self.table_weak_mode(metatable),
            None => (false, false),
        };
        let mut heap = self.canonical_heap.borrow_mut();
        heap.set_table_weak_mode(table.object_id(), weak_keys, weak_values)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))
    }

    /// The table's `__gc` metamethod, if its metatable defines one as a
    /// callable value.
    pub(super) fn table_finalizer(&self, table: TableRef) -> Option<LuaValue> {
        let metatable = self.table_metatable(table)?;
        match self.table_get_str_field(metatable, b"__gc")? {
            value @ (LuaValue::Closure(_)
            | LuaValue::NativeFunction(_)
            | LuaValue::Native(_)
            | LuaValue::RegisteredNative(_)) => Some(value),
            _ => None,
        }
    }

    /// Allocates a fresh `string.gmatch` iterator state as a
    /// `HeapObject::NativeCallable` under the shared `LEGACY_STATE_PROVIDER`
    /// namespace (see `value.rs`'s `GMatchRef` and
    /// docs/features/table-closure-coroutine-cutover.md §2). Never memoized -
    /// every `gmatch()` call gets its own independently mutable iterator,
    /// even over identical source/pattern text.
    pub(super) fn gmatch_alloc(
        &mut self,
        source: &[u8],
        pattern: &[u8],
        position: usize,
    ) -> LuaResult<GMatchRef> {
        self.charge_allocation(source.len() + pattern.len(), None)?;
        let mut heap = self.canonical_heap.borrow_mut();
        let source_id = heap.alloc_string_fresh(source);
        let pattern_id = heap.alloc_string_fresh(pattern);
        let captures = vec![
            Value::object(source_id),
            Value::object(pattern_id),
            Value::integer(position as i64),
            Value::integer(-1),
        ];
        let object = heap.alloc_native_callable(LEGACY_STATE_PROVIDER, GMATCH_ITERATOR_FUNCTION, captures);
        Ok(GMatchRef::new(object))
    }

    /// Reads an iterator's `(source bytes, pattern bytes, position,
    /// last_end)` off its `captures`, decoding the two string object ids
    /// back into owned byte buffers.
    pub(super) fn gmatch_read(&self, state: GMatchRef) -> LuaResult<(Vec<u8>, Vec<u8>, usize, Option<usize>)> {
        let heap = self.canonical_heap.borrow();
        let captures = match heap.object(state.object_id()) {
            Ok(HeapObject::NativeCallable(object)) => &object.captures,
            _ => return Err(LuaError::new("internal error: gmatch state missing")),
        };
        let string_bytes = |value: Value| -> LuaResult<Vec<u8>> {
            let id = value
                .as_object()
                .ok_or_else(|| LuaError::new("internal error: gmatch state corrupt"))?;
            match heap.object(id) {
                Ok(HeapObject::String(bytes)) => Ok(bytes.clone()),
                _ => Err(LuaError::new("internal error: gmatch state corrupt")),
            }
        };
        let source = string_bytes(captures[0])?;
        let pattern = string_bytes(captures[1])?;
        let position = captures[2]
            .as_integer()
            .ok_or_else(|| LuaError::new("internal error: gmatch state corrupt"))? as usize;
        let last_end = match captures[3].as_integer() {
            Some(value) if value >= 0 => Some(value as usize),
            _ => None,
        };
        Ok((source, pattern, position, last_end))
    }

    /// Rewrites an iterator's mutable `position`/`last_end` in place,
    /// preserving its already-interned source/pattern string ids.
    pub(super) fn gmatch_advance(
        &self,
        state: GMatchRef,
        position: usize,
        last_end: Option<usize>,
    ) -> LuaResult<()> {
        let mut heap = self.canonical_heap.borrow_mut();
        let (source, pattern) = match heap.object(state.object_id()) {
            Ok(HeapObject::NativeCallable(object)) => (object.captures[0], object.captures[1]),
            _ => return Err(LuaError::new("internal error: gmatch state missing")),
        };
        let captures = vec![
            source,
            pattern,
            Value::integer(position as i64),
            Value::integer(last_end.map_or(-1, |value| value as i64)),
        ];
        heap.set_native_callable_captures(state.object_id(), captures)
            .map_err(|error| LuaError::new(format!("internal error: {error}")))
    }
}
