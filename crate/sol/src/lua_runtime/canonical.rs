//! Temporary adapters from the existing `Rc`-backed Lua runtime into U2's
//! canonical `sol-core` heap. They make migration incremental while preserving
//! identity for the object classes already bridged.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use sol_core::{Heap, NativeCallableId, ObjectId, ThreadStatus, Value};

use super::frame::{Frame, NativeCont, XCallStage};
use super::{
    table_weak_mode, BridgeScalar, CoroutineStatus, GMatchState, LuaClosure, LuaCoroutine,
    LuaError, LuaTable, LuaValue, NativeBridge, NativeFunction,
};

const LEGACY_STATE_PROVIDER: u32 = u32::MAX;
const GMATCH_ITERATOR_FUNCTION: u32 = 0;
const COROUTINE_WRAPPER_FUNCTION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAdapterError {
    Unsupported(&'static str),
    Heap(String),
}

impl fmt::Display for CanonicalAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(kind) => {
                write!(
                    f,
                    "the transitional canonical adapter does not support {kind} yet"
                )
            }
            Self::Heap(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for CanonicalAdapterError {}

/// Imports old-runtime graphs once. Reusing one adapter for a migration unit
/// guarantees repeated references and cycles map to the same canonical ID.
#[derive(Default)]
pub struct CanonicalAdapter {
    tables: HashMap<usize, ObjectId>,
    closures: HashMap<usize, ObjectId>,
    upvalues: HashMap<usize, ObjectId>,
    initialized_upvalues: HashSet<usize>,
    environments: HashMap<usize, ObjectId>,
    initialized_environments: HashSet<usize>,
    prototypes: HashMap<usize, u32>,
    native_functions: HashMap<NativeFunction, ObjectId>,
    native_bridges: HashMap<usize, ObjectId>,
    bridge_providers: HashMap<u32, Rc<NativeBridge>>,
    registered_native_providers: HashMap<u32, u32>,
    registered_natives: HashMap<NativeCallableId, ObjectId>,
    iterators: HashMap<usize, ObjectId>,
    threads: HashMap<usize, ObjectId>,
    coroutine_wrappers: HashMap<usize, ObjectId>,
}

impl CanonicalAdapter {
    pub fn import(
        &mut self,
        heap: &mut Heap,
        value: &LuaValue,
    ) -> Result<Value, CanonicalAdapterError> {
        match value {
            LuaValue::Nil => Ok(Value::NIL),
            LuaValue::Bool(value) => Ok(Value::boolean(*value)),
            LuaValue::Integer(value) => Ok(Value::integer(*value)),
            LuaValue::Float(value) => Ok(Value::float(*value)),
            LuaValue::String(value) => Ok(Value::object(heap.alloc_string(value.as_bytes()))),
            LuaValue::Table(table) => self.import_table(heap, table),
            LuaValue::CanonicalTable(table) => {
                let object = table.object_id();
                if heap.contains(object) {
                    Ok(Value::object(object))
                } else {
                    Err(CanonicalAdapterError::Unsupported(
                        "canonical table belongs to another heap",
                    ))
                }
            }
            LuaValue::Closure(closure) => self.import_closure(heap, closure),
            LuaValue::NativeFunction(function) => self.import_native_function(heap, *function),
            LuaValue::Native(bridge) => self.import_native_bridge(heap, bridge),
            LuaValue::RegisteredNative(callable) => self.import_registered_native(heap, *callable),
            LuaValue::CFunction(callable) => {
                let object = callable.object_id();
                if heap.contains(object) {
                    Ok(Value::object(object))
                } else {
                    Err(CanonicalAdapterError::Unsupported(
                        "canonical C function belongs to another heap",
                    ))
                }
            }
            LuaValue::GMatchIterator(state) => self.import_gmatch_iterator(heap, state),
            LuaValue::Thread(thread) => self.import_thread(heap, thread),
            LuaValue::CoroutineWrapper(thread) => self.import_coroutine_wrapper(heap, thread),
            LuaValue::Userdata(userdata) => {
                let object = userdata.object_id();
                if heap.contains(object) {
                    Ok(Value::object(object))
                } else {
                    Err(CanonicalAdapterError::Unsupported(
                        "canonical userdata belongs to another heap",
                    ))
                }
            }
            LuaValue::LightUserdata(_) => Err(CanonicalAdapterError::Unsupported(
                "debug.upvalueid identities",
            )),
        }
    }

    fn import_registered_native(
        &mut self,
        heap: &mut Heap,
        callable: NativeCallableId,
    ) -> Result<Value, CanonicalAdapterError> {
        if let Some(object) = self.registered_natives.get(&callable) {
            return Ok(Value::object(*object));
        }
        let provider = match self.registered_native_providers.get(&callable.provider) {
            Some(provider) => *provider,
            None => {
                let provider = heap
                    .reserve_native_provider()
                    .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
                self.registered_native_providers
                    .insert(callable.provider, provider);
                provider
            }
        };
        let object = heap.alloc_native_callable(provider, callable.function, Vec::new());
        self.registered_natives.insert(callable, object);
        Ok(Value::object(object))
    }

    /// Imports the value, traceback, and cause-ready wrapper of a legacy Lua
    /// error. Synthesized errors use their message string as the Lua value;
    /// explicitly raised values retain the identity established by `import`.
    pub fn import_error(
        &mut self,
        heap: &mut Heap,
        error: &LuaError,
    ) -> Result<Value, CanonicalAdapterError> {
        let value = match &error.value {
            Some(value) => self.import(heap, value)?,
            None => Value::object(heap.alloc_string(error.message.as_bytes())),
        };
        let traceback = error
            .stack
            .iter()
            .map(|frame| frame.as_bytes().to_vec())
            .collect();
        let error = heap
            .alloc_error(value, None, traceback)
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        Ok(Value::object(error))
    }

    fn import_table(
        &mut self,
        heap: &mut Heap,
        table: &Rc<std::cell::RefCell<LuaTable>>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(table) as usize;
        if let Some(id) = self.tables.get(&identity) {
            return Ok(Value::object(*id));
        }

        let id = heap.alloc_table();
        self.tables.insert(identity, id);
        let (entries, metatable, weak_mode) = {
            let table = table.borrow();
            let metatable = table.metatable.clone();
            let weak_mode = metatable
                .as_ref()
                .map(table_weak_mode)
                .unwrap_or((false, false));
            (table.entries(false), metatable, weak_mode)
        };
        for (key, value) in entries {
            let key = self.import(heap, &key)?;
            let value = self.import(heap, &value)?;
            heap.table_set(id, key, value)
                .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        }
        if let Some(metatable) = metatable {
            let metatable = self.import_table(heap, &metatable)?.as_object().unwrap();
            heap.set_metatable(id, Some(metatable))
                .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        }
        heap.set_table_weak_mode(id, weak_mode.0, weak_mode.1)
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        Ok(Value::object(id))
    }

    fn import_closure(
        &mut self,
        heap: &mut Heap,
        closure: &Rc<LuaClosure>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(closure) as usize;
        if let Some(id) = self.closures.get(&identity) {
            return Ok(Value::object(*id));
        }

        // Allocate every cell before importing its contents. A cell may
        // point back to this closure, or to another closure that shares the
        // same cell, so placeholder allocation is what makes those cycles
        // representable without duplicating identity.
        let legacy_upvalues = closure.upvals.borrow().clone();
        let mut canonical_upvalues = Vec::with_capacity(legacy_upvalues.len() + 1);
        for cell in &legacy_upvalues {
            let cell_identity = Rc::as_ptr(cell) as usize;
            let id = match self.upvalues.get(&cell_identity) {
                Some(id) => *id,
                None => {
                    let id = heap.alloc_upvalue(Value::NIL, None);
                    self.upvalues.insert(cell_identity, id);
                    id
                }
            };
            canonical_upvalues.push(id);
        }

        // The legacy closure stores globals beside its ordinary upvalue
        // vector. Canonical closures make `_ENV` an explicit shared upvalue.
        let environment = closure.globals.as_value();
        let LuaValue::Table(environment_table) = &environment else {
            unreachable!("Globals::as_value always returns a table")
        };
        let environment_identity = Rc::as_ptr(environment_table) as usize;
        let environment_upvalue = match self.environments.get(&environment_identity) {
            Some(id) => *id,
            None => {
                let id = heap.alloc_upvalue(Value::NIL, None);
                self.environments.insert(environment_identity, id);
                id
            }
        };
        let environment_index = canonical_upvalues.len();
        let captured_upvalues = canonical_upvalues.clone();
        canonical_upvalues.push(environment_upvalue);

        let prototype_identity = Rc::as_ptr(&closure.proto) as usize;
        let prototype = match self.prototypes.get(&prototype_identity) {
            Some(prototype) => *prototype,
            None => {
                let prototype = u32::try_from(self.prototypes.len()).map_err(|_| {
                    CanonicalAdapterError::Unsupported("more than u32::MAX Lua prototypes")
                })?;
                self.prototypes.insert(prototype_identity, prototype);
                prototype
            }
        };
        let id = heap
            .alloc_closure(prototype, canonical_upvalues, environment_index)
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        self.closures.insert(identity, id);

        // Mark a placeholder initialized before descending through its value:
        // recursive imports then reuse the same cell and closure handles.
        if self.initialized_environments.insert(environment_identity) {
            let value = self.import(heap, &environment)?;
            heap.set_upvalue(environment_upvalue, value)
                .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        }
        for (cell, canonical) in legacy_upvalues.iter().zip(captured_upvalues) {
            let imported = self.import_upvalue(heap, cell)?;
            debug_assert_eq!(imported, canonical);
        }
        Ok(Value::object(id))
    }

    fn import_upvalue(
        &mut self,
        heap: &mut Heap,
        cell: &Rc<std::cell::RefCell<LuaValue>>,
    ) -> Result<ObjectId, CanonicalAdapterError> {
        let identity = Rc::as_ptr(cell) as usize;
        let id = match self.upvalues.get(&identity) {
            Some(id) => *id,
            None => {
                let id = heap.alloc_upvalue(Value::NIL, None);
                self.upvalues.insert(identity, id);
                id
            }
        };
        if self.initialized_upvalues.insert(identity) {
            let legacy = cell.borrow().clone();
            let value = self.import(heap, &legacy)?;
            heap.set_upvalue(id, value)
                .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        }
        Ok(id)
    }

    fn import_native_function(
        &mut self,
        heap: &mut Heap,
        function: NativeFunction,
    ) -> Result<Value, CanonicalAdapterError> {
        if let Some(id) = self.native_functions.get(&function) {
            return Ok(Value::object(*id));
        }
        // Provider zero is the portable Lua standard library. `repr(u32)` on
        // `NativeFunction` makes this registry key explicit rather than
        // smuggling a Rust function pointer into the portable heap.
        let id = heap.alloc_native_callable(0, function as u32, Vec::new());
        self.native_functions.insert(function, id);
        Ok(Value::object(id))
    }

    fn import_native_bridge(
        &mut self,
        heap: &mut Heap,
        bridge: &Rc<NativeBridge>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(bridge) as usize;
        if let Some(id) = self.native_bridges.get(&identity) {
            return Ok(Value::object(*id));
        }
        let provider = heap
            .reserve_native_provider()
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        let id = heap.alloc_native_callable(provider, 0, Vec::new());
        self.native_bridges.insert(identity, id);
        self.bridge_providers.insert(provider, bridge.clone());
        Ok(Value::object(id))
    }

    /// Resolves a provider/function pair imported from a legacy native Sol
    /// bridge. Standard-library and legacy-state providers intentionally do
    /// not resolve here; they belong to their respective portable dispatchers.
    pub fn registered_native_bridge(
        &self,
        provider: u32,
        function: u32,
    ) -> Option<Rc<NativeBridge>> {
        (function == 0)
            .then(|| self.bridge_providers.get(&provider).cloned())
            .flatten()
    }

    fn import_gmatch_iterator(
        &mut self,
        heap: &mut Heap,
        state: &Rc<std::cell::RefCell<GMatchState>>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(state) as usize;
        if let Some(id) = self.iterators.get(&identity) {
            return Ok(Value::object(*id));
        }
        let state = state.borrow();
        let position = i64::try_from(state.position).map_err(|_| {
            CanonicalAdapterError::Unsupported("iterator position larger than i64::MAX")
        })?;
        let last_end = match state.last_end {
            Some(last_end) => Value::integer(i64::try_from(last_end).map_err(|_| {
                CanonicalAdapterError::Unsupported("iterator position larger than i64::MAX")
            })?),
            None => Value::NIL,
        };
        let captures = vec![
            Value::object(heap.alloc_string(state.source.as_slice())),
            Value::object(heap.alloc_string(state.pattern.as_slice())),
            Value::integer(position),
            last_end,
        ];
        let id =
            heap.alloc_native_callable(LEGACY_STATE_PROVIDER, GMATCH_ITERATOR_FUNCTION, captures);
        self.iterators.insert(identity, id);
        Ok(Value::object(id))
    }

    fn import_thread(
        &mut self,
        heap: &mut Heap,
        thread: &Rc<LuaCoroutine>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(thread) as usize;
        if let Some(id) = self.threads.get(&identity) {
            return Ok(Value::object(*id));
        }

        // Allocate and memoize before walking frames: a live register may
        // reach a table/closure/wrapper that points back to this coroutine.
        let id = heap.alloc_thread(Vec::new());
        self.threads.insert(identity, id);
        let mut roots = Vec::new();
        if let Some(body) = thread.body.borrow().clone() {
            self.import_root(heap, &mut roots, &body)?;
        }
        for frame in thread.frames.borrow().iter() {
            self.import_frame_roots(heap, &mut roots, frame)?;
        }
        let status = match thread.status.get() {
            CoroutineStatus::Suspended => ThreadStatus::Suspended,
            CoroutineStatus::Running => ThreadStatus::Running,
            CoroutineStatus::Normal => ThreadStatus::Normal,
            CoroutineStatus::Dead => ThreadStatus::Dead,
        };
        heap.set_thread_state(id, status, roots, Vec::new())
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        Ok(Value::object(id))
    }

    fn import_coroutine_wrapper(
        &mut self,
        heap: &mut Heap,
        thread: &Rc<LuaCoroutine>,
    ) -> Result<Value, CanonicalAdapterError> {
        let identity = Rc::as_ptr(thread) as usize;
        if let Some(id) = self.coroutine_wrappers.get(&identity) {
            return Ok(Value::object(*id));
        }

        // Placeholder-first allocation handles a wrapper captured by the
        // body of its own coroutine.
        let id = heap.alloc_native_callable(
            LEGACY_STATE_PROVIDER,
            COROUTINE_WRAPPER_FUNCTION,
            Vec::new(),
        );
        self.coroutine_wrappers.insert(identity, id);
        let thread = self.import_thread(heap, thread)?;
        heap.set_native_callable_captures(id, vec![thread])
            .map_err(|error| CanonicalAdapterError::Heap(error.to_string()))?;
        Ok(Value::object(id))
    }

    fn import_frame_roots(
        &mut self,
        heap: &mut Heap,
        roots: &mut Vec<Value>,
        frame: &Frame,
    ) -> Result<(), CanonicalAdapterError> {
        match frame {
            Frame::Lua(frame) => {
                self.import_root(heap, roots, &frame.globals.as_value())?;
                for value in frame.regs.iter().chain(&frame.varargs) {
                    self.import_root(heap, roots, value)?;
                }
                for cell in &frame.upvals {
                    roots.push(Value::object(self.import_upvalue(heap, cell)?));
                }
                for cell in frame.cells.iter().flatten() {
                    roots.push(Value::object(self.import_upvalue(heap, cell)?));
                }
            }
            Frame::Native(NativeCont::Xpcall(XCallStage::Function { handler })) => {
                self.import_root(heap, roots, handler)?;
            }
            Frame::Native(NativeCont::Sort(state)) => {
                self.import_root(heap, roots, &state.table)?;
                for value in state
                    .sorter
                    .values()
                    .iter()
                    .chain(state.sorter.merge_roots())
                {
                    self.import_root(heap, roots, value)?;
                }
                if let Some(comparator) = &state.comparator {
                    self.import_root(heap, roots, comparator)?;
                }
            }
            Frame::Native(NativeCont::Gsub(state)) => {
                self.import_root(heap, roots, &state.repl)?;
            }
            Frame::Native(
                NativeCont::Pcall | NativeCont::Xpcall(XCallStage::Handler) | NativeCont::Once,
            ) => {}
        }
        Ok(())
    }

    fn import_root(
        &mut self,
        heap: &mut Heap,
        roots: &mut Vec<Value>,
        value: &LuaValue,
    ) -> Result<(), CanonicalAdapterError> {
        roots.push(self.import(heap, value)?);
        Ok(())
    }

    pub fn import_typed_scalar(kind: BridgeScalar, bits: u64) -> Value {
        sol_core::BoundaryValue::unboxed(kind, bits).boxed()
    }

    pub fn export_typed_scalar(
        kind: BridgeScalar,
        value: Value,
    ) -> Result<u64, CanonicalAdapterError> {
        sol_core::BoundaryValue::checked_unbox(value, kind)
            .ok()
            .and_then(sol_core::BoundaryValue::bits)
            .ok_or(CanonicalAdapterError::Unsupported("typed scalar mismatch"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use sol_core::Capabilities;

    use super::super::CanonicalString;
    use super::*;

    #[test]
    fn table_cycles_and_repeated_references_keep_one_canonical_identity() {
        let string_heap = Rc::new(RefCell::new(Heap::new(Capabilities::SANDBOX)));
        let table = Rc::new(RefCell::new(LuaTable::default()));
        table
            .borrow_mut()
            .set(
                LuaValue::String(CanonicalString::intern(string_heap, "self")),
                LuaValue::Table(table.clone()),
            )
            .unwrap();

        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let first = adapter
            .import(&mut heap, &LuaValue::Table(table.clone()))
            .unwrap();
        let second = adapter.import(&mut heap, &LuaValue::Table(table)).unwrap();
        assert_eq!(first, second);
        let key = Value::object(heap.alloc_string(b"self"));
        assert_eq!(
            heap.table_get(first.as_object().unwrap(), key).unwrap(),
            first
        );

        let root = heap.add_root(first);
        heap.collect_major();
        assert!(heap.contains(first.as_object().unwrap()));
        heap.remove_root(root);
        assert!(heap.collect_major().reclaimed >= 1);
    }

    #[test]
    fn typed_scalar_adapter_round_trips_without_boxing_inside_typed_code() {
        for (kind, bits) in [
            (BridgeScalar::I64, 42_u64),
            (BridgeScalar::F64, 3.5_f64.to_bits()),
            (BridgeScalar::Bool, 1_u64),
        ] {
            let value = CanonicalAdapter::import_typed_scalar(kind, bits);
            assert_eq!(CanonicalAdapter::export_typed_scalar(kind, value), Ok(bits));
        }
    }

    #[test]
    fn production_closure_environment_and_self_capture_form_one_traced_graph() {
        let run = super::super::run_source(
            br#"
                local recursive
                recursive = function()
                    return recursive
                end
                return recursive
            "#,
        )
        .unwrap();
        assert!(matches!(run.value, LuaValue::Closure(_)));

        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let closure_value = adapter.import(&mut heap, &run.value).unwrap();
        let closure_id = closure_value.as_object().unwrap();
        let sol_core::HeapObject::Closure(closure) = heap.object(closure_id).unwrap() else {
            panic!("imported Lua closure is not a canonical closure")
        };
        let environment_upvalue = closure.upvalues[closure.environment];
        let captured_upvalues = closure
            .upvalues
            .iter()
            .copied()
            .filter(|upvalue| *upvalue != environment_upvalue)
            .collect::<Vec<_>>();
        assert_eq!(captured_upvalues.len(), 1);
        let sol_core::HeapObject::Upvalue(capture) = heap.object(captured_upvalues[0]).unwrap()
        else {
            panic!("closure capture is not a canonical upvalue")
        };
        assert_eq!(capture.value, closure_value);

        let sol_core::HeapObject::Upvalue(environment) = heap.object(environment_upvalue).unwrap()
        else {
            panic!("closure environment is not a canonical upvalue")
        };
        let environment_table = environment.value.as_object().unwrap();
        let print_key = Value::object(heap.alloc_string(b"print"));
        let print = heap.table_get(environment_table, print_key).unwrap();
        assert!(matches!(
            heap.object(print.as_object().unwrap()).unwrap(),
            sol_core::HeapObject::NativeCallable(_)
        ));

        let root = heap.add_root(closure_value);
        heap.collect_major();
        assert!(heap.contains(closure_id));
        assert!(heap.contains(captured_upvalues[0]));
        assert!(heap.contains(environment_table));
        heap.remove_root(root);
        let collection = heap.collect_major();
        assert!(collection.reclaimed > 0);
        assert!(heap.is_empty());
    }

    #[test]
    fn repeated_native_function_import_preserves_callable_identity() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let native = LuaValue::NativeFunction(NativeFunction::Print);
        let first = adapter.import(&mut heap, &native).unwrap();
        let second = adapter.import(&mut heap, &native).unwrap();
        assert_eq!(first, second);
        let sol_core::HeapObject::NativeCallable(callable) =
            heap.object(first.as_object().unwrap()).unwrap()
        else {
            panic!("imported native function is not a canonical callable")
        };
        assert_eq!(callable.provider, 0);
        assert_eq!(callable.function, NativeFunction::Print as u32);
    }

    #[test]
    fn sibling_closures_keep_one_shared_canonical_upvalue() {
        let run = super::super::run_source(
            br#"
                local shared = 41
                local first = function() return shared end
                local second = function() return shared + 1 end
                return { first = first, second = second }
            "#,
        )
        .unwrap();
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let table = adapter.import(&mut heap, &run.value).unwrap();
        let table = table.as_object().unwrap();
        let first_key = Value::object(heap.alloc_string(b"first"));
        let second_key = Value::object(heap.alloc_string(b"second"));
        let first = heap
            .table_get(table, first_key)
            .unwrap()
            .as_object()
            .unwrap();
        let second = heap
            .table_get(table, second_key)
            .unwrap()
            .as_object()
            .unwrap();

        let captured_cell = |heap: &Heap, closure| {
            let sol_core::HeapObject::Closure(closure) = heap.object(closure).unwrap() else {
                panic!("table member is not a canonical closure")
            };
            closure
                .upvalues
                .iter()
                .enumerate()
                .find_map(|(index, upvalue)| (index != closure.environment).then_some(*upvalue))
                .unwrap()
        };
        assert_eq!(captured_cell(&heap, first), captured_cell(&heap, second));
    }

    #[test]
    fn raised_error_payload_keeps_table_identity_and_reachability() {
        let error = super::super::run_source(
            br#"
                local payload = { code = 42 }
                error(payload)
            "#,
        )
        .unwrap_err();
        let payload = error.value.as_ref().unwrap();
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let payload = adapter.import(&mut heap, payload).unwrap();
        let canonical_error = adapter.import_error(&mut heap, &error).unwrap();
        let error_id = canonical_error.as_object().unwrap();
        let sol_core::HeapObject::Error(error) = heap.object(error_id).unwrap() else {
            panic!("imported Lua error is not a canonical error")
        };
        assert_eq!(error.value, payload);

        let root = heap.add_root(canonical_error);
        heap.collect_major();
        assert!(heap.contains(error_id));
        assert!(heap.contains(payload.as_object().unwrap()));
        heap.remove_root(root);
        let collection = heap.collect_major();
        assert!(collection.reclaimed >= 2);
    }

    #[test]
    fn stateful_iterator_snapshot_traces_subject_and_pattern() {
        let run = super::super::run_source(b"return string.gmatch('ababa', 'a')").unwrap();
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let first = adapter.import(&mut heap, &run.value).unwrap();
        let second = adapter.import(&mut heap, &run.value).unwrap();
        assert_eq!(first, second);
        let sol_core::HeapObject::NativeCallable(iterator) =
            heap.object(first.as_object().unwrap()).unwrap()
        else {
            panic!("gmatch iterator is not a canonical native callable")
        };
        assert_eq!(iterator.provider, LEGACY_STATE_PROVIDER);
        assert_eq!(iterator.function, GMATCH_ITERATOR_FUNCTION);
        assert_eq!(iterator.captures.len(), 4);
        assert!(iterator.captures[0].as_object().is_some());
        assert!(iterator.captures[1].as_object().is_some());
        let captured_strings = iterator.captures[..2].to_vec();

        let root = heap.add_root(first);
        heap.collect_major();
        for capture in &captured_strings {
            assert!(heap.contains(capture.as_object().unwrap()));
        }
        heap.remove_root(root);
        heap.collect_major();
        assert!(heap.is_empty());
    }

    #[test]
    fn suspended_coroutine_snapshot_roots_live_frame_values() {
        let run = super::super::run_source(
            br#"
                local thread = coroutine.create(function()
                    local held = { marker = 42 }
                    coroutine.yield()
                    return held
                end)
                assert(coroutine.resume(thread))
                return thread
            "#,
        )
        .unwrap();
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let thread = adapter.import(&mut heap, &run.value).unwrap();
        let thread_id = thread.as_object().unwrap();
        let sol_core::HeapObject::Thread(snapshot) = heap.object(thread_id).unwrap() else {
            panic!("coroutine is not a canonical thread")
        };
        assert_eq!(snapshot.status, ThreadStatus::Suspended);
        let stack = snapshot.stack.clone();
        let marker = Value::object(heap.alloc_string(b"marker"));
        let held = stack.iter().find_map(|value| {
            let id = value.as_object()?;
            if !matches!(heap.object(id).ok()?, sol_core::HeapObject::Table(_)) {
                return None;
            }
            (heap.table_get(id, marker).ok()? == Value::integer(42)).then_some(id)
        });
        let held = held.expect("suspended frame lost its live local table");

        let root = heap.add_root(thread);
        heap.collect_major();
        assert!(heap.contains(thread_id));
        assert!(heap.contains(held));
        heap.remove_root(root);
        heap.collect_major();
        assert!(heap.is_empty());
    }

    #[test]
    fn coroutine_wrapper_cycle_uses_placeholder_identity() {
        let run = super::super::run_source(
            br#"
                local wrapped
                wrapped = coroutine.wrap(function()
                    coroutine.yield(wrapped)
                end)
                return wrapped
            "#,
        )
        .unwrap();
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let wrapper = adapter.import(&mut heap, &run.value).unwrap();
        let wrapper_id = wrapper.as_object().unwrap();
        let sol_core::HeapObject::NativeCallable(callable) = heap.object(wrapper_id).unwrap()
        else {
            panic!("coroutine wrapper is not a canonical callable")
        };
        assert_eq!(callable.provider, LEGACY_STATE_PROVIDER);
        assert_eq!(callable.function, COROUTINE_WRAPPER_FUNCTION);
        assert_eq!(callable.captures.len(), 1);
        let thread = callable.captures[0].as_object().unwrap();
        assert!(matches!(
            heap.object(thread).unwrap(),
            sol_core::HeapObject::Thread(_)
        ));

        let root = heap.add_root(wrapper);
        heap.collect_major();
        assert!(heap.contains(wrapper_id));
        assert!(heap.contains(thread));
        heap.remove_root(root);
        heap.collect_major();
        assert!(heap.is_empty());
    }

    #[test]
    fn native_bridges_use_unique_heap_provider_ids_and_resolve_without_heap_pointers() {
        extern "C" fn stub(_args: *const u64, _len: i64) -> u64 {
            42
        }

        let bridge = Rc::new(NativeBridge {
            name: "stub".to_string(),
            ptr: stub as *const () as *const u8,
            params: Vec::new(),
            ret: BridgeScalar::I64,
        });
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut first_adapter = CanonicalAdapter::default();
        let first = first_adapter
            .import(&mut heap, &LuaValue::Native(bridge.clone()))
            .unwrap();
        let repeated = first_adapter
            .import(&mut heap, &LuaValue::Native(bridge.clone()))
            .unwrap();
        assert_eq!(first, repeated);
        let sol_core::HeapObject::NativeCallable(first_callable) =
            heap.object(first.as_object().unwrap()).unwrap()
        else {
            panic!("native bridge is not a canonical callable")
        };
        let first_provider = first_callable.provider;
        assert_ne!(first_provider, 0);
        assert_eq!(first_callable.function, 0);
        assert!(Rc::ptr_eq(
            &first_adapter
                .registered_native_bridge(first_provider, 0)
                .unwrap(),
            &bridge
        ));
        assert!(first_adapter
            .registered_native_bridge(first_provider, 1)
            .is_none());

        // A separate adapter importing into the same heap must receive a new
        // provider namespace even when its local registry starts empty.
        let second_bridge = Rc::new(NativeBridge {
            name: "second".to_string(),
            ptr: stub as *const () as *const u8,
            params: Vec::new(),
            ret: BridgeScalar::I64,
        });
        let mut second_adapter = CanonicalAdapter::default();
        let second = second_adapter
            .import(&mut heap, &LuaValue::Native(second_bridge))
            .unwrap();
        let sol_core::HeapObject::NativeCallable(second_callable) =
            heap.object(second.as_object().unwrap()).unwrap()
        else {
            panic!("second native bridge is not a canonical callable")
        };
        assert_ne!(first_provider, second_callable.provider);
    }

    #[test]
    fn registered_native_identity_survives_canonical_collection() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let mut adapter = CanonicalAdapter::default();
        let callable = LuaValue::RegisteredNative(NativeCallableId::new(7, 11));
        let first = adapter.import(&mut heap, &callable).unwrap();
        let repeated = adapter.import(&mut heap, &callable).unwrap();
        assert_eq!(first, repeated);
        let id = first.as_object().unwrap();
        let root = heap.add_root(first);
        heap.collect_major();
        let sol_core::HeapObject::NativeCallable(callable) = heap.object(id).unwrap() else {
            panic!("registered native is not a canonical callable")
        };
        assert_eq!(callable.function, 11);
        heap.remove_root(root);
        heap.collect_major();
        assert!(!heap.contains(id));
    }
}
