use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use indexmap::IndexMap;

use crate::{Capabilities, ObjectId, Value, ValueTag};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    String,
    Table,
    Closure,
    NativeCallable,
    Upvalue,
    Thread,
    Userdata,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcGeneration {
    Young,
    Old,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizerState {
    None,
    Registered,
    Queued,
    Finalized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHeader {
    pub kind: ObjectKind,
    pub generation: GcGeneration,
    pub finalizer: FinalizerState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TableKey {
    Boolean(bool),
    Integer(i64),
    Float(u64),
    String(Vec<u8>),
    Object(ObjectId),
}

#[derive(Debug, Clone, Default)]
pub struct TableObject {
    pub array: Vec<Value>,
    /// Insertion-order-preserving, not a plain `HashMap`: assigning `nil` to
    /// an existing key stores a tombstone (`table_set`) rather than removing
    /// the entry, so a `HashMap`'s insert-time reordering on a later
    /// overwrite can never strand or reorder still-live entries out from
    /// under an in-progress `table_entries`/`next`-style traversal - see
    /// `sol::lua_runtime::value::LuaTable::hash`'s identical rationale.
    pub hash: IndexMap<TableKey, Value>,
    pub metatable: Option<ObjectId>,
    pub weak_keys: bool,
    pub weak_values: bool,
    pub version: u64,
}

#[derive(Debug, Clone)]
pub struct ClosureObject {
    pub prototype: u32,
    pub upvalues: Vec<ObjectId>,
    /// Index in `upvalues` containing the lexical `_ENV` cell.
    pub environment: usize,
}

/// A runtime/provider callback and the managed values it closes over.
///
/// `(provider, function)` is a portable registry key, never a process pointer.
/// Native and WASM hosts resolve it through their own provider registry while
/// the portable heap owns identity and captured-value tracing.
#[derive(Debug, Clone)]
pub struct NativeCallableObject {
    pub provider: u32,
    pub function: u32,
    pub captures: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct UpvalueObject {
    pub value: Value,
    pub open_stack_slot: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStatus {
    Suspended,
    Running,
    Normal,
    Dead,
}

#[derive(Debug, Clone)]
pub struct ThreadObject {
    pub status: ThreadStatus,
    pub stack: Vec<Value>,
    pub yielded: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct UserdataObject {
    pub host_handle: u64,
    /// Inline storage for full userdata created through the embedding API.
    /// The collector owns these bytes together with the userdata identity;
    /// `host_handle` remains available for hosts that keep payloads in an
    /// external, capability-controlled registry.
    pub bytes: Vec<u8>,
    /// Lua 5.5 user values. These are ordinary traced references owned by
    /// the userdata object and therefore participate in write barriers.
    pub user_values: Vec<Value>,
    pub metatable: Option<ObjectId>,
}

#[derive(Debug, Clone)]
pub struct ErrorObject {
    pub value: Value,
    pub cause: Option<ObjectId>,
    pub traceback: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum HeapObject {
    String(Vec<u8>),
    Table(TableObject),
    Closure(ClosureObject),
    NativeCallable(NativeCallableObject),
    Upvalue(UpvalueObject),
    Thread(ThreadObject),
    Userdata(UserdataObject),
    Error(ErrorObject),
}

impl HeapObject {
    fn kind(&self) -> ObjectKind {
        match self {
            Self::String(_) => ObjectKind::String,
            Self::Table(_) => ObjectKind::Table,
            Self::Closure(_) => ObjectKind::Closure,
            Self::NativeCallable(_) => ObjectKind::NativeCallable,
            Self::Upvalue(_) => ObjectKind::Upvalue,
            Self::Thread(_) => ObjectKind::Thread,
            Self::Userdata(_) => ObjectKind::Userdata,
            Self::Error(_) => ObjectKind::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeapError {
    StaleHandle(ObjectId),
    WrongKind {
        handle: ObjectId,
        expected: ObjectKind,
        actual: ObjectKind,
    },
    NilTableKey,
    NanTableKey,
    InvalidEnvironmentIndex {
        index: usize,
        upvalues: usize,
    },
    NativeProviderIdsExhausted,
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleHandle(handle) => write!(f, "stale managed-object handle {handle:?}"),
            Self::WrongKind {
                handle,
                expected,
                actual,
            } => write!(
                f,
                "managed-object handle {handle:?} is {actual:?}, expected {expected:?}"
            ),
            Self::NilTableKey => f.write_str("table index is nil"),
            Self::NanTableKey => f.write_str("table index is NaN"),
            Self::InvalidEnvironmentIndex { index, upvalues } => write!(
                f,
                "closure environment index {index} is outside {upvalues} upvalues"
            ),
            Self::NativeProviderIdsExhausted => {
                f.write_str("canonical native-provider ID space is exhausted")
            }
        }
    }
}

impl std::error::Error for HeapError {}

#[derive(Debug)]
struct Entry {
    header: ObjectHeader,
    object: HeapObject,
}

#[derive(Debug)]
struct Slot {
    generation: u32,
    entry: Option<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RootId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WeakHandle(ObjectId);

impl WeakHandle {
    pub const fn object(self) -> ObjectId {
        self.0
    }
}

/// Precise description of canonical `Value` slots in an interpreter or native
/// frame. JIT stack-map emission can construct this without depending on the
/// collector implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackMap {
    slots: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackMapError {
    pub slot: usize,
    pub frame_len: usize,
}

impl fmt::Display for StackMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stack-map slot {} is outside a {}-slot frame",
            self.slot, self.frame_len
        )
    }
}

impl std::error::Error for StackMapError {}

impl StackMap {
    pub fn new(mut slots: Vec<usize>) -> Self {
        slots.sort_unstable();
        slots.dedup();
        Self { slots }
    }

    pub fn roots(&self, frame: &[Value]) -> Result<Vec<Value>, StackMapError> {
        self.slots
            .iter()
            .map(|slot| {
                frame.get(*slot).copied().ok_or(StackMapError {
                    slot: *slot,
                    frame_len: frame.len(),
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    Minor,
    Major,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub kind: CollectionKind,
    pub reclaimed: usize,
    pub promoted: usize,
    pub finalizers: Vec<ObjectId>,
}

/// Canonical managed heap. All externally retained objects are addressed by
/// stable, generation-checked handles; roots are explicit and portable.
pub struct Heap {
    slots: Vec<Slot>,
    free: Vec<usize>,
    roots: HashMap<RootId, Value>,
    next_root: u64,
    interned_strings: HashMap<Vec<u8>, ObjectId>,
    remembered: HashSet<ObjectId>,
    next_native_provider: u32,
    pub capabilities: Capabilities,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new(Capabilities::SANDBOX)
    }
}

impl Heap {
    pub fn new(capabilities: Capabilities) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            roots: HashMap::new(),
            next_root: 1,
            interned_strings: HashMap::new(),
            remembered: HashSet::new(),
            // Provider zero is reserved for sol-core's portable standard
            // library callback namespace.
            next_native_provider: 1,
            capabilities,
        }
    }

    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.entry.is_some())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, id: ObjectId) -> bool {
        self.slots
            .get(id.slot())
            .is_some_and(|slot| slot.generation == id.generation() && slot.entry.is_some())
    }

    pub fn downgrade(&self, id: ObjectId) -> Result<WeakHandle, HeapError> {
        self.entry(id)?;
        Ok(WeakHandle(id))
    }

    pub fn upgrade(&self, handle: WeakHandle) -> Option<ObjectId> {
        self.contains(handle.0).then_some(handle.0)
    }

    pub fn object(&self, id: ObjectId) -> Result<&HeapObject, HeapError> {
        Ok(&self.entry(id)?.object)
    }

    pub fn header(&self, id: ObjectId) -> Result<&ObjectHeader, HeapError> {
        Ok(&self.entry(id)?.header)
    }

    pub fn add_root(&mut self, value: Value) -> RootId {
        let id = RootId(self.next_root);
        self.next_root = self.next_root.wrapping_add(1).max(1);
        self.roots.insert(id, value);
        id
    }

    pub fn update_root(&mut self, root: RootId, value: Value) -> bool {
        self.roots.insert(root, value).is_some()
    }

    pub fn remove_root(&mut self, root: RootId) -> Option<Value> {
        self.roots.remove(&root)
    }

    pub fn alloc_string(&mut self, bytes: impl AsRef<[u8]>) -> ObjectId {
        let bytes = bytes.as_ref();
        if let Some(id) = self.interned_strings.get(bytes).copied() {
            if self.contains(id) {
                return id;
            }
        }
        let owned = bytes.to_vec();
        let id = self.alloc(HeapObject::String(owned.clone()));
        self.interned_strings.insert(owned, id);
        id
    }

    pub fn alloc_table(&mut self) -> ObjectId {
        self.alloc(HeapObject::Table(TableObject::default()))
    }

    pub fn alloc_upvalue(&mut self, value: Value, open_stack_slot: Option<u32>) -> ObjectId {
        self.alloc(HeapObject::Upvalue(UpvalueObject {
            value,
            open_stack_slot,
        }))
    }

    pub fn alloc_closure(
        &mut self,
        prototype: u32,
        upvalues: Vec<ObjectId>,
        environment: usize,
    ) -> Result<ObjectId, HeapError> {
        for upvalue in &upvalues {
            self.expect_kind(*upvalue, ObjectKind::Upvalue)?;
        }
        if environment >= upvalues.len() {
            return Err(HeapError::InvalidEnvironmentIndex {
                index: environment,
                upvalues: upvalues.len(),
            });
        }
        Ok(self.alloc(HeapObject::Closure(ClosureObject {
            prototype,
            upvalues,
            environment,
        })))
    }

    pub fn alloc_native_callable(
        &mut self,
        provider: u32,
        function: u32,
        captures: Vec<Value>,
    ) -> ObjectId {
        self.alloc(HeapObject::NativeCallable(NativeCallableObject {
            provider,
            function,
            captures,
        }))
    }

    /// Reserves a heap-local provider namespace for host/native callables.
    /// Keeping allocation here prevents two independent adapters or hosts
    /// from assigning the same provider ID inside one identity domain.
    pub fn reserve_native_provider(&mut self) -> Result<u32, HeapError> {
        let provider = self.next_native_provider;
        self.next_native_provider = self
            .next_native_provider
            .checked_add(1)
            .ok_or(HeapError::NativeProviderIdsExhausted)?;
        Ok(provider)
    }

    pub fn set_native_callable_captures(
        &mut self,
        callable: ObjectId,
        captures: Vec<Value>,
    ) -> Result<(), HeapError> {
        self.expect_kind(callable, ObjectKind::NativeCallable)?;
        let roots = captures.clone();
        let HeapObject::NativeCallable(object) = &mut self.entry_mut(callable)?.object else {
            unreachable!()
        };
        object.captures = captures;
        for value in roots {
            self.write_barrier(callable, value);
        }
        Ok(())
    }

    pub fn alloc_thread(&mut self, stack: Vec<Value>) -> ObjectId {
        self.alloc(HeapObject::Thread(ThreadObject {
            status: ThreadStatus::Suspended,
            stack,
            yielded: Vec::new(),
        }))
    }

    /// Replaces a thread snapshot after its handle has been allocated.
    /// Placeholder-first initialization is required for graphs where a live
    /// frame reaches a table or closure that points back to the thread.
    pub fn set_thread_state(
        &mut self,
        thread: ObjectId,
        status: ThreadStatus,
        stack: Vec<Value>,
        yielded: Vec<Value>,
    ) -> Result<(), HeapError> {
        self.expect_kind(thread, ObjectKind::Thread)?;
        let roots = stack.iter().chain(&yielded).copied().collect::<Vec<_>>();
        let HeapObject::Thread(object) = &mut self.entry_mut(thread)?.object else {
            unreachable!()
        };
        object.status = status;
        object.stack = stack;
        object.yielded = yielded;
        for value in roots {
            self.write_barrier(thread, value);
        }
        Ok(())
    }

    pub fn alloc_userdata(&mut self, host_handle: u64) -> ObjectId {
        self.alloc(HeapObject::Userdata(UserdataObject {
            host_handle,
            bytes: Vec::new(),
            user_values: Vec::new(),
            metatable: None,
        }))
    }

    /// Allocates canonically-owned full-userdata storage.
    pub fn alloc_userdata_bytes(&mut self, size: usize) -> ObjectId {
        self.alloc_userdata_bytes_with_uservalues(size, 0)
    }

    pub fn alloc_userdata_bytes_with_uservalues(
        &mut self,
        size: usize,
        user_values: usize,
    ) -> ObjectId {
        self.alloc(HeapObject::Userdata(UserdataObject {
            host_handle: 0,
            bytes: vec![0; size],
            user_values: vec![Value::NIL; user_values],
            metatable: None,
        }))
    }

    pub fn userdata_user_value(&self, id: ObjectId, index: usize) -> Result<Value, HeapError> {
        Ok(self
            .userdata(id)?
            .user_values
            .get(index)
            .copied()
            .unwrap_or(Value::NIL))
    }

    pub fn set_userdata_user_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: Value,
    ) -> Result<bool, HeapError> {
        let userdata = self.userdata_mut(id)?;
        let Some(slot) = userdata.user_values.get_mut(index) else {
            return Ok(false);
        };
        *slot = value;
        self.write_barrier(id, value);
        Ok(true)
    }

    pub fn userdata(&self, id: ObjectId) -> Result<&UserdataObject, HeapError> {
        self.expect_kind(id, ObjectKind::Userdata)?;
        let HeapObject::Userdata(object) = &self.entry(id)?.object else {
            unreachable!()
        };
        Ok(object)
    }

    pub fn userdata_mut(&mut self, id: ObjectId) -> Result<&mut UserdataObject, HeapError> {
        self.expect_kind(id, ObjectKind::Userdata)?;
        let HeapObject::Userdata(object) = &mut self.entry_mut(id)?.object else {
            unreachable!()
        };
        Ok(object)
    }

    pub fn alloc_error(
        &mut self,
        value: Value,
        cause: Option<ObjectId>,
        traceback: Vec<Vec<u8>>,
    ) -> Result<ObjectId, HeapError> {
        if let Some(cause) = cause {
            self.expect_kind(cause, ObjectKind::Error)?;
        }
        Ok(self.alloc(HeapObject::Error(ErrorObject {
            value,
            cause,
            traceback,
        })))
    }

    pub fn register_finalizer(&mut self, id: ObjectId) -> Result<(), HeapError> {
        self.entry_mut(id)?.header.finalizer = FinalizerState::Registered;
        Ok(())
    }

    pub fn finish_finalizer(&mut self, id: ObjectId) -> Result<(), HeapError> {
        let header = &mut self.entry_mut(id)?.header;
        if header.finalizer == FinalizerState::Queued {
            header.finalizer = FinalizerState::Finalized;
        }
        Ok(())
    }

    pub fn set_table_weak_mode(
        &mut self,
        table: ObjectId,
        weak_keys: bool,
        weak_values: bool,
    ) -> Result<(), HeapError> {
        let object = self.table_mut(table)?;
        object.weak_keys = weak_keys;
        object.weak_values = weak_values;
        object.version = object.version.wrapping_add(1);
        Ok(())
    }

    pub fn set_metatable(
        &mut self,
        object: ObjectId,
        metatable: Option<ObjectId>,
    ) -> Result<(), HeapError> {
        if let Some(metatable) = metatable {
            self.expect_kind(metatable, ObjectKind::Table)?;
        }
        match &mut self.entry_mut(object)?.object {
            HeapObject::Table(table) => {
                table.metatable = metatable;
                table.version = table.version.wrapping_add(1);
            }
            HeapObject::Userdata(userdata) => userdata.metatable = metatable,
            actual => {
                return Err(HeapError::WrongKind {
                    handle: object,
                    expected: ObjectKind::Table,
                    actual: actual.kind(),
                })
            }
        }
        if let Some(metatable) = metatable {
            self.write_barrier(object, Value::object(metatable));
        }
        Ok(())
    }

    pub fn table_get(&self, table: ObjectId, key: Value) -> Result<Value, HeapError> {
        let table = self.table(table)?;
        if let Some(index) = positive_array_index(key) {
            return Ok(table.array.get(index - 1).copied().unwrap_or(Value::NIL));
        }
        let key = self.table_key(key)?;
        Ok(table.hash.get(&key).copied().unwrap_or(Value::NIL))
    }

    /// Snapshot of live table entries in the runtime's iteration order.
    /// Used by the embedding API's `lua_next`; callers must not assume a
    /// stable order across structural mutations.
    pub fn table_entries(&mut self, table: ObjectId) -> Result<Vec<(Value, Value)>, HeapError> {
        let (array, hash) = {
            let table = self.table(table)?;
            (table.array.clone(), table.hash.clone())
        };
        let mut entries = array
            .into_iter()
            .enumerate()
            .filter(|(_, value)| *value != Value::NIL)
            .map(|(index, value)| (Value::integer(index as i64 + 1), value))
            .collect::<Vec<_>>();
        for (key, value) in hash {
            if value == Value::NIL {
                continue;
            }
            let key = match key {
                TableKey::Boolean(value) => Value::boolean(value),
                TableKey::Integer(value) => Value::integer(value),
                TableKey::Float(bits) => Value::float(f64::from_bits(bits)),
                TableKey::String(bytes) => Value::object(self.alloc_string(&bytes)),
                TableKey::Object(value) => Value::object(value),
            };
            entries.push((key, value));
        }
        Ok(entries)
    }

    pub fn table_set(
        &mut self,
        table: ObjectId,
        key: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        let array_index = positive_array_index(key);
        let hash_key = if array_index.is_none() {
            Some(self.table_key(key)?)
        } else {
            None
        };
        let object = self.table_mut(table)?;
        if let Some(index) = array_index {
            let index = index - 1;
            if index >= object.array.len() {
                object.array.resize(index + 1, Value::NIL);
            }
            object.array[index] = value;
        } else {
            // Always `insert`, even for `nil` (a tombstone, not a removal):
            // `IndexMap::remove`/`shift_remove` would either reorder or
            // shift already-live entries, exactly what the ordering
            // guarantee on `hash` above exists to prevent.
            object.hash.insert(hash_key.unwrap(), value);
        }
        object.version = object.version.wrapping_add(1);
        self.write_barrier(table, key);
        self.write_barrier(table, value);
        Ok(())
    }

    pub fn set_upvalue(&mut self, upvalue: ObjectId, value: Value) -> Result<(), HeapError> {
        self.expect_kind(upvalue, ObjectKind::Upvalue)?;
        let HeapObject::Upvalue(cell) = &mut self.entry_mut(upvalue)?.object else {
            unreachable!()
        };
        cell.value = value;
        self.write_barrier(upvalue, value);
        Ok(())
    }

    pub fn collect_major(&mut self) -> Collection {
        self.collect(CollectionKind::Major, &[])
    }

    pub fn collect_major_with_roots(&mut self, frame_roots: &[Value]) -> Collection {
        self.collect(CollectionKind::Major, frame_roots)
    }

    /// The initial U2 collector shares the precise full tracing algorithm for
    /// minor and major collections. The remembered set and generations are
    /// already maintained, allowing a later performance-only change to limit
    /// minor tracing without changing object semantics.
    pub fn collect_minor(&mut self) -> Collection {
        self.collect(CollectionKind::Minor, &[])
    }

    fn alloc(&mut self, object: HeapObject) -> ObjectId {
        let kind = object.kind();
        let entry = Entry {
            header: ObjectHeader {
                kind,
                generation: GcGeneration::Young,
                finalizer: FinalizerState::None,
            },
            object,
        };
        if let Some(slot_index) = self.free.pop() {
            let generation = self.slots[slot_index].generation;
            self.slots[slot_index].entry = Some(entry);
            ObjectId::new(slot_index, generation)
        } else {
            let slot_index = self.slots.len();
            self.slots.push(Slot {
                generation: 0,
                entry: Some(entry),
            });
            ObjectId::new(slot_index, 0)
        }
    }

    fn entry(&self, id: ObjectId) -> Result<&Entry, HeapError> {
        self.slots
            .get(id.slot())
            .filter(|slot| slot.generation == id.generation())
            .and_then(|slot| slot.entry.as_ref())
            .ok_or(HeapError::StaleHandle(id))
    }

    fn entry_mut(&mut self, id: ObjectId) -> Result<&mut Entry, HeapError> {
        self.slots
            .get_mut(id.slot())
            .filter(|slot| slot.generation == id.generation())
            .and_then(|slot| slot.entry.as_mut())
            .ok_or(HeapError::StaleHandle(id))
    }

    fn expect_kind(&self, id: ObjectId, expected: ObjectKind) -> Result<(), HeapError> {
        let actual = self.entry(id)?.header.kind;
        if actual == expected {
            Ok(())
        } else {
            Err(HeapError::WrongKind {
                handle: id,
                expected,
                actual,
            })
        }
    }

    fn table(&self, id: ObjectId) -> Result<&TableObject, HeapError> {
        match &self.entry(id)?.object {
            HeapObject::Table(table) => Ok(table),
            actual => Err(HeapError::WrongKind {
                handle: id,
                expected: ObjectKind::Table,
                actual: actual.kind(),
            }),
        }
    }

    fn table_mut(&mut self, id: ObjectId) -> Result<&mut TableObject, HeapError> {
        match &mut self.entry_mut(id)?.object {
            HeapObject::Table(table) => Ok(table),
            actual => Err(HeapError::WrongKind {
                handle: id,
                expected: ObjectKind::Table,
                actual: actual.kind(),
            }),
        }
    }

    fn table_key(&self, value: Value) -> Result<TableKey, HeapError> {
        match value.tag() {
            ValueTag::Nil => Err(HeapError::NilTableKey),
            ValueTag::Boolean => Ok(TableKey::Boolean(value.as_bool().unwrap())),
            ValueTag::Integer => Ok(TableKey::Integer(value.as_integer().unwrap())),
            ValueTag::Float => {
                let float = value.as_float().unwrap();
                if float.is_nan() {
                    return Err(HeapError::NanTableKey);
                }
                if let Some(integer) = exact_integer(float) {
                    Ok(TableKey::Integer(integer))
                } else {
                    Ok(TableKey::Float(canonical_float_bits(float)))
                }
            }
            ValueTag::Object => {
                let object = value.as_object().unwrap();
                match self.object(object)? {
                    HeapObject::String(bytes) => Ok(TableKey::String(bytes.clone())),
                    _ => Ok(TableKey::Object(object)),
                }
            }
        }
    }

    fn write_barrier(&mut self, owner: ObjectId, value: Value) {
        let Some(child) = value.as_object() else {
            return;
        };
        let owner_old = self
            .entry(owner)
            .is_ok_and(|entry| entry.header.generation == GcGeneration::Old);
        let child_young = self
            .entry(child)
            .is_ok_and(|entry| entry.header.generation == GcGeneration::Young);
        if owner_old && child_young {
            self.remembered.insert(owner);
        }
    }

    fn collect(&mut self, kind: CollectionKind, frame_roots: &[Value]) -> Collection {
        let mut marked = HashSet::new();
        let mut queue = VecDeque::new();
        let roots: Vec<Value> = self.roots.values().copied().collect();
        for root in roots {
            mark_value(root, self, &mut marked, &mut queue);
        }
        for root in frame_roots {
            mark_value(*root, self, &mut marked, &mut queue);
        }
        self.drain_mark_queue(&mut marked, &mut queue);
        self.mark_ephemerons(&mut marked, &mut queue);

        let mut result = Collection {
            kind,
            reclaimed: 0,
            promoted: 0,
            finalizers: Vec::new(),
        };
        let unreachable_finalizers: Vec<ObjectId> = self
            .live_ids()
            .filter(|id| {
                !marked.contains(id)
                    && self
                        .entry(*id)
                        .is_ok_and(|entry| entry.header.finalizer == FinalizerState::Registered)
            })
            .collect();
        for id in unreachable_finalizers {
            if let Ok(entry) = self.entry_mut(id) {
                entry.header.finalizer = FinalizerState::Queued;
            }
            result.finalizers.push(id);
            mark_id(id, self, &mut marked, &mut queue);
        }
        self.drain_mark_queue(&mut marked, &mut queue);
        self.mark_ephemerons(&mut marked, &mut queue);
        self.sweep_weak_tables(&marked);

        for index in 0..self.slots.len() {
            let id = ObjectId::new(index, self.slots[index].generation);
            if self.slots[index].entry.is_none() {
                continue;
            }
            if marked.contains(&id) {
                let header = &mut self.slots[index].entry.as_mut().unwrap().header;
                if header.generation == GcGeneration::Young {
                    header.generation = GcGeneration::Old;
                    result.promoted += 1;
                }
                continue;
            }
            self.slots[index].entry = None;
            self.slots[index].generation = self.slots[index].generation.wrapping_add(1);
            self.free.push(index);
            result.reclaimed += 1;
        }
        let live_objects: HashSet<ObjectId> = self.live_ids().collect();
        self.remembered.retain(|id| live_objects.contains(id));
        self.interned_strings
            .retain(|_, id| live_objects.contains(id));
        result
    }

    fn live_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.entry
                .as_ref()
                .map(|_| ObjectId::new(index, slot.generation))
        })
    }

    fn drain_mark_queue(&self, marked: &mut HashSet<ObjectId>, queue: &mut VecDeque<ObjectId>) {
        while let Some(id) = queue.pop_front() {
            let Ok(entry) = self.entry(id) else {
                continue;
            };
            match &entry.object {
                HeapObject::String(_) => {}
                HeapObject::Table(table) => {
                    if let Some(metatable) = table.metatable {
                        mark_id(metatable, self, marked, queue);
                    }
                    if !table.weak_values {
                        for value in &table.array {
                            mark_value(*value, self, marked, queue);
                        }
                    }
                    for (key, value) in &table.hash {
                        if !table.weak_keys {
                            mark_key(key, self, marked, queue);
                        }
                        if !table.weak_values && !table.weak_keys {
                            mark_value(*value, self, marked, queue);
                        }
                    }
                }
                HeapObject::Closure(closure) => {
                    for upvalue in &closure.upvalues {
                        mark_id(*upvalue, self, marked, queue);
                    }
                }
                HeapObject::NativeCallable(callable) => {
                    for value in &callable.captures {
                        mark_value(*value, self, marked, queue);
                    }
                }
                HeapObject::Upvalue(upvalue) => mark_value(upvalue.value, self, marked, queue),
                HeapObject::Thread(thread) => {
                    for value in thread.stack.iter().chain(&thread.yielded) {
                        mark_value(*value, self, marked, queue);
                    }
                }
                HeapObject::Userdata(userdata) => {
                    if let Some(metatable) = userdata.metatable {
                        mark_id(metatable, self, marked, queue);
                    }
                    for value in &userdata.user_values {
                        mark_value(*value, self, marked, queue);
                    }
                }
                HeapObject::Error(error) => {
                    mark_value(error.value, self, marked, queue);
                    if let Some(cause) = error.cause {
                        mark_id(cause, self, marked, queue);
                    }
                }
            }
        }
    }

    fn mark_ephemerons(&self, marked: &mut HashSet<ObjectId>, queue: &mut VecDeque<ObjectId>) {
        // Which objects are weak-key (non-weak-value) tables is a structural
        // property of the heap that doesn't change during collection, so
        // it's computed once here, over *all* live objects. The previous
        // version instead re-derived "which of the currently marked ids are
        // ephemeron tables" by re-collecting and rescanning the *entire*
        // `marked` set on every fixed-point round - most of which is never a
        // table at all - so each round's cost grew with the (monotonically
        // growing) total marked-object count rather than with the number of
        // ephemeron tables, making the whole loop quadratic in marked-object
        // count whenever more than one round was needed to converge (e.g. a
        // chain of ephemeron tables whose values key into each other). Each
        // round below instead only walks this fixed, typically-much-smaller
        // candidate list and skips any not yet marked.
        let ephemeron_tables: Vec<ObjectId> = self
            .live_ids()
            .filter(|id| {
                matches!(
                    self.object(*id),
                    Ok(HeapObject::Table(table)) if table.weak_keys && !table.weak_values
                )
            })
            .collect();
        loop {
            let before = marked.len();
            for &id in &ephemeron_tables {
                if !marked.contains(&id) {
                    continue;
                }
                let Ok(HeapObject::Table(table)) = self.object(id) else {
                    continue;
                };
                for (key, value) in &table.hash {
                    if key_is_live(key, marked) {
                        mark_value(*value, self, marked, queue);
                    }
                }
                // Array keys are scalar integers and therefore always live.
                for value in &table.array {
                    mark_value(*value, self, marked, queue);
                }
            }
            self.drain_mark_queue(marked, queue);
            if marked.len() == before {
                break;
            }
        }
    }

    fn sweep_weak_tables(&mut self, marked: &HashSet<ObjectId>) {
        for slot in &mut self.slots {
            let Some(Entry {
                object: HeapObject::Table(table),
                ..
            }) = &mut slot.entry
            else {
                continue;
            };
            if table.weak_values {
                for value in &mut table.array {
                    if value.as_object().is_some_and(|id| !marked.contains(&id)) {
                        *value = Value::NIL;
                    }
                }
            }
            table.hash.retain(|key, value| {
                let key_alive = !table.weak_keys || key_is_live(key, marked);
                let value_alive =
                    !table.weak_values || value.as_object().is_none_or(|id| marked.contains(&id));
                key_alive && value_alive
            });
        }
    }
}

fn positive_array_index(value: Value) -> Option<usize> {
    let integer = match value.tag() {
        ValueTag::Integer => value.as_integer()?,
        ValueTag::Float => exact_integer(value.as_float()?)?,
        _ => return None,
    };
    (integer > 0).then_some(integer as usize)
}

fn exact_integer(value: f64) -> Option<i64> {
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value < I64_UPPER_EXCLUSIVE
    {
        Some(value as i64)
    } else {
        None
    }
}

fn canonical_float_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn mark_value(
    value: Value,
    heap: &Heap,
    marked: &mut HashSet<ObjectId>,
    queue: &mut VecDeque<ObjectId>,
) {
    if let Some(id) = value.as_object() {
        mark_id(id, heap, marked, queue);
    }
}

fn mark_id(
    id: ObjectId,
    heap: &Heap,
    marked: &mut HashSet<ObjectId>,
    queue: &mut VecDeque<ObjectId>,
) {
    if heap.contains(id) && marked.insert(id) {
        queue.push_back(id);
    }
}

fn mark_key(
    key: &TableKey,
    heap: &Heap,
    marked: &mut HashSet<ObjectId>,
    queue: &mut VecDeque<ObjectId>,
) {
    if let TableKey::Object(id) = key {
        mark_id(*id, heap, marked, queue);
    }
}

fn key_is_live(key: &TableKey, marked: &HashSet<ObjectId>) -> bool {
    match key {
        TableKey::Object(id) => marked.contains(id),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_identity_and_stale_handles_survive_slot_reuse() {
        let mut heap = Heap::default();
        let first = heap.alloc_table();
        let same = Value::object(first);
        assert_eq!(same, Value::object(first));
        let weak = heap.downgrade(first).unwrap();
        assert_eq!(heap.collect_major().reclaimed, 1);
        assert_eq!(heap.upgrade(weak), None);

        let second = heap.alloc_table();
        assert_ne!(first, second);
        assert!(heap.object(first).is_err());
        assert!(heap.object(second).is_ok());
    }

    #[test]
    fn strings_are_interned_and_table_numeric_keys_are_canonical() {
        let mut heap = Heap::default();
        let one = heap.alloc_string(b"name");
        let two = heap.alloc_string(b"name");
        assert_eq!(one, two);

        let table = heap.alloc_table();
        heap.table_set(table, Value::float(1.0), Value::integer(42))
            .unwrap();
        assert_eq!(
            heap.table_get(table, Value::integer(1)).unwrap(),
            Value::integer(42)
        );
        heap.table_set(table, Value::float(-0.0), Value::integer(7))
            .unwrap();
        assert_eq!(
            heap.table_get(table, Value::float(0.0)).unwrap(),
            Value::integer(7)
        );
    }

    #[test]
    fn table_hash_part_preserves_insertion_order_across_overwrite_and_tombstone() {
        // Regression test for `TableObject::hash` being an `IndexMap`, not a
        // `HashMap`: overwriting an existing key must not move it, and
        // clearing a key with `nil` (a tombstone, not a removal) must not
        // reorder or drop the entries that come after it - exactly the
        // corruption a plain `HashMap` risks on insert-time reordering,
        // which would silently strand or duplicate entries for a caller
        // walking `table_entries` (`lua_next`) while mutating.
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        let key_a = heap.alloc_string("a");
        let key_b = heap.alloc_string("b");
        let key_c = heap.alloc_string("c");
        heap.table_set(table, Value::object(key_a), Value::integer(1))
            .unwrap();
        heap.table_set(table, Value::object(key_b), Value::integer(2))
            .unwrap();
        heap.table_set(table, Value::object(key_c), Value::integer(3))
            .unwrap();

        // Overwriting an existing key keeps its original position.
        heap.table_set(table, Value::object(key_b), Value::integer(20))
            .unwrap();
        // Clearing a key with `nil` tombstones it in place instead of
        // shifting later entries into its slot.
        heap.table_set(table, Value::object(key_a), Value::NIL)
            .unwrap();

        let entries = heap.table_entries(table).unwrap();
        let values: Vec<i64> = entries
            .into_iter()
            .map(|(_, value)| value.as_integer().unwrap())
            .collect();
        assert_eq!(values, vec![20, 3]);
    }

    #[test]
    fn closure_environment_is_a_real_traced_upvalue() {
        let mut heap = Heap::default();
        let environment = heap.alloc_table();
        let environment_cell = heap.alloc_upvalue(Value::object(environment), None);
        let closure = heap.alloc_closure(7, vec![environment_cell], 0).unwrap();
        let root = heap.add_root(Value::object(closure));

        heap.collect_major();
        assert!(heap.contains(environment));
        assert!(heap.contains(environment_cell));
        heap.remove_root(root);
        assert_eq!(heap.collect_major().reclaimed, 3);
    }

    #[test]
    fn native_callable_registry_ids_and_captures_are_portable_roots() {
        let mut heap = Heap::default();
        assert_eq!(heap.reserve_native_provider().unwrap(), 1);
        assert_eq!(heap.reserve_native_provider().unwrap(), 2);
        let captured = heap.alloc_table();
        let callable = heap.alloc_native_callable(7, 11, vec![Value::object(captured)]);
        let HeapObject::NativeCallable(object) = heap.object(callable).unwrap() else {
            panic!("allocated native callable has the wrong object kind")
        };
        assert_eq!((object.provider, object.function), (7, 11));

        let root = heap.add_root(Value::object(callable));
        heap.collect_major();
        assert!(heap.contains(callable));
        assert!(heap.contains(captured));
        heap.remove_root(root);
        assert_eq!(heap.collect_major().reclaimed, 2);
    }

    #[test]
    fn weak_values_clear_without_retaining_the_target() {
        let mut heap = Heap::default();
        let weak_table = heap.alloc_table();
        heap.set_table_weak_mode(weak_table, false, true).unwrap();
        let table_root = heap.add_root(Value::object(weak_table));
        let target = heap.alloc_table();
        heap.table_set(weak_table, Value::integer(1), Value::object(target))
            .unwrap();

        heap.collect_major();
        assert!(!heap.contains(target));
        assert_eq!(
            heap.table_get(weak_table, Value::integer(1)).unwrap(),
            Value::NIL
        );
        heap.remove_root(table_root);
    }

    #[test]
    fn ephemeron_values_live_only_while_their_object_keys_live() {
        let mut heap = Heap::default();
        let ephemeron = heap.alloc_table();
        heap.set_table_weak_mode(ephemeron, true, false).unwrap();
        let table_root = heap.add_root(Value::object(ephemeron));
        let key = heap.alloc_table();
        let value = heap.alloc_table();
        heap.table_set(ephemeron, Value::object(key), Value::object(value))
            .unwrap();
        let key_root = heap.add_root(Value::object(key));

        heap.collect_major();
        assert!(heap.contains(key));
        assert!(heap.contains(value));
        heap.remove_root(key_root);
        heap.collect_major();
        assert!(!heap.contains(key));
        assert!(!heap.contains(value));
        assert_eq!(
            heap.table_get(ephemeron, Value::object(key)).unwrap_err(),
            HeapError::StaleHandle(key)
        );
        heap.remove_root(table_root);
    }

    #[test]
    fn a_chain_of_ephemeron_tables_reaches_a_fixed_point_across_multiple_rounds() {
        // Regression test for `mark_ephemerons`'s fixed-point loop: `table_b`
        // is only reachable as a *value* inside `table_a`'s single entry, so
        // it isn't marked until the loop's first round processes `table_a` -
        // it must still be recognized (on the loop's second round) as an
        // ephemeron table in its own right and have its own entry scanned,
        // not just be marked and left unscanned. This distinguishes "an
        // object became marked" from "an object became marked *and* is
        // itself an ephemeron table whose own entries still need a round to
        // be examined" - the case the quadratic-rescan fix above must still
        // get right despite computing its candidate table list once, up
        // front, rather than re-deriving it from `marked` every round.
        let mut heap = Heap::default();
        let table_a = heap.alloc_table();
        heap.set_table_weak_mode(table_a, true, false).unwrap();
        let root_a = heap.add_root(Value::object(table_a));
        let key_a = heap.alloc_table();
        let root_key_a = heap.add_root(Value::object(key_a));
        let table_b = heap.alloc_table();
        heap.set_table_weak_mode(table_b, true, false).unwrap();
        heap.table_set(table_a, Value::object(key_a), Value::object(table_b))
            .unwrap();
        let key_b = heap.alloc_table();
        let root_key_b = heap.add_root(Value::object(key_b));
        let value_b = heap.alloc_table();
        heap.table_set(table_b, Value::object(key_b), Value::object(value_b))
            .unwrap();

        heap.collect_major();
        assert!(heap.contains(table_b));
        assert!(heap.contains(value_b));

        heap.remove_root(root_a);
        heap.remove_root(root_key_a);
        heap.remove_root(root_key_b);
    }

    #[test]
    fn finalizers_are_queued_once_and_objects_survive_the_callback_window() {
        let mut heap = Heap::default();
        let userdata = heap.alloc_userdata(99);
        heap.register_finalizer(userdata).unwrap();

        let first = heap.collect_major();
        assert_eq!(first.finalizers, vec![userdata]);
        assert!(heap.contains(userdata));
        heap.finish_finalizer(userdata).unwrap();
        let second = heap.collect_major();
        assert!(second.finalizers.is_empty());
        assert!(!heap.contains(userdata));
    }

    #[test]
    fn coroutine_stacks_are_precise_roots() {
        let mut heap = Heap::default();
        let captured = heap.alloc_table();
        let thread = heap.alloc_thread(vec![Value::object(captured)]);
        let root = heap.add_root(Value::object(thread));
        heap.collect_minor();
        assert!(heap.contains(captured));
        assert_eq!(heap.header(captured).unwrap().generation, GcGeneration::Old);

        heap.remove_root(root);
        assert_eq!(heap.collect_major().reclaimed, 2);
    }

    #[test]
    fn userdata_user_values_are_traced_and_use_the_generational_barrier() {
        let mut heap = Heap::default();
        let userdata = heap.alloc_userdata_bytes_with_uservalues(8, 1);
        let root = heap.add_root(Value::object(userdata));
        heap.collect_major();

        let child = heap.alloc_table();
        assert!(heap
            .set_userdata_user_value(userdata, 0, Value::object(child))
            .unwrap());
        assert!(heap.remembered.contains(&userdata));
        heap.collect_minor();
        assert!(heap.contains(child));
        assert_eq!(
            heap.userdata_user_value(userdata, 0).unwrap(),
            Value::object(child)
        );

        heap.set_userdata_user_value(userdata, 0, Value::NIL)
            .unwrap();
        heap.collect_major();
        assert!(!heap.contains(child));
        heap.remove_root(root);
    }

    #[test]
    fn old_to_young_mutations_remain_reachable_under_minor_collection() {
        let mut heap = Heap::default();
        let owner = heap.alloc_table();
        let root = heap.add_root(Value::object(owner));
        heap.collect_major();
        let child = heap.alloc_table();
        heap.table_set(owner, Value::integer(1), Value::object(child))
            .unwrap();
        assert!(heap.remembered.contains(&owner));
        heap.collect_minor();
        assert!(heap.contains(child));
        heap.remove_root(root);
    }

    #[test]
    fn stack_maps_expose_only_declared_precise_frame_roots() {
        let mut heap = Heap::default();
        let live = heap.alloc_table();
        let dead = heap.alloc_table();
        let frame = [Value::integer(99), Value::object(live), Value::object(dead)];
        let roots = StackMap::new(vec![1]).roots(&frame).unwrap();
        let collection = heap.collect_major_with_roots(&roots);
        assert_eq!(collection.reclaimed, 1);
        assert!(heap.contains(live));
        assert!(!heap.contains(dead));
        assert_eq!(
            StackMap::new(vec![3]).roots(&frame),
            Err(StackMapError {
                slot: 3,
                frame_len: 3,
            })
        );
    }
}
