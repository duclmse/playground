use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::rc::Rc;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableKey {
    Boolean(bool),
    Integer(i64),
    Float(u64),
    /// The precomputed `u64` is `StringObject::hash` carried along so hashing
    /// this key (every table/global access by name) never re-hashes the byte
    /// content - see `TableKey`'s manual `Hash` impl below and
    /// `StringObject`'s own doc comment for where the digest is computed.
    String(Vec<u8>, u64),
    Object(ObjectId),
}

/// Derived `Hash` would re-hash `String`'s full byte content on every table or
/// global access by name; this manual impl substitutes the precomputed digest
/// instead; `PartialEq`/`Eq` (still derived, above) keep comparing exact bytes,
/// so this only memoizes the hash, it doesn't change key equality.
impl std::hash::Hash for TableKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Boolean(value) => value.hash(state),
            Self::Integer(value) => value.hash(state),
            Self::Float(bits) => bits.hash(state),
            Self::String(_, hash) => state.write_u64(*hash),
            Self::Object(id) => id.hash(state),
        }
    }
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
    /// Length of the longest confirmed non-`Value::NIL` prefix of `array`: a
    /// "border" per the Lua manual's `#` operator (any `n` with `t[n] != nil`
    /// and `t[n+1] == nil`; undefined when the array part has holes).
    /// Maintained incrementally by `Heap::table_set` so `t[#t + 1] = v` in a
    /// loop stays O(1) amortized rather than O(n^2) - see
    /// `sol::lua_runtime::value::LuaTable::array_border`'s identical
    /// rationale. `Heap::table_len` recomputes from scratch for the rare
    /// paths that mutate `array` directly instead of through `table_set`.
    pub array_border: usize,
}

#[derive(Debug, Clone)]
pub struct ClosureObject {
    pub prototype: u32,
    /// Shared, not cloned, by every call to this closure - a closure's set
    /// of captured upvalue cells never changes after construction (only
    /// `debug.upvaluejoin` rebinds a single slot, through `Cell::set`), so
    /// resolving a closure for a call is a refcount bump instead of a fresh
    /// `Vec` allocation + element copy.
    pub upvalues: Rc<[Cell<ObjectId>]>,
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

/// A string's bytes plus a lazily-computed, memoized hash - a stable digest
/// (not any particular `HashMap`/`IndexMap`'s own randomized hasher), cached
/// so using a string as a table or global key repeatedly never re-hashes its
/// byte content more than once. Lua strings are immutable once allocated, so
/// the digest never goes stale once computed.
///
/// Computed on first use rather than at allocation time: most allocated
/// strings (concatenation results, library return values, ...) are never
/// used as a table/global key at all, so hashing every one of them up front
/// would charge every string allocation for a cost only key-used strings
/// actually need - confirmed by benchmark (`string_concat` regressed from
/// ~62ms to ~137ms under an eager-hash design before this was changed to
/// lazy).
#[derive(Debug, Clone)]
pub struct StringObject {
    pub bytes: Vec<u8>,
    hash: Cell<Option<u64>>,
}

impl StringObject {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            hash: Cell::new(None),
        }
    }

    pub fn hash(&self) -> u64 {
        if let Some(hash) = self.hash.get() {
            return hash;
        }
        let hash = hash_string_bytes(&self.bytes);
        self.hash.set(Some(hash));
        hash
    }
}

fn hash_string_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Clone)]
pub enum HeapObject {
    String(StringObject),
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
    InvalidNextKey,
    InvalidUpvalueIndex { index: usize, upvalues: usize },
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
            Self::InvalidNextKey => f.write_str("invalid key to 'next'"),
            Self::InvalidUpvalueIndex { index, upvalues } => write!(
                f,
                "upvalue index {index} is outside {upvalues} upvalues"
            ),
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

/// Which half of a resumable [`Heap::step_major_with_conditional_roots`]
/// cycle is in progress. Marking and sweeping never overlap: sweeping only
/// starts once the mark queue has fully drained and converged (ephemerons
/// and finalizer-reachability included), mirroring `collect`'s own two-phase
/// structure but split across calls instead of run to completion in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncrementalPhase {
    Marking,
    Sweeping { next_index: usize },
}

/// Persisted state for a major collection spread across multiple
/// `collectgarbage("step", ...)` calls. `marked`/`queue` are the same
/// gray-set/queue a one-shot `collect()` uses locally, just kept alive
/// between calls instead of living only on the stack of a single call.
struct IncrementalCycle {
    kind: CollectionKind,
    marked: HashSet<ObjectId>,
    queue: VecDeque<ObjectId>,
    phase: IncrementalPhase,
    reclaimed: usize,
    promoted: usize,
    finalizers: Vec<ObjectId>,
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
    incremental: Option<IncrementalCycle>,
    pub capabilities: Capabilities,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new(Capabilities::SANDBOX)
    }
}

impl Heap {
    /// Real Lua's `LUAI_MAXSHORTLEN`: strings at or under this length are
    /// always interned, regardless of whether they're a compile-time
    /// literal or a runtime computation.
    const MAX_SHORT_STRING_LEN: usize = 40;

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
            incremental: None,
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
        let id = self.alloc(HeapObject::String(StringObject::new(owned.clone())));
        self.interned_strings.insert(owned, id);
        id
    }

    /// Allocates a string object the way a runtime computation (string
    /// concatenation, a library call's result, ...) should: real Lua always
    /// interns short strings (`LUAI_MAXSHORTLEN`, 40 bytes) regardless of
    /// where they come from, so bytes at or under that length still
    /// deduplicate against an existing equal-content string here. Past that
    /// length, real Lua never interns, so every call allocates a distinct
    /// `ObjectId`, even for bytes equal to an already interned or previously
    /// fresh-allocated long string — a long runtime-computed string that
    /// happens to equal an existing string's bytes must still compare
    /// unequal by identity. Use `alloc_string` instead for values that
    /// should always alias existing equal-content strings regardless of
    /// length (e.g. compile-time literal constants); use this for
    /// everything computed at run time.
    pub fn alloc_string_fresh(&mut self, bytes: impl AsRef<[u8]>) -> ObjectId {
        let bytes = bytes.as_ref();
        if bytes.len() <= Self::MAX_SHORT_STRING_LEN {
            return self.alloc_string(bytes);
        }
        self.alloc(HeapObject::String(StringObject::new(bytes.to_vec())))
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
        let upvalues: Rc<[Cell<ObjectId>]> = upvalues.into_iter().map(Cell::new).collect();
        Ok(self.alloc(HeapObject::Closure(ClosureObject {
            prototype,
            upvalues,
            environment,
        })))
    }

    /// Rebinds one of a closure's upvalue cells to a different `Upvalue`
    /// object - the primitive `debug.upvaluejoin` needs to alias two
    /// closures' upvalues onto the same shared cell.
    pub fn set_closure_upvalue(
        &mut self,
        closure: ObjectId,
        index: usize,
        upvalue: ObjectId,
    ) -> Result<(), HeapError> {
        self.expect_kind(closure, ObjectKind::Closure)?;
        self.expect_kind(upvalue, ObjectKind::Upvalue)?;
        let HeapObject::Closure(object) = &mut self.entry_mut(closure)?.object else {
            unreachable!()
        };
        if index >= object.upvalues.len() {
            return Err(HeapError::InvalidUpvalueIndex {
                index,
                upvalues: object.upvalues.len(),
            });
        }
        object.upvalues[index].set(upvalue);
        self.write_barrier(closure, Value::object(upvalue));
        Ok(())
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
        // A positive integer key doesn't always live in the array part - see
        // `should_grow_array` on `table_set` - so a key beyond the array's
        // current length must still be looked up in the hash part rather
        // than assumed absent.
        if let Some(index) = positive_array_index(key) {
            let object = self.table(table)?;
            if index <= object.array.len() {
                return Ok(object.array.get(index - 1).copied().unwrap_or(Value::NIL));
            }
        }
        let key = self.table_key(key)?;
        Ok(self.table(table)?.hash.get(&key).copied().unwrap_or(Value::NIL))
    }

    /// The `#` operator: the table's cached array border (see
    /// `TableObject::array_border`), O(1) amortized as long as every mutation
    /// went through `table_set`.
    pub fn table_len(&self, table: ObjectId) -> Result<usize, HeapError> {
        Ok(self.table(table)?.array_border)
    }

    /// Recomputes `array_border` from scratch by scanning for the longest
    /// non-nil prefix. Only needed after code that mutates a table's `array`
    /// directly instead of through `table_set` (e.g. weak-table sweep pruning
    /// stale array slots to `Value::NIL` in place).
    pub fn recompute_table_len(&mut self, table: ObjectId) -> Result<(), HeapError> {
        let object = self.table_mut(table)?;
        object.array_border = object
            .array
            .iter()
            .take_while(|value| **value != Value::NIL)
            .count();
        Ok(())
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
                TableKey::String(bytes, _) => Value::object(self.alloc_string(&bytes)),
                TableKey::Object(value) => Value::object(value),
            };
            entries.push((key, value));
        }
        Ok(entries)
    }

    /// `next(t, key)`: the key/value pair immediately after `key` in this
    /// table's iteration order (array part in index order, then the hash
    /// part in insertion order), skipping tombstoned (nil-valued) entries,
    /// or `None` once iteration is exhausted. `key == Value::NIL` starts
    /// from the beginning. A key that was live when last returned by
    /// `next` - and has since been set to nil, which real Lua explicitly
    /// permits mid-traversal - can still be located to resume from; any
    /// other unrecognized key is `HeapError::InvalidNextKey`. Mirrors
    /// `sol::lua_runtime::dispatch::LuaRuntime::next`'s tombstone-tolerant
    /// resume behavior over `LuaTable::entries_with_tombstones`.
    pub fn table_next(
        &mut self,
        table: ObjectId,
        key: Value,
    ) -> Result<Option<(Value, Value)>, HeapError> {
        let object = self.table(table)?;
        let array_len = object.array.len();
        let start = if key == Value::NIL {
            0
        } else if let Some(index) = positive_array_index(key).filter(|index| *index <= array_len)
        {
            index
        } else {
            let hash_key = self.table_key(key)?;
            let object = self.table(table)?;
            let position = object
                .hash
                .get_index_of(&hash_key)
                .ok_or(HeapError::InvalidNextKey)?;
            array_len + position + 1
        };

        let object = self.table(table)?;
        if start < array_len {
            if let Some((offset, value)) = object.array[start..]
                .iter()
                .enumerate()
                .find(|(_, value)| **value != Value::NIL)
            {
                return Ok(Some((Value::integer((start + offset + 1) as i64), *value)));
            }
        }
        let hash_start = start.saturating_sub(array_len);
        let found = object
            .hash
            .iter()
            .skip(hash_start)
            .find(|(_, value)| **value != Value::NIL)
            .map(|(key, value)| (key.clone(), *value));
        let Some((key, value)) = found else {
            return Ok(None);
        };
        let key = match key {
            TableKey::Boolean(value) => Value::boolean(value),
            TableKey::Integer(value) => Value::integer(value),
            TableKey::Float(bits) => Value::float(f64::from_bits(bits)),
            TableKey::String(bytes, _) => Value::object(self.alloc_string(&bytes)),
            TableKey::Object(value) => Value::object(value),
        };
        Ok(Some((key, value)))
    }

    pub fn table_set(
        &mut self,
        table: ObjectId,
        key: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        // A positive integer key only takes the array-part fast path when
        // doing so wouldn't require growing the array far beyond its
        // current length - see `should_grow_array`. A sparse/huge key (e.g.
        // `t[math.maxinteger] = v`) instead falls to the hash branch below,
        // exactly like any other non-array-eligible key; `table_get` and
        // `table_next` already know to check the hash part for a positive
        // integer key beyond the array's current length.
        let current_array_len = self.table(table)?.array.len();
        let array_index =
            positive_array_index(key).filter(|&index| should_grow_array(current_array_len, index));
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
            if value != Value::NIL {
                // Filling exactly the slot right past the confirmed non-nil
                // prefix extends it - the common `t[#t + 1] = v` append idiom
                // hits this every time, keeping the border O(1) amortized. A
                // `while` (not `if`) because a prior out-of-order write past
                // the border may already have left later slots non-nil too.
                if index == object.array_border {
                    object.array_border += 1;
                    while object.array_border < object.array.len()
                        && object.array[object.array_border] != Value::NIL
                    {
                        object.array_border += 1;
                    }
                }
            } else if index < object.array_border {
                // The confirmed prefix can no longer include `index`, but
                // everything strictly before it is still confirmed.
                object.array_border = index;
            }
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

    pub fn upvalue_value(&self, upvalue: ObjectId) -> Result<Value, HeapError> {
        match &self.entry(upvalue)?.object {
            HeapObject::Upvalue(cell) => Ok(cell.value),
            actual => Err(HeapError::WrongKind {
                handle: upvalue,
                expected: ObjectKind::Upvalue,
                actual: actual.kind(),
            }),
        }
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
        self.collect(CollectionKind::Major, &[], &HashMap::new())
    }

    pub fn collect_major_with_roots(&mut self, frame_roots: &[Value]) -> Collection {
        self.collect(CollectionKind::Major, frame_roots, &HashMap::new())
    }

    /// Like [`collect_major_with_roots`](Self::collect_major_with_roots), but
    /// `conditional_roots` supplies extra out-edges for `Thread` objects
    /// specifically: whenever a `Thread` id in this map is (or becomes, via
    /// the ordinary mark queue) reachable, every `Value` in its associated
    /// list is marked too, exactly as if it were one of that `Thread`'s own
    /// fields. This is a strict one-way dependency (a conditionally-rooted
    /// value never makes the `Thread` itself more reachable), so unlike
    /// [`mark_ephemerons`](Self::mark_ephemerons)'s genuine fixed point it
    /// needs no separate round-trip loop: `drain_mark_queue` already visits
    /// every marked id exactly once, so looking the id up in this map at
    /// that single visit is sufficient, including for a `Thread` id that only
    /// becomes reachable *because* an ephemeron round or another
    /// conditionally-rooted `Thread` pulled it in - all three mechanisms
    /// share the same `queue`/`marked` pair, so whichever one first marks an
    /// id is followed, on that same id's single queue visit, by any edges
    /// this map has for it. A `Thread` id absent from this map (not a
    /// registered coroutine, or a coroutine already covered by an
    /// unconditional root elsewhere) contributes nothing extra.
    ///
    /// This is `sol_core`'s half of closing the coroutine-frame-rooting gap
    /// documented in
    /// `docs/features/table-closure-coroutine-cutover.md` §6/§11: a
    /// coroutine kept alive only by a reference cycle routed through its own
    /// suspended frames is correctly *not* rooted by this map alone (nothing
    /// external ever marks its `Thread` id to begin with), while one
    /// reachable from any ordinary root (ephemeron-qualified or not) has its
    /// frame contents rooted too, matching the liveness a directly-embedded
    /// `HeapObject` field would get for free.
    pub fn collect_major_with_conditional_roots(
        &mut self,
        frame_roots: &[Value],
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
    ) -> Collection {
        self.collect(CollectionKind::Major, frame_roots, conditional_roots)
    }

    /// A genuine young-generation-only collection: see
    /// [`collect_minor_with_conditional_roots`](Self::collect_minor_with_conditional_roots).
    pub fn collect_minor(&mut self) -> Collection {
        self.collect(CollectionKind::Minor, &[], &HashMap::new())
    }

    /// `collectgarbage("step")`'s generational-mode counterpart to
    /// [`step_major_with_conditional_roots`](Self::step_major_with_conditional_roots):
    /// one complete, cheap young-generation-only mark/sweep pass, run to
    /// completion in a single call rather than resumed across several - real
    /// Lua's own minor collections are similarly single-shot, since the
    /// young generation is kept small by design, so there's no need to
    /// bound and resume the work the way a much larger major cycle does.
    ///
    /// Tracing only descends into an `Old` object when it's in
    /// `remembered` (see `should_trace_during_minor`); an `Old` object
    /// reached but not descended into is still presumed alive (never
    /// reclaimed, never has a weak table entry pointing at it cleared) -
    /// only the next major collection re-derives `Old` reachability from
    /// scratch. This is what makes a minor collection cheap: it examines
    /// only the young generation plus whatever `Old` objects the
    /// write-barrier-maintained `remembered` set says might reference it.
    pub fn collect_minor_with_conditional_roots(
        &mut self,
        frame_roots: &[Value],
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
    ) -> Collection {
        self.collect(CollectionKind::Minor, frame_roots, conditional_roots)
    }

    /// Whether a `step_major_with_conditional_roots` cycle is currently
    /// in progress (neither finished nor never started).
    pub fn incremental_cycle_in_progress(&self) -> bool {
        self.incremental.is_some()
    }

    /// Performs at most `work` units of major-collection work, resuming any
    /// cycle already in progress or starting a fresh one seeded from the
    /// given roots. This is `collect`'s own mark/ephemeron/finalizer/sweep
    /// algorithm, split into a phase machine that can pause and resume
    /// across calls instead of always running to completion in one -
    /// `collectgarbage("step", size)`'s collector-side counterpart.
    ///
    /// Returns `(true, collection)` once the whole cycle has completed
    /// (mark, the ephemeron fixed point, the finalizer-reachability round,
    /// and a full sweep of every slot), with `collection` holding that
    /// cycle's totals. Returns `(false, collection)` if more work remains,
    /// with `collection.reclaimed`/`collection.promoted` still accumulating
    /// and `collection.finalizers` always empty (finalizers only become
    /// queued, and only need running, once the cycle actually finishes).
    ///
    /// Every call re-marks every current root before doing any bounded
    /// work, not just the first: raw stack/register roots have no
    /// write-barrier protection (only heap-object-to-heap-object edges
    /// do), so a value newly stored into a root since the last step needs
    /// fresh rescanning to stay visible to the cycle. This rescan only
    /// happens during the marking phase; once sweeping starts, `alloc`
    /// itself protects any object born mid-sweep (see its comment) since
    /// rescanning roots can no longer feed the (already-drained) mark
    /// queue.
    pub fn step_major_with_conditional_roots(
        &mut self,
        work: usize,
        frame_roots: &[Value],
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
    ) -> (bool, Collection) {
        let mut cycle = self.incremental.take().unwrap_or(IncrementalCycle {
            kind: CollectionKind::Major,
            marked: HashSet::new(),
            queue: VecDeque::new(),
            phase: IncrementalPhase::Marking,
            reclaimed: 0,
            promoted: 0,
            finalizers: Vec::new(),
        });

        let mut budget = work.max(1);

        if matches!(cycle.phase, IncrementalPhase::Marking) {
            let roots: Vec<Value> = self.roots.values().copied().collect();
            for root in roots {
                mark_value(root, self, &mut cycle.marked, &mut cycle.queue);
            }
            for root in frame_roots {
                mark_value(*root, self, &mut cycle.marked, &mut cycle.queue);
            }

            while budget > 0 {
                let Some(id) = cycle.queue.pop_front() else {
                    break;
                };
                self.trace_object(id, &mut cycle.marked, &mut cycle.queue, conditional_roots);
                budget -= 1;
            }

            if cycle.queue.is_empty() {
                // The mark queue converging is the same trigger `collect`
                // uses to move on to ephemerons and finalizers; neither
                // sub-pass is itself budgeted, since both are bounded by
                // the (typically small) count of ephemeron tables or
                // finalizer-registered objects, not by total heap size.
                self.mark_ephemerons(&mut cycle.marked, &mut cycle.queue, conditional_roots, false);
                let unreachable_finalizers: Vec<ObjectId> = self
                    .live_ids()
                    .filter(|id| {
                        !cycle.marked.contains(id)
                            && self.entry(*id).is_ok_and(|entry| {
                                entry.header.finalizer == FinalizerState::Registered
                            })
                    })
                    .collect();
                for id in unreachable_finalizers {
                    if let Ok(entry) = self.entry_mut(id) {
                        entry.header.finalizer = FinalizerState::Queued;
                    }
                    cycle.finalizers.push(id);
                    mark_id(id, self, &mut cycle.marked, &mut cycle.queue);
                }
                self.drain_mark_queue(&mut cycle.marked, &mut cycle.queue, conditional_roots, false);
                self.mark_ephemerons(&mut cycle.marked, &mut cycle.queue, conditional_roots, false);
                self.sweep_weak_tables(&mut cycle.marked, false);
                cycle.phase = IncrementalPhase::Sweeping { next_index: 0 };
            }
        }

        if let IncrementalPhase::Sweeping { next_index } = &mut cycle.phase {
            let end = (*next_index + budget).min(self.slots.len());
            for index in *next_index..end {
                let id = ObjectId::new(index, self.slots[index].generation);
                if self.slots[index].entry.is_none() {
                    continue;
                }
                if cycle.marked.contains(&id) {
                    let header = &mut self.slots[index].entry.as_mut().unwrap().header;
                    if header.generation == GcGeneration::Young {
                        header.generation = GcGeneration::Old;
                        cycle.promoted += 1;
                    }
                    continue;
                }
                self.slots[index].entry = None;
                self.slots[index].generation = self.slots[index].generation.wrapping_add(1);
                self.free.push(index);
                cycle.reclaimed += 1;
            }
            *next_index = end;
            if *next_index >= self.slots.len() {
                let live_objects: HashSet<ObjectId> = self.live_ids().collect();
                self.remembered.retain(|id| live_objects.contains(id));
                self.interned_strings
                    .retain(|_, id| live_objects.contains(id));
                return (
                    true,
                    Collection {
                        kind: cycle.kind,
                        reclaimed: cycle.reclaimed,
                        promoted: cycle.promoted,
                        finalizers: cycle.finalizers,
                    },
                );
            }
        }

        let result = Collection {
            kind: cycle.kind,
            reclaimed: cycle.reclaimed,
            promoted: cycle.promoted,
            finalizers: Vec::new(),
        };
        self.incremental = Some(cycle);
        (false, result)
    }

    /// Approximates the currently live, heap-managed byte footprint (for
    /// Lua's `collectgarbage("count")`) by summing every live object's fixed
    /// header plus the allocated capacity of its variable-length storage.
    /// This is a live snapshot recomputed from the current slots on every
    /// call, not a cumulative allocation counter - a temporary object
    /// already reclaimed by a prior collection contributes nothing, matching
    /// real Lua's `count` reporting the collector's current retained set
    /// rather than total bytes ever allocated.
    pub fn live_bytes(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| slot.entry.as_ref())
            .map(|entry| Self::object_byte_footprint(&entry.object))
            .sum()
    }

    fn object_byte_footprint(object: &HeapObject) -> usize {
        match object {
            HeapObject::String(string) => {
                // `size_of::<HeapObject>()` is the whole enum, sized to its
                // largest variant (`TableObject`, currently); charging that
                // as every string's fixed overhead - rather than a string
                // object's own natural size - inflates `collectgarbage
                // ("count")` by the gap between a string and whatever the
                // biggest heap object variant happens to be, and that gap
                // grows every time an unrelated variant gains a field.
                // `StringObject`'s own size is the right fixed cost here,
                // same as every other arm below charging its own struct's
                // size rather than the enum's.
                std::mem::size_of::<StringObject>() + string.bytes.capacity()
            }
            HeapObject::Table(table) => {
                std::mem::size_of::<TableObject>()
                    // `collectgarbage("count")` reports logical retained
                    // table payload. Rust's Vec/IndexMap capacity is an
                    // implementation high-water mark and can survive after
                    // weak sweeping has removed every entry; charging it
                    // makes a dead weak table look like it still owns the
                    // storage of its former keys/values.
                    + table.array.len() * std::mem::size_of::<Value>()
                    + table.hash.len()
                        * (std::mem::size_of::<TableKey>() + std::mem::size_of::<Value>())
            }
            HeapObject::Closure(closure) => {
                std::mem::size_of::<ClosureObject>()
                    + closure.upvalues.len() * std::mem::size_of::<Cell<ObjectId>>()
            }
            HeapObject::NativeCallable(callable) => {
                std::mem::size_of::<NativeCallableObject>()
                    + callable.captures.capacity() * std::mem::size_of::<Value>()
            }
            HeapObject::Upvalue(_) => std::mem::size_of::<UpvalueObject>(),
            HeapObject::Thread(thread) => {
                std::mem::size_of::<ThreadObject>()
                    + thread.stack.capacity() * std::mem::size_of::<Value>()
                    + thread.yielded.capacity() * std::mem::size_of::<Value>()
            }
            HeapObject::Userdata(userdata) => {
                std::mem::size_of::<UserdataObject>()
                    + userdata.bytes.capacity()
                    + userdata.user_values.capacity() * std::mem::size_of::<Value>()
            }
            HeapObject::Error(error) => {
                std::mem::size_of::<ErrorObject>()
                    + error
                        .traceback
                        .iter()
                        .map(|line| line.capacity())
                        .sum::<usize>()
            }
        }
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
        let id = if let Some(slot_index) = self.free.pop() {
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
        };
        // An object born while an incremental cycle is mid-sweep has no
        // chance to be picked up by that cycle's mark phase (already
        // finished) or by the next step's root rescan (sweeping doesn't
        // rescan roots - see `step_major_with_conditional_roots`), so
        // without this it could land in a slot the sweep cursor hasn't
        // reached yet and get freed out from under its only reference.
        // Marking it live immediately is the standard "allocate black
        // during sweep" fix. A birth during the marking phase needs no
        // such protection: that phase re-marks every root on every step,
        // so the object is reachable by the time marking converges as
        // long as it's stored somewhere rooted, exactly like the mutator
        // invariant an ordinary write barrier upholds mid-mark.
        if let Some(cycle) = self.incremental.as_mut() {
            if matches!(cycle.phase, IncrementalPhase::Sweeping { .. }) {
                cycle.marked.insert(id);
            }
        }
        id
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
                    HeapObject::String(string) => {
                        Ok(TableKey::String(string.bytes.clone(), string.hash()))
                    }
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
        // Steele/Dijkstra insertion barrier: if an in-progress incremental
        // mark phase already scanned `owner` (or queued it to be scanned),
        // storing a new pointer into it can hide `child` from the rest of
        // the cycle, since nothing will visit `owner` again to discover it.
        // Re-marking `child` here closes that gap. `owner` not yet marked
        // needs no help - `trace_object` will see this write when it
        // eventually scans `owner`'s current fields. Over-marking an
        // owner that's merely queued (not yet actually scanned) is a
        // harmless, deliberately conservative superset of the strict
        // black-owner rule.
        if let Some(mut cycle) = self.incremental.take() {
            if matches!(cycle.phase, IncrementalPhase::Marking) && cycle.marked.contains(&owner) {
                mark_id(child, self, &mut cycle.marked, &mut cycle.queue);
            }
            self.incremental = Some(cycle);
        }
    }

    fn collect(
        &mut self,
        kind: CollectionKind,
        frame_roots: &[Value],
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
    ) -> Collection {
        // A full, one-shot collection recomputes reachability from
        // scratch and always finishes what it starts, so any
        // `step_major_with_conditional_roots` cycle in progress is
        // superseded rather than merely stale: dropping it here avoids
        // ever resuming a step cycle against a heap a completed full
        // sweep has already reshaped.
        self.incremental = None;
        let minor = matches!(kind, CollectionKind::Minor);
        let mut marked = HashSet::new();
        let mut queue = VecDeque::new();
        let roots: Vec<Value> = self.roots.values().copied().collect();
        for root in roots {
            mark_value(root, self, &mut marked, &mut queue);
        }
        for root in frame_roots {
            mark_value(*root, self, &mut marked, &mut queue);
        }
        if minor {
            // A minor collection never retraces an `Old` object's fields
            // unless it gets dequeued and `should_trace_during_minor` lets
            // it through (remembered-or-young) - but an `Old` object that
            // is itself unreachable from any current root this round (its
            // owning local went out of scope, say) never gets enqueued at
            // all otherwise, so its `remembered` write-barrier edge to a
            // Young child is never walked and that child is (wrongly)
            // swept as garbage even though the `Old` parent is still very
            // much alive - a minor sweep leaves every `Old` object standing
            // regardless of reachability (see the sweep loop below), so the
            // remembered set itself must seed the mark queue here, the same
            // way real Lua's generational GC treats its remembered/"gray
            // again" list as additional roots for a minor cycle.
            let remembered: Vec<ObjectId> = self.remembered.iter().copied().collect();
            for id in remembered {
                mark_id(id, self, &mut marked, &mut queue);
            }
        }
        self.drain_mark_queue(&mut marked, &mut queue, conditional_roots, minor);
        self.mark_ephemerons(&mut marked, &mut queue, conditional_roots, minor);

        let mut result = Collection {
            kind,
            reclaimed: 0,
            promoted: 0,
            finalizers: Vec::new(),
        };
        // A minor collection only ever examines the young generation - see
        // `should_trace_during_minor` - so an `Old` registered-finalizer
        // object left unmarked here (never traced into at all) must not be
        // mistaken for unreachable; only a major collection re-derives
        // `Old` reachability from scratch and can safely finalize it.
        let unreachable_finalizers: Vec<ObjectId> = self
            .live_ids()
            .filter(|id| {
                !marked.contains(id)
                    && (!minor || self.is_young(*id))
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
        self.drain_mark_queue(&mut marked, &mut queue, conditional_roots, minor);
        self.mark_ephemerons(&mut marked, &mut queue, conditional_roots, minor);
        self.sweep_weak_tables(&mut marked, minor);

        for index in 0..self.slots.len() {
            let id = ObjectId::new(index, self.slots[index].generation);
            let Some(generation) = self.slots[index].entry.as_ref().map(|entry| entry.header.generation)
            else {
                continue;
            };
            if minor && generation == GcGeneration::Old {
                // Never evaluated for reachability above (see
                // `should_trace_during_minor`), so never reclaimed here -
                // only the next major collection gets to decide its fate.
                continue;
            }
            if marked.contains(&id) {
                if generation == GcGeneration::Young {
                    self.slots[index].entry.as_mut().unwrap().header.generation = GcGeneration::Old;
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

    fn drain_mark_queue(
        &self,
        marked: &mut HashSet<ObjectId>,
        queue: &mut VecDeque<ObjectId>,
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
        minor: bool,
    ) {
        while let Some(id) = queue.pop_front() {
            // `conditional_roots` entries are proxy roots living in the
            // host's own Rust state (a suspended coroutine's frame
            // contents - see `frame_roots`'s doc comment in
            // `lua_runtime/gc.rs`), not real object-graph edges a
            // write-barriered mutation would ever register `id` into
            // `remembered` for. `trace_object` is the only place that
            // consults them, so `id` must still be traced once it's
            // reached even when the young/remembered generational skip
            // would otherwise apply - an `Old`, unremembered coroutine
            // object stays *reached* the same way a root does, and
            // skipping its trace here would silently drop its pinned
            // frame contents from this collection instead.
            if minor && !conditional_roots.contains_key(&id) && !self.should_trace_during_minor(id)
            {
                continue;
            }
            self.trace_object(id, marked, queue, conditional_roots);
        }
    }

    fn is_young(&self, id: ObjectId) -> bool {
        self.header(id)
            .is_ok_and(|header| header.generation == GcGeneration::Young)
    }

    /// Whether a minor collection should descend into `id`'s own fields to
    /// discover further edges (reached, but not yet dequeued/visited).
    /// `write_barrier` maintains the invariant this relies on: an `Old`
    /// object can only come to point at a `Young` one through a mutation,
    /// and every mutation site adds the mutated owner to `remembered` when
    /// that happens, so an `Old`, *not*-remembered object's own fields are
    /// guaranteed to point only at other `Old` objects, unchanged since the
    /// last time anything traced them - safe to skip re-visiting, which is
    /// what keeps a minor collection cheap (proportional to the young
    /// generation's size plus `remembered`'s, not the whole heap). `Young`
    /// objects (not yet proven stable) and any `remembered` `Old` object
    /// are always traced.
    fn should_trace_during_minor(&self, id: ObjectId) -> bool {
        self.remembered.contains(&id) || self.is_young(id)
    }

    /// Marks every object one gray `id` directly points to - the single
    /// "process one queue entry" step, shared by `drain_mark_queue`'s
    /// unbounded drain and `step_major_with_conditional_roots`'s
    /// budget-bounded incremental drain.
    fn trace_object(
        &self,
        id: ObjectId,
        marked: &mut HashSet<ObjectId>,
        queue: &mut VecDeque<ObjectId>,
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
    ) {
        let Ok(entry) = self.entry(id) else {
            return;
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
                for upvalue in closure.upvalues.iter() {
                    mark_id(upvalue.get(), self, marked, queue);
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
                if let Some(extra) = conditional_roots.get(&id) {
                    for value in extra {
                        mark_value(*value, self, marked, queue);
                    }
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

    fn mark_ephemerons(
        &self,
        marked: &mut HashSet<ObjectId>,
        queue: &mut VecDeque<ObjectId>,
        conditional_roots: &HashMap<ObjectId, Vec<Value>>,
        minor: bool,
    ) {
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
                    // A minor collection never re-derives an `Old` key's
                    // true reachability (see `should_trace_during_minor`),
                    // so it's presumed live here regardless of `marked` -
                    // only the next major collection may decide otherwise.
                    let key_alive = key_is_live(key, marked)
                        || (minor
                            && matches!(key, TableKey::Object(id) if self.header(*id).is_ok_and(|h| h.generation == GcGeneration::Old)));
                    if key_alive {
                        mark_value(*value, self, marked, queue);
                    }
                }
                // Array keys are scalar integers and therefore always live.
                for value in &table.array {
                    mark_value(*value, self, marked, queue);
                }
            }
            self.drain_mark_queue(marked, queue, conditional_roots, minor);
            if marked.len() == before {
                break;
            }
        }
    }

    /// Real Lua never actually drops a string out of a weak-value table
    /// slot, short or long - confirmed against the pinned 5.5 oracle
    /// (`a[1] = string.rep('b', 21); collectgarbage(); assert(a[1])`,
    /// matching `gc.lua`'s own "-- strings are *values*" comment on this
    /// exact case). Every other collectable kind follows ordinary
    /// weak-value semantics: nilled once unreachable. (A string used as a
    /// table *key* needs no equivalent carve-out: `table_key` already
    /// normalizes a string key to an inline `TableKey::String(Vec<u8>)`,
    /// never an `ObjectId`, so it was never subject to identity-based
    /// liveness checking in the first place.)
    ///
    /// A survivor exempted this way was never marked reachable during the
    /// ordinary mark phase (weak-value table slots are never traced as
    /// out-edges - see `trace_object`'s `Table` arm), so it must be added to
    /// `marked` right here, not just spared from being nilled out of the
    /// table: otherwise the caller's own final sweep-by-`marked` pass would
    /// still free its slot out from under the reference this function just
    /// decided to keep.
    fn sweep_weak_tables(&mut self, marked: &mut HashSet<ObjectId>, minor: bool) {
        let string_ids: HashSet<ObjectId> = self
            .live_ids()
            .filter(|id| matches!(self.object(*id), Ok(HeapObject::String(_))))
            .collect();
        // A minor collection never re-derives an `Old` object's true
        // reachability (see `should_trace_during_minor`), so a weak table
        // entry pointing at one must not be nilled out here on the strength
        // of incomplete (young-only) mark info - presume every live `Old`
        // id "survives" a minor sweep instead. Nothing is ever incorrectly
        // reclaimed this way, only left for the next major collection to
        // decide for real.
        let old_ids: Option<HashSet<ObjectId>> =
            minor.then(|| self.live_ids().filter(|id| !self.is_young(*id)).collect());
        for slot in &mut self.slots {
            let Some(Entry {
                object: HeapObject::Table(table),
                ..
            }) = &mut slot.entry
            else {
                continue;
            };
            if table.weak_values {
                let mut nilled_within_border = false;
                for (index, value) in table.array.iter_mut().enumerate() {
                    if !value_survives_weak_value_sweep(*value, &string_ids, marked, old_ids.as_ref())
                    {
                        *value = Value::NIL;
                        nilled_within_border |= index < table.array_border;
                    }
                }
                // Niling a weak-value array slot directly (not through
                // `table_set`) can invalidate the confirmed non-nil prefix;
                // rescan rather than trying to track this incrementally here.
                if nilled_within_border {
                    table.array_border = table
                        .array
                        .iter()
                        .take_while(|value| **value != Value::NIL)
                        .count();
                }
            }
            table.hash.retain(|key, value| {
                let key_alive = !table.weak_keys
                    || key_is_live(key, marked)
                    || old_ids.as_ref().is_some_and(
                        |set| matches!(key, TableKey::Object(id) if set.contains(id)),
                    );
                let value_alive = !table.weak_values
                    || value_survives_weak_value_sweep(*value, &string_ids, marked, old_ids.as_ref());
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

/// Whether `table_set` should place 1-based array-eligible key `key` in the
/// array part given the array's `current_len`, versus the hash part.
///
/// Real Lua's `luaH_newkey`/`computesizes` only ever folds a key into the
/// array part when doing so keeps the array more than half full; matching
/// that exactly isn't needed for correctness here (a positive integer key
/// beyond the array's length is still found via the hash part - see
/// `table_get`/`table_next`), only for avoiding pathological array growth.
/// This uses a simpler doubling bound instead: an append (or a moderate
/// out-of-order write, e.g. into a gap the array will plausibly grow to fill)
/// stays in the array, while a sparse/huge key (e.g. `t[math.maxinteger] =
/// v`, real Lua 5.5 test suite behavior exercised by `attrib.lua`) falls to
/// the hash part rather than resizing the array to fit it.
fn should_grow_array(current_len: usize, key: usize) -> bool {
    key <= current_len.max(4).saturating_mul(2)
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

/// See `sweep_weak_tables`'s doc comment: a string value survives
/// unconditionally (and is marked reachable right here, since it was never
/// traced as an out-edge to begin with); `old_ids` (only `Some` during a
/// minor collection) makes any other live `Old` id survive too, since a
/// minor collection never re-derives `Old` reachability; everything else
/// follows ordinary weak-value liveness.
fn value_survives_weak_value_sweep(
    value: Value,
    string_ids: &HashSet<ObjectId>,
    marked: &mut HashSet<ObjectId>,
    old_ids: Option<&HashSet<ObjectId>>,
) -> bool {
    let Some(id) = value.as_object() else {
        return true;
    };
    if old_ids.is_some_and(|set| set.contains(&id)) {
        return true;
    }
    if marked.contains(&id) {
        return true;
    }
    if string_ids.contains(&id) {
        marked.insert(id);
        return true;
    }
    false
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
    fn string_table_keys_stay_collision_safe_after_hash_caching() {
        // Regression test for the `TableKey::String` hash-caching change:
        // `table_key()` now reads a precomputed `StringObject::hash` instead
        // of hashing fresh bytes every time, and `TableKey`'s manual `Hash`
        // impl writes that cached digest directly. Distinct-content keys
        // must still resolve to distinct table slots, and two different
        // `ObjectId`s with identical bytes (e.g. two long, non-interned
        // strings with the same content) must still collide onto the same
        // logical key, exactly as they did when `Hash` was derived over raw
        // bytes.
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        let alpha = heap.alloc_string(b"alpha");
        let beta = heap.alloc_string(b"beta");
        heap.table_set(table, Value::object(alpha), Value::integer(1))
            .unwrap();
        heap.table_set(table, Value::object(beta), Value::integer(2))
            .unwrap();
        assert_eq!(
            heap.table_get(table, Value::object(alpha)).unwrap(),
            Value::integer(1)
        );
        assert_eq!(
            heap.table_get(table, Value::object(beta)).unwrap(),
            Value::integer(2)
        );

        // Two long (non-interned) strings with identical content get
        // distinct `ObjectId`s but must still collide onto one table key.
        let long_content = "x".repeat(Heap::MAX_SHORT_STRING_LEN + 1);
        let first_long = heap.alloc_string_fresh(long_content.as_bytes());
        let second_long = heap.alloc_string_fresh(long_content.as_bytes());
        assert_ne!(first_long, second_long);
        heap.table_set(table, Value::object(first_long), Value::integer(99))
            .unwrap();
        assert_eq!(
            heap.table_get(table, Value::object(second_long)).unwrap(),
            Value::integer(99)
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
    fn table_array_border_stays_amortized_o1_under_append_and_retraction() {
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        assert_eq!(heap.table_len(table).unwrap(), 0);

        // The common `t[#t + 1] = v` append idiom extends the border by one
        // on every call.
        for index in 1..=5 {
            heap.table_set(table, Value::integer(index), Value::integer(index * 10))
                .unwrap();
            assert_eq!(heap.table_len(table).unwrap(), index as usize);
        }

        // An out-of-order write past the current border doesn't move it yet...
        heap.table_set(table, Value::integer(10), Value::integer(100))
            .unwrap();
        assert_eq!(heap.table_len(table).unwrap(), 5);

        // ...but filling the gap all the way up walks the border forward
        // past the pre-existing out-of-order write in one `table_set` call.
        for index in 6..=9 {
            heap.table_set(table, Value::integer(index), Value::integer(index * 10))
                .unwrap();
        }
        assert_eq!(heap.table_len(table).unwrap(), 10);

        // Nil-ing a slot inside the confirmed prefix retracts the border to
        // just before it, even though later slots are still non-nil.
        heap.table_set(table, Value::integer(3), Value::NIL)
            .unwrap();
        assert_eq!(heap.table_len(table).unwrap(), 2);
    }

    #[test]
    fn table_set_routes_a_sparse_huge_integer_key_to_the_hash_part_instead_of_the_array() {
        // Real Lua 5.5's own test suite does exactly this (`attrib.lua`'s
        // "test of large float/integer indices"): a positive integer key far
        // beyond the array's current length must not force the array to grow
        // to fit it (that used to try to allocate a `Vec` with room for
        // `i64::MAX` elements and crash) - it lives in the hash part instead,
        // with no user-observable difference in `table_get`/`#t`/`next`.
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        heap.table_set(table, Value::integer(1), Value::integer(11))
            .unwrap();
        heap.table_set(table, Value::integer(2), Value::integer(22))
            .unwrap();

        heap.table_set(table, Value::integer(1_000_000_000_000), Value::integer(33))
            .unwrap();
        assert_eq!(heap.table_len(table).unwrap(), 2);
        assert_eq!(
            heap.table_get(table, Value::integer(1_000_000_000_000))
                .unwrap()
                .as_integer(),
            Some(33)
        );
        assert_eq!(
            heap.table_get(table, Value::integer(1)).unwrap().as_integer(),
            Some(11)
        );
        assert_eq!(
            heap.table_get(table, Value::integer(2)).unwrap().as_integer(),
            Some(22)
        );

        // `i64::MAX` (real Lua's `math.maxinteger`): the same key, one past
        // the largest value a `Vec<Value>` index/capacity could represent.
        heap.table_set(table, Value::integer(i64::MAX), Value::integer(44))
            .unwrap();
        assert_eq!(heap.table_len(table).unwrap(), 2);
        assert_eq!(
            heap.table_get(table, Value::integer(i64::MAX))
                .unwrap()
                .as_integer(),
            Some(44)
        );

        // Overwriting a hash-resident huge key stays in the hash part too.
        heap.table_set(table, Value::integer(1_000_000_000_000), Value::integer(55))
            .unwrap();
        assert_eq!(
            heap.table_get(table, Value::integer(1_000_000_000_000))
                .unwrap()
                .as_integer(),
            Some(55)
        );
    }

    #[test]
    fn table_array_border_rescans_after_weak_value_sweep_nils_inside_it() {
        // `sweep_weak_tables` nils dead weak-value array slots directly,
        // bypassing `table_set`'s incremental border maintenance - the
        // border must still reflect the truth afterward.
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        heap.set_table_weak_mode(table, false, true).unwrap();
        let garbage = heap.alloc_table();
        heap.table_set(table, Value::integer(1), Value::integer(1))
            .unwrap();
        heap.table_set(table, Value::integer(2), Value::object(garbage))
            .unwrap();
        heap.table_set(table, Value::integer(3), Value::integer(3))
            .unwrap();
        assert_eq!(heap.table_len(table).unwrap(), 3);

        // Nothing roots `garbage` or the outer table itself; a major
        // collection sweeps the weak-value reference at index 2, which must
        // retract the border to 1 rather than leaving it stale at 3.
        let root = heap.add_root(Value::object(table));
        heap.collect_major();
        assert_eq!(heap.table_len(table).unwrap(), 1);
        heap.remove_root(root);
    }

    #[test]
    fn live_bytes_tracks_current_retained_set_not_cumulative_allocation() {
        let mut heap = Heap::default();
        let baseline = heap.live_bytes();

        let table = heap.alloc_table();
        let root = heap.add_root(Value::object(table));
        heap.table_set(table, Value::integer(1), Value::integer(1))
            .unwrap();
        let with_table = heap.live_bytes();
        assert!(
            with_table > baseline,
            "a live table must contribute to the byte count"
        );

        // An unrooted, unreachable table must not be counted even though it
        // was allocated - `live_bytes` reports the retained set, not
        // cumulative allocation.
        heap.alloc_table();
        heap.collect_major();
        let after_garbage_collected = heap.live_bytes();
        assert_eq!(
            after_garbage_collected, with_table,
            "a swept, unreachable table must not inflate the live count"
        );

        heap.remove_root(root);
        heap.collect_major();
        assert_eq!(
            heap.live_bytes(),
            baseline,
            "dropping the last root and collecting must return to baseline"
        );
    }

    #[test]
    fn table_next_walks_array_then_hash_in_order_and_terminates() {
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        heap.table_set(table, Value::integer(1), Value::integer(10))
            .unwrap();
        heap.table_set(table, Value::integer(2), Value::integer(20))
            .unwrap();
        let name_key = Value::object(heap.alloc_string(b"name"));
        heap.table_set(table, name_key, Value::integer(30)).unwrap();

        let (key, value) = heap.table_next(table, Value::NIL).unwrap().unwrap();
        assert_eq!((key, value), (Value::integer(1), Value::integer(10)));

        let (key, value) = heap.table_next(table, key).unwrap().unwrap();
        assert_eq!((key, value), (Value::integer(2), Value::integer(20)));

        let (key, value) = heap.table_next(table, key).unwrap().unwrap();
        assert_eq!((key, value), (name_key, Value::integer(30)));

        assert_eq!(heap.table_next(table, key).unwrap(), None);
    }

    #[test]
    fn table_next_skips_a_key_nilled_since_it_was_last_returned() {
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        heap.table_set(table, Value::integer(1), Value::integer(10))
            .unwrap();
        heap.table_set(table, Value::integer(2), Value::integer(20))
            .unwrap();
        heap.table_set(table, Value::integer(3), Value::integer(30))
            .unwrap();

        let (key, _) = heap.table_next(table, Value::NIL).unwrap().unwrap();
        assert_eq!(key, Value::integer(1));
        // Real Lua permits clearing the just-visited key mid-traversal;
        // `next` must still resume correctly from it.
        heap.table_set(table, key, Value::NIL).unwrap();
        let (key, value) = heap.table_next(table, key).unwrap().unwrap();
        assert_eq!((key, value), (Value::integer(2), Value::integer(20)));
    }

    #[test]
    fn table_next_rejects_a_key_never_stored_in_the_table() {
        let mut heap = Heap::default();
        let table = heap.alloc_table();
        heap.table_set(table, Value::integer(1), Value::integer(10))
            .unwrap();
        assert_eq!(
            heap.table_next(table, Value::integer(99)).unwrap_err(),
            HeapError::InvalidNextKey
        );
    }

    #[test]
    fn upvalue_value_reads_back_what_set_upvalue_wrote() {
        let mut heap = Heap::default();
        let cell = heap.alloc_upvalue(Value::integer(1), None);
        assert_eq!(heap.upvalue_value(cell).unwrap(), Value::integer(1));
        heap.set_upvalue(cell, Value::integer(2)).unwrap();
        assert_eq!(heap.upvalue_value(cell).unwrap(), Value::integer(2));

        let table = heap.alloc_table();
        assert_eq!(
            heap.upvalue_value(table).unwrap_err(),
            HeapError::WrongKind {
                handle: table,
                expected: ObjectKind::Upvalue,
                actual: ObjectKind::Table,
            }
        );
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
    fn conditional_thread_roots_only_apply_once_the_thread_is_independently_reachable() {
        let mut heap = Heap::default();
        let thread = heap.alloc_thread(vec![]);
        let extra = heap.alloc_table();
        let mut conditional_roots = HashMap::new();
        conditional_roots.insert(thread, vec![Value::object(extra)]);

        // Nothing roots `thread` itself: `extra` must not be kept alive just
        // because it's in the conditional-roots map.
        let collection = heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(!heap.contains(thread));
        assert!(!heap.contains(extra));
        assert_eq!(collection.reclaimed, 2);

        // Re-allocate and this time root the thread directly (e.g. a local
        // variable holding this coroutine): its conditional edge must now
        // fire and keep `extra` alive too.
        let thread = heap.alloc_thread(vec![]);
        let extra = heap.alloc_table();
        let mut conditional_roots = HashMap::new();
        conditional_roots.insert(thread, vec![Value::object(extra)]);
        let root = heap.add_root(Value::object(thread));

        heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(heap.contains(thread));
        assert!(heap.contains(extra));

        heap.remove_root(root);
        assert_eq!(heap.collect_major().reclaimed, 2);
    }

    #[test]
    fn a_cycle_of_conditionally_rooted_threads_collects_together_when_unreachable() {
        let mut heap = Heap::default();
        let thread_a = heap.alloc_thread(vec![]);
        let thread_b = heap.alloc_thread(vec![]);
        let mut conditional_roots = HashMap::new();
        // Each thread's "frame contents" reference the other, mirroring two
        // suspended coroutines whose only remaining live locals point back
        // at each other - the exact self-cycle-through-a-coroutine leak this
        // mechanism exists to close (see
        // docs/features/table-closure-coroutine-cutover.md §6/§11).
        conditional_roots.insert(thread_a, vec![Value::object(thread_b)]);
        conditional_roots.insert(thread_b, vec![Value::object(thread_a)]);

        let collection = heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(!heap.contains(thread_a));
        assert!(!heap.contains(thread_b));
        assert_eq!(collection.reclaimed, 2);

        // Same cycle, but this time thread_a is also independently
        // reachable (e.g. a surviving Lua local still holds it): both must
        // now survive, since thread_b is reachable transitively through
        // thread_a's conditional edge.
        let thread_a = heap.alloc_thread(vec![]);
        let thread_b = heap.alloc_thread(vec![]);
        let mut conditional_roots = HashMap::new();
        conditional_roots.insert(thread_a, vec![Value::object(thread_b)]);
        conditional_roots.insert(thread_b, vec![Value::object(thread_a)]);
        let root = heap.add_root(Value::object(thread_a));

        heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(heap.contains(thread_a));
        assert!(heap.contains(thread_b));

        heap.remove_root(root);
        assert_eq!(heap.collect_major().reclaimed, 2);
    }

    #[test]
    fn conditional_thread_roots_and_ephemeron_marking_interact_correctly() {
        // A Thread reachable only via an ephemeron table's *value* (so it
        // only becomes marked partway through `mark_ephemerons`'s fixed
        // point, not in the initial `drain_mark_queue` pass) must still have
        // its own conditional roots applied - proving the two mechanisms,
        // sharing one `queue`/`marked` pair, compose without a dedicated
        // extra round-trip.
        let mut heap = Heap::default();
        let ephemeron = heap.alloc_table();
        heap.set_table_weak_mode(ephemeron, true, false).unwrap();
        let ephemeron_root = heap.add_root(Value::object(ephemeron));
        let key = heap.alloc_table();
        let key_root = heap.add_root(Value::object(key));
        let thread = heap.alloc_thread(vec![]);
        heap.table_set(ephemeron, Value::object(key), Value::object(thread))
            .unwrap();
        let extra = heap.alloc_table();
        let mut conditional_roots = HashMap::new();
        conditional_roots.insert(thread, vec![Value::object(extra)]);

        heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(heap.contains(thread));
        assert!(heap.contains(extra));

        heap.remove_root(key_root);
        heap.collect_major_with_conditional_roots(&[], &conditional_roots);
        assert!(!heap.contains(thread));
        assert!(!heap.contains(extra));
        heap.remove_root(ephemeron_root);
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

    #[test]
    fn step_major_with_conditional_roots_needs_fewer_calls_with_a_bigger_budget() {
        fn steps_to_finish(work: usize, garbage_count: usize) -> (usize, Collection) {
            let mut heap = Heap::default();
            let root_table = heap.alloc_table();
            let root = heap.add_root(Value::object(root_table));
            for _ in 0..garbage_count {
                heap.alloc_table();
            }
            let mut calls = 0;
            loop {
                calls += 1;
                let (finished, collection) =
                    heap.step_major_with_conditional_roots(work, &[], &HashMap::new());
                if finished {
                    heap.remove_root(root);
                    return (calls, collection);
                }
            }
        }

        // Mirrors `gc.lua`'s own `dosteps` acceptance test: a smaller
        // per-call budget must take strictly more calls to finish the same
        // cycle than a larger one, and either way the unrooted garbage is
        // fully reclaimed once the cycle actually completes.
        let (small_budget_calls, small_collection) = steps_to_finish(2, 40);
        let (large_budget_calls, large_collection) = steps_to_finish(1000, 40);
        assert!(small_budget_calls > large_budget_calls);
        assert_eq!(large_budget_calls, 1);
        assert_eq!(small_collection.reclaimed, 40);
        assert_eq!(large_collection.reclaimed, 40);
    }

    #[test]
    fn step_major_with_conditional_roots_write_barrier_protects_a_late_root_mutation() {
        let mut heap = Heap::default();
        let root_table = heap.alloc_table();
        let root = heap.add_root(Value::object(root_table));

        // A chain long enough that a `work: 1` budget needs several calls
        // to drain, so `root_table` gets dequeued and fully traced (goes
        // "black") well before the cycle as a whole converges.
        let mut previous = root_table;
        for _ in 0..10 {
            let next = heap.alloc_table();
            heap.table_set(previous, Value::integer(1), Value::object(next))
                .unwrap();
            previous = next;
        }

        // Trace `root_table` itself (and only it) this call.
        let (finished, _) = heap.step_major_with_conditional_roots(1, &[], &HashMap::new());
        assert!(!finished);

        // With `root_table` already traced, point it at a brand new object
        // the rest of the (now-detached) chain has no way to discover.
        // Without the insertion barrier this would be silently swept once
        // the cycle finishes, even though a live root points straight at
        // it right now.
        let late_child = heap.alloc_table();
        heap.table_set(root_table, Value::integer(1), Value::object(late_child))
            .unwrap();

        loop {
            let (finished, _) = heap.step_major_with_conditional_roots(1, &[], &HashMap::new());
            if finished {
                break;
            }
        }

        assert!(heap.contains(late_child));
        heap.remove_root(root);
    }
}
