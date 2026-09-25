//! Core value representation for the dynamic `.lua` runtime: `LuaValue`
//! and the types it is built from (tables, closures, native bridges, keys,
//! errors, global scopes). See `lua_runtime` module docs for the split
//! rationale.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;

use super::*;

/// Shorthand for the runtime's pervasive shared-mutable-cell pattern:
/// reference-counted, aliasable ownership over a heap-allocated `T` (tables,
/// closure upvalue cells, `gmatch` iterator state, ...).
pub(super) type RcRef<T> = Rc<RefCell<T>>;

/// A closure's upvalue-cell storage: one slot per captured variable, `None`
/// where a slot has been recycled from the frame pool but not yet initialized
/// for the current call. Each `Some` is a canonical `sol_core::Heap`-resident
/// `UpvalueObject`'s id, shared by `Copy` between a frame's `cells[reg]` and
/// any closure's `ClosureObject.upvalues` - see `sol_core::Heap::alloc_upvalue`/
/// `upvalue_value`/`set_upvalue`.
pub(super) type Cells = Vec<Option<sol_core::ObjectId>>;

/// A precisely rooted handle to an object owned by the canonical `sol-core`
/// heap. Legacy frames and tables may clone this small guard while migration
/// is in progress, but they never own or duplicate the managed object itself.
#[derive(Clone)]
pub struct CanonicalUserdata(Rc<CanonicalObjectRoot>);

#[derive(Clone)]
pub struct CanonicalTable(Rc<CanonicalObjectRoot>);

/// A precisely rooted handle to a byte string interned in the canonical
/// heap. Strings are immutable once allocated and hold no references to
/// other values, so unlike tables/closures/coroutines they migrate onto
/// `sol-core` independently of the rest of the value graph.
#[derive(Clone)]
pub struct CanonicalString(Rc<CanonicalObjectRoot>);

#[derive(Clone)]
pub struct CanonicalCFunction {
    root: Rc<CanonicalObjectRoot>,
}

struct CanonicalObjectRoot {
    heap: RcRef<sol_core::Heap>,
    object: sol_core::ObjectId,
    root: sol_core::RootId,
}

impl Drop for CanonicalObjectRoot {
    fn drop(&mut self) {
        self.heap.borrow_mut().remove_root(self.root);
    }
}

/// A cheap `Copy` handle into a canonical `sol-core` table object, distinct
/// from the `Rc<CanonicalObjectRoot>`-rooted `CanonicalTable` above: it holds
/// no root and has no `Drop` side effect, so it is safe to store at hot-path
/// volume (registers, table cells, upvalues) once frame-walk rooting (see
/// docs/features/table-closure-coroutine-cutover.md §3) makes every live one
/// reachable from a GC safepoint. `CanonicalTable` stays reserved for
/// genuinely long-lived anchors (the embedding registry, `lua_ref`-style
/// persistent C handles). Backs `LuaValue::Table` as of the
/// coordinated flip in docs/features/table-closure-coroutine-cutover.md §8
/// step 4.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TableRef(sol_core::ObjectId);

/// The `ClosureRef` counterpart to `TableRef`; see its doc comment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClosureRef(sol_core::ObjectId);

/// The `ThreadRef` counterpart to `TableRef`; see its doc comment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ThreadRef(sol_core::ObjectId);

impl TableRef {
    pub(super) fn new(object: sol_core::ObjectId) -> Self {
        Self(object)
    }

    pub(super) fn object_id(self) -> sol_core::ObjectId {
        self.0
    }

    pub(super) fn alloc(heap: &mut sol_core::Heap) -> Self {
        Self(heap.alloc_table())
    }

    pub(super) fn get(self, heap: &sol_core::Heap, key: sol_core::Value) -> sol_core::Value {
        // `NilTableKey`/`NanTableKey` are expected outcomes of a *read* (Lua
        // returns `nil` for `t[nil]`/`t[0/0]`) - only a *write* with such a
        // key raises a catchable Lua error, via `table_set`'s own
        // `HeapError` -> `LuaError` conversion. Any other `HeapError` here
        // means a genuinely dead/mistyped handle, which is an internal bug.
        match heap.table_get(self.0, key) {
            Ok(value) => value,
            Err(sol_core::HeapError::NilTableKey | sol_core::HeapError::NanTableKey) => {
                sol_core::Value::NIL
            }
            Err(_) => panic!("TableRef must address a live table object"),
        }
    }

    pub(super) fn set(
        self,
        heap: &mut sol_core::Heap,
        key: sol_core::Value,
        value: sol_core::Value,
    ) -> Result<(), sol_core::HeapError> {
        heap.table_set(self.0, key, value)
    }

    pub(super) fn len(self, heap: &sol_core::Heap) -> usize {
        heap.table_len(self.0)
            .expect("TableRef must address a live table object")
    }

    pub(super) fn next(
        self,
        heap: &mut sol_core::Heap,
        key: sol_core::Value,
    ) -> Result<Option<(sol_core::Value, sol_core::Value)>, sol_core::HeapError> {
        heap.table_next(self.0, key)
    }
}

impl ClosureRef {
    pub(super) fn new(object: sol_core::ObjectId) -> Self {
        Self(object)
    }

    pub(super) fn object_id(self) -> sol_core::ObjectId {
        self.0
    }

    /// Allocates a closure over already-allocated upvalue cells (see
    /// `sol_core::Heap::alloc_upvalue`/`upvalue_value`/`set_upvalue` for
    /// creating and reading/writing those cells).
    pub(super) fn alloc(
        heap: &mut sol_core::Heap,
        prototype: u32,
        upvalues: Vec<sol_core::ObjectId>,
        environment: usize,
    ) -> Result<Self, sol_core::HeapError> {
        Ok(Self(heap.alloc_closure(prototype, upvalues, environment)?))
    }
}

impl ThreadRef {
    pub(super) fn new(object: sol_core::ObjectId) -> Self {
        Self(object)
    }

    pub(super) fn object_id(self) -> sol_core::ObjectId {
        self.0
    }

    pub(super) fn alloc(heap: &mut sol_core::Heap) -> Self {
        Self(heap.alloc_thread(Vec::new()))
    }
}

/// A cheap `Copy` handle to a `string.gmatch` iterator's state, stored as a
/// `HeapObject::NativeCallable` under the shared `canonical::LEGACY_STATE_PROVIDER`
/// namespace (see `table.rs`'s `gmatch_alloc`/`gmatch_read`/`gmatch_advance`)
/// rather than as its own `HeapObject` kind - see
/// docs/features/table-closure-coroutine-cutover.md §2. Never memoized: each
/// `gmatch()` call is independently mutable even over identical text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GMatchRef(sol_core::ObjectId);

impl GMatchRef {
    pub(super) fn new(object: sol_core::ObjectId) -> Self {
        Self(object)
    }

    pub(super) fn object_id(self) -> sol_core::ObjectId {
        self.0
    }
}

impl fmt::Debug for CanonicalUserdata {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_tuple("CanonicalUserdata")
            .field(&self.0.object)
            .finish()
    }
}

impl fmt::Debug for CanonicalTable {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_tuple("CanonicalTable")
            .field(&self.0.object)
            .finish()
    }
}

impl fmt::Debug for CanonicalString {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_tuple("CanonicalString")
            .field(&String::from_utf8_lossy(self.as_bytes()))
            .finish()
    }
}

impl fmt::Debug for CanonicalCFunction {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_tuple("CanonicalCFunction")
            .field(&self.root.object)
            .finish()
    }
}

impl CanonicalUserdata {
    pub(super) fn allocate_with_uservalues(
        heap: RcRef<sol_core::Heap>,
        size: usize,
        user_values: usize,
    ) -> Self {
        let (object, root) = {
            let mut heap_ref = heap.borrow_mut();
            let object = heap_ref.alloc_userdata_bytes_with_uservalues(size, user_values);
            let root = heap_ref.add_root(sol_core::Value::object(object));
            (object, root)
        };
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub(super) fn root_existing(heap: RcRef<sol_core::Heap>, object: sol_core::ObjectId) -> Self {
        let root = heap.borrow_mut().add_root(sol_core::Value::object(object));
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub fn object_id(&self) -> sol_core::ObjectId {
        self.0.object
    }

    pub fn len(&self) -> usize {
        self.0
            .heap
            .borrow()
            .userdata(self.0.object)
            .expect("rooted userdata must remain live")
            .bytes
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn bytes_ptr(&self) -> *mut std::ffi::c_void {
        self.0
            .heap
            .borrow_mut()
            .userdata_mut(self.0.object)
            .expect("rooted userdata must remain live")
            .bytes
            .as_mut_ptr()
            .cast()
    }
}

impl CanonicalTable {
    pub(super) fn allocate(heap: RcRef<sol_core::Heap>) -> Self {
        let (object, root) = {
            let mut heap_ref = heap.borrow_mut();
            let object = heap_ref.alloc_table();
            let root = heap_ref.add_root(sol_core::Value::object(object));
            (object, root)
        };
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub(super) fn root_existing(heap: RcRef<sol_core::Heap>, object: sol_core::ObjectId) -> Self {
        let root = heap.borrow_mut().add_root(sol_core::Value::object(object));
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub fn object_id(&self) -> sol_core::ObjectId {
        self.0.object
    }

    pub fn len(&self) -> usize {
        match self.0.heap.borrow().object(self.0.object) {
            Ok(sol_core::HeapObject::Table(table)) => table.array.len() + table.hash.len(),
            _ => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CanonicalString {
    /// Interns `bytes` in the canonical heap, deduplicating identical
    /// content into the same `ObjectId` (matching real Lua's short-string
    /// interning, but applied uniformly since the heap tracks all strings
    /// by content already).
    pub(super) fn intern(heap: RcRef<sol_core::Heap>, bytes: impl AsRef<[u8]>) -> Self {
        let (object, root) = {
            let mut heap_ref = heap.borrow_mut();
            let object = heap_ref.alloc_string(bytes);
            let root = heap_ref.add_root(sol_core::Value::object(object));
            (object, root)
        };
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    /// Allocates a new string without deduplicating by content, so it never
    /// aliases the identity of an existing equal-content string (real Lua
    /// only interns short strings; a runtime-computed value must not alias a
    /// pre-existing string just because the bytes match). Use this for every
    /// string constructed at run time (concatenation, string-library
    /// results, formatted output, ...); reserve `intern` for compile-time
    /// literal constants and fixed structural labels.
    pub(super) fn fresh(heap: RcRef<sol_core::Heap>, bytes: impl AsRef<[u8]>) -> Self {
        let (object, root) = {
            let mut heap_ref = heap.borrow_mut();
            let object = heap_ref.alloc_string_fresh(bytes);
            let root = heap_ref.add_root(sol_core::Value::object(object));
            (object, root)
        };
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub(super) fn root_existing(heap: RcRef<sol_core::Heap>, object: sol_core::ObjectId) -> Self {
        let root = heap.borrow_mut().add_root(sol_core::Value::object(object));
        Self(Rc::new(CanonicalObjectRoot { heap, object, root }))
    }

    pub fn object_id(&self) -> sol_core::ObjectId {
        self.0.object
    }

    pub fn as_bytes(&self) -> &[u8] {
        let heap = self.0.heap.borrow();
        let sol_core::HeapObject::String(bytes) = heap
            .object(self.0.object)
            .expect("rooted string must remain live")
        else {
            unreachable!("canonical string has the wrong object kind")
        };
        // SAFETY: `bytes`'s heap-allocated buffer address is independent of
        // the owning slab entry moving, and `self.0`'s root keeps the string
        // live for as long as this borrowed slice can be observed - the same
        // stable-pointer-into-owned-bytes precedent as
        // `CanonicalUserdata::bytes_ptr`.
        unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) }
    }

    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.as_bytes().as_ptr()
    }
}

impl PartialEq for CanonicalString {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for CanonicalString {}

impl std::hash::Hash for CanonicalString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl PartialOrd for CanonicalString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.as_bytes().cmp(other.as_bytes()))
    }
}

impl Ord for CanonicalString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl AsRef<[u8]> for CanonicalString {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl CanonicalCFunction {
    pub(super) fn allocate(
        heap: RcRef<sol_core::Heap>,
        callable: sol_core::NativeCallableId,
        captures: Vec<sol_core::Value>,
    ) -> Self {
        let (object, root) = {
            let mut heap_ref = heap.borrow_mut();
            let object =
                heap_ref.alloc_native_callable(callable.provider, callable.function, captures);
            let root = heap_ref.add_root(sol_core::Value::object(object));
            (object, root)
        };
        Self {
            root: Rc::new(CanonicalObjectRoot { heap, object, root }),
        }
    }

    pub(super) fn root_existing(heap: RcRef<sol_core::Heap>, object: sol_core::ObjectId) -> Self {
        let root = heap.borrow_mut().add_root(sol_core::Value::object(object));
        Self {
            root: Rc::new(CanonicalObjectRoot { heap, object, root }),
        }
    }

    pub fn object_id(&self) -> sol_core::ObjectId {
        self.root.object
    }
    pub fn callable_id(&self) -> sol_core::NativeCallableId {
        let heap = self.root.heap.borrow();
        let sol_core::HeapObject::NativeCallable(callable) = heap
            .object(self.root.object)
            .expect("rooted C function must remain live")
        else {
            unreachable!("canonical C function has the wrong object kind")
        };
        sol_core::NativeCallableId::new(callable.provider, callable.function)
    }
}

#[derive(Clone, Debug)]
pub enum LuaValue {
    Nil,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(CanonicalString),
    Table(TableRef),
    /// Canonical table used by the embedding registry and userdata metatables
    /// while legacy interpreter tables are migrated incrementally.
    CanonicalTable(CanonicalTable),
    Closure(ClosureRef),
    NativeFunction(NativeFunction),
    /// A Cranelift-compiled Sol function, callable from dynamic Lua code - see
    /// `docs/features/lua-compatibility.md`'s per-function typed/dynamic split.
    /// One-directional: native code never holds one of these back - it is an
    /// external registration descriptor and is converted to `RegisteredNative`
    /// before installation in the runtime object graph.
    Native(Rc<NativeBridge>),
    /// Pointer-free identity of a native callable installed in this runtime.
    /// The host pointer and signature live only in `LuaRuntime`'s side registry
    /// and therefore cannot leak into tables, frames, or snapshots.
    RegisteredNative(sol_core::NativeCallableId),
    /// Lua C API callback. Only its portable registry identity enters the value
    /// graph; the process pointer remains in the runtime side table.
    CFunction(CanonicalCFunction),
    /// Stateful iterator returned by `string.gmatch`; carries its own position
    /// so repeated calls advance through the subject string.
    GMatchIterator(GMatchRef),
    /// A coroutine created by `coroutine.create`. See `LuaCoroutine`.
    Thread(ThreadRef),
    /// The callable wrapper `coroutine.wrap` returns: calling it resumes the
    /// underlying coroutine directly, propagating an error raised inside the
    /// coroutine as a real Lua error instead of `coroutine.resume`'s
    /// `(false, message)` pair - matching real Lua's `coroutine.wrap`.
    CoroutineWrapper(ThreadRef),
    /// Opaque host value; no host capabilities are exposed by default.
    Userdata(CanonicalUserdata),
    /// A `debug.upvalueid`-style opaque identity: `type()` reports "userdata"
    /// and it compares equal only to another `LightUserdata` wrapping the same
    /// value, but it carries no capabilities and cannot be dereferenced from
    /// Lua. Currently only produced by `debug.upvalueid`, to expose whether two
    /// closures share the same upvalue storage cell.
    LightUserdata(usize),
}

/// Scalar types that can cross the dynamic/native boundary. Deliberately a
/// small closed set, not `crate::types::Type` - the bridge only ever needs to
/// know how to bit-pack/unpack a `u64` register slot, never anything about the
/// GC-managed representations (`String`/`Array`/`Any`/...) that live on the
/// typed side.
pub type BridgeScalar = sol_core::ScalarKind;

/// A native Sol function's uniform-ABI wrapper pointer
/// (`extern "C" fn(*const u64, i64) -> u64`, see `codegen.rs::compile_wrapper`)
/// plus enough signature info to marshal `LuaValue` args/results across it.
pub struct NativeBridge {
    pub name: String,
    pub ptr: *const u8,
    pub params: Vec<BridgeScalar>,
    pub ret: BridgeScalar,
}

impl fmt::Debug for NativeBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NativeBridge({})", self.name)
    }
}

/// Marshals dynamic Lua values across the one-directional bridge into a
/// native Sol function's uniform-ABI wrapper (`extern "C" fn(*const u64, i64)
/// -> u64`, see `codegen.rs::compile_wrapper` / `interp.rs::call_native`) and
/// decodes its scalar result back into a `LuaValue`.
pub(super) fn call_native_bridge(
    bridge: &NativeBridge,
    args: Vec<LuaValue>,
) -> LuaResult<Vec<LuaValue>> {
    if args.len() != bridge.params.len() {
        return Err(LuaError::new(format!(
            "'{}' expects {} argument(s), got {}",
            bridge.name,
            bridge.params.len(),
            args.len()
        )));
    }
    let mut packed = Vec::with_capacity(args.len());
    for (index, (arg, param)) in args.iter().zip(&bridge.params).enumerate() {
        let bits = match (param, arg) {
            (BridgeScalar::I64, LuaValue::Integer(value)) => *value as u64,
            (BridgeScalar::F64, LuaValue::Float(value)) => value.to_bits(),
            (BridgeScalar::F64, LuaValue::Integer(value)) => (*value as f64).to_bits(),
            (BridgeScalar::Bool, LuaValue::Bool(value)) => *value as u64,
            _ => {
                return Err(LuaError::new(format!(
                    "bad argument #{} to '{}' ({:?} expected, got {})",
                    index + 1,
                    bridge.name,
                    param,
                    arg.type_name()
                )))
            }
        };
        packed.push(bits);
    }
    let f: extern "C" fn(*const u64, i64) -> u64 = unsafe { std::mem::transmute(bridge.ptr) };
    let raw = f(packed.as_ptr(), packed.len() as i64);
    let result = match bridge.ret {
        BridgeScalar::I64 => LuaValue::Integer(raw as i64),
        BridgeScalar::F64 => LuaValue::Float(f64::from_bits(raw)),
        BridgeScalar::Bool => LuaValue::Bool(raw != 0),
    };
    Ok(vec![result])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum NativeFunction {
    Print,
    Assert,
    Type,
    ToString,
    ToNumber,
    StringLen,
    StringByte,
    StringChar,
    StringSub,
    StringLower,
    StringUpper,
    StringReverse,
    StringRep,
    StringDump,
    RawGet,
    RawSet,
    RawEqual,
    RawLen,
    GetMetatable,
    SetMetatable,
    Error,
    PCall,
    XCall,
    Select,
    Next,
    Pairs,
    IPairs,
    IPairsIterator,
    TableConcat,
    TableInsert,
    TableRemove,
    TablePack,
    TableUnpack,
    TableSort,
    TableMove,
    MathAbs,
    MathFloor,
    MathCeil,
    MathMin,
    MathMax,
    MathToInteger,
    MathType,
    MathSqrt,
    MathSin,
    MathCos,
    MathTan,
    MathExp,
    MathLog,
    MathAcos,
    MathAsin,
    MathAtan,
    MathDeg,
    MathRad,
    MathFmod,
    MathModf,
    MathUlt,
    MathFrexp,
    MathLdexp,
    MathRandom,
    MathRandomSeed,
    Utf8Len,
    Utf8Char,
    Utf8Codepoint,
    Utf8Offset,
    Utf8Codes,
    Utf8IteratorStrict,
    Utf8IteratorLax,
    Require,
    PackageSearchPath,
    PackageSearcherPreload,
    PackageSearcherLua,
    PackageSearcherC,
    PackageSearcherCRoot,
    StringFind,
    StringMatch,
    StringGMatch,
    StringGSub,
    StringFormat,
    TableCreate,
    CollectGarbage,
    OsTime,
    OsClock,
    OsDifftime,
    OsDate,
    OsGetenv,
    OsExit,
    IoWrite,
    IoRead,
    IoInput,
    FileWrite,
    IoOutput,
    FileClose,
    FileGc,
    OsRemove,
    OsSetlocale,
    OsTmpname,
    PackageLoadLib,
    Load,
    DoFile,
    StringPack,
    StringUnpack,
    StringPackSize,
    CoroutineCreate,
    CoroutineResume,
    CoroutineYield,
    CoroutineStatus,
    CoroutineWrap,
    CoroutineRunning,
    CoroutineIsYieldable,
    CoroutineClose,
    DebugGetupvalue,
    DebugUpvalueid,
    DebugUpvaluejoin,
    DebugSetupvalue,
    DebugGetinfo,
    DebugGetmetatable,
    DebugSetmetatable,
    DebugTraceback,
    DebugSethook,
    DebugGethook,
    DebugSetuservalue,
}

impl NativeFunction {
    /// The short, unqualified name this function is registered under in
    /// `init.rs` (e.g. `"insert"` for `table.insert`, not `"table.insert"`) -
    /// matches how real Lua's own argument-check errors name the callee
    /// (`bad argument #1 to 'insert' (...)`), which reports the name the
    /// value was looked up by rather than a fully qualified path. Used only
    /// to build those messages; keep in sync with `init.rs`'s registrations.
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Assert => "assert",
            Self::Type => "type",
            Self::ToString => "tostring",
            Self::ToNumber => "tonumber",
            Self::StringLen => "len",
            Self::StringByte => "byte",
            Self::StringChar => "char",
            Self::StringSub => "sub",
            Self::StringLower => "lower",
            Self::StringUpper => "upper",
            Self::StringReverse => "reverse",
            Self::StringRep => "rep",
            Self::StringDump => "dump",
            Self::RawGet => "rawget",
            Self::RawSet => "rawset",
            Self::RawEqual => "rawequal",
            Self::RawLen => "rawlen",
            Self::GetMetatable => "getmetatable",
            Self::SetMetatable => "setmetatable",
            Self::Error => "error",
            Self::PCall => "pcall",
            Self::XCall => "xpcall",
            Self::Select => "select",
            Self::Next => "next",
            Self::Pairs => "pairs",
            Self::IPairs => "ipairs",
            Self::IPairsIterator => "ipairs",
            Self::TableConcat => "concat",
            Self::TableInsert => "insert",
            Self::TableRemove => "remove",
            Self::TablePack => "pack",
            Self::TableUnpack => "unpack",
            Self::TableSort => "sort",
            Self::TableMove => "move",
            Self::MathAbs => "abs",
            Self::MathFloor => "floor",
            Self::MathCeil => "ceil",
            Self::MathMin => "min",
            Self::MathMax => "max",
            Self::MathToInteger => "tointeger",
            Self::MathType => "type",
            Self::MathSqrt => "sqrt",
            Self::MathSin => "sin",
            Self::MathCos => "cos",
            Self::MathTan => "tan",
            Self::MathExp => "exp",
            Self::MathLog => "log",
            Self::MathAcos => "acos",
            Self::MathAsin => "asin",
            Self::MathAtan => "atan",
            Self::MathDeg => "deg",
            Self::MathRad => "rad",
            Self::MathFmod => "fmod",
            Self::MathModf => "modf",
            Self::MathUlt => "ult",
            Self::MathFrexp => "frexp",
            Self::MathLdexp => "ldexp",
            Self::MathRandom => "random",
            Self::MathRandomSeed => "randomseed",
            Self::Utf8Len => "len",
            Self::Utf8Char => "char",
            Self::Utf8Codepoint => "codepoint",
            Self::Utf8Offset => "offset",
            Self::Utf8Codes => "codes",
            Self::Utf8IteratorStrict | Self::Utf8IteratorLax => "codes",
            Self::Require => "require",
            Self::PackageSearchPath => "searchpath",
            Self::PackageSearcherPreload
            | Self::PackageSearcherLua
            | Self::PackageSearcherC
            | Self::PackageSearcherCRoot => "searcher",
            Self::StringFind => "find",
            Self::StringMatch => "match",
            Self::StringGMatch => "gmatch",
            Self::StringGSub => "gsub",
            Self::StringFormat => "format",
            Self::TableCreate => "create",
            Self::CollectGarbage => "collectgarbage",
            Self::OsTime => "time",
            Self::OsClock => "clock",
            Self::OsDifftime => "difftime",
            Self::OsDate => "date",
            Self::OsGetenv => "getenv",
            Self::OsExit => "exit",
            Self::IoWrite => "write",
            Self::IoRead => "read",
            Self::IoInput => "input",
            Self::FileWrite => "write",
            Self::IoOutput => "output",
            Self::FileClose => "close",
            Self::FileGc => "__gc",
            Self::OsRemove => "remove",
            Self::OsSetlocale => "setlocale",
            Self::OsTmpname => "tmpname",
            Self::PackageLoadLib => "loadlib",
            Self::Load => "load",
            Self::DoFile => "dofile",
            Self::StringPack => "pack",
            Self::StringUnpack => "unpack",
            Self::StringPackSize => "packsize",
            Self::CoroutineCreate => "create",
            Self::CoroutineResume => "resume",
            Self::CoroutineYield => "yield",
            Self::CoroutineStatus => "status",
            Self::CoroutineWrap => "wrap",
            Self::CoroutineRunning => "running",
            Self::CoroutineIsYieldable => "isyieldable",
            Self::CoroutineClose => "close",
            Self::DebugGetupvalue => "getupvalue",
            Self::DebugUpvalueid => "upvalueid",
            Self::DebugUpvaluejoin => "upvaluejoin",
            Self::DebugSetupvalue => "setupvalue",
            Self::DebugGetinfo => "getinfo",
            Self::DebugGetmetatable => "getmetatable",
            Self::DebugSetmetatable => "setmetatable",
            Self::DebugTraceback => "traceback",
            Self::DebugSethook => "sethook",
            Self::DebugGethook => "gethook",
            Self::DebugSetuservalue => "setuservalue",
        }
    }

    /// Reverse mapping for the codec's `NativeFunction` encoding
    /// (`HeapObject::NativeCallable { provider: 0, function, .. }`, see
    /// `docs/features/table-closure-coroutine-cutover.md` §2). Relies on
    /// `#[repr(u32)]`'s implicit sequential discriminants matching this
    /// list's declaration order exactly - covered by a round-trip test.
    pub(super) fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Print,
            1 => Self::Assert,
            2 => Self::Type,
            3 => Self::ToString,
            4 => Self::ToNumber,
            5 => Self::StringLen,
            6 => Self::StringByte,
            7 => Self::StringChar,
            8 => Self::StringSub,
            9 => Self::StringLower,
            10 => Self::StringUpper,
            11 => Self::StringReverse,
            12 => Self::StringRep,
            13 => Self::StringDump,
            14 => Self::RawGet,
            15 => Self::RawSet,
            16 => Self::RawEqual,
            17 => Self::RawLen,
            18 => Self::GetMetatable,
            19 => Self::SetMetatable,
            20 => Self::Error,
            21 => Self::PCall,
            22 => Self::XCall,
            23 => Self::Select,
            24 => Self::Next,
            25 => Self::Pairs,
            26 => Self::IPairs,
            27 => Self::IPairsIterator,
            28 => Self::TableConcat,
            29 => Self::TableInsert,
            30 => Self::TableRemove,
            31 => Self::TablePack,
            32 => Self::TableUnpack,
            33 => Self::TableSort,
            34 => Self::TableMove,
            35 => Self::MathAbs,
            36 => Self::MathFloor,
            37 => Self::MathCeil,
            38 => Self::MathMin,
            39 => Self::MathMax,
            40 => Self::MathToInteger,
            41 => Self::MathType,
            42 => Self::MathSqrt,
            43 => Self::MathSin,
            44 => Self::MathCos,
            45 => Self::MathTan,
            46 => Self::MathExp,
            47 => Self::MathLog,
            48 => Self::MathAcos,
            49 => Self::MathAsin,
            50 => Self::MathAtan,
            51 => Self::MathDeg,
            52 => Self::MathRad,
            53 => Self::MathFmod,
            54 => Self::MathModf,
            55 => Self::MathUlt,
            56 => Self::MathFrexp,
            57 => Self::MathLdexp,
            58 => Self::MathRandom,
            59 => Self::MathRandomSeed,
            60 => Self::Utf8Len,
            61 => Self::Utf8Char,
            62 => Self::Utf8Codepoint,
            63 => Self::Utf8Offset,
            64 => Self::Utf8Codes,
            65 => Self::Utf8IteratorStrict,
            66 => Self::Utf8IteratorLax,
            67 => Self::Require,
            68 => Self::PackageSearchPath,
            69 => Self::PackageSearcherPreload,
            70 => Self::PackageSearcherLua,
            71 => Self::PackageSearcherC,
            72 => Self::PackageSearcherCRoot,
            73 => Self::StringFind,
            74 => Self::StringMatch,
            75 => Self::StringGMatch,
            76 => Self::StringGSub,
            77 => Self::StringFormat,
            78 => Self::TableCreate,
            79 => Self::CollectGarbage,
            80 => Self::OsTime,
            81 => Self::OsClock,
            82 => Self::OsDifftime,
            83 => Self::OsDate,
            84 => Self::OsGetenv,
            85 => Self::OsExit,
            86 => Self::IoWrite,
            87 => Self::IoRead,
            88 => Self::IoInput,
            89 => Self::FileWrite,
            90 => Self::IoOutput,
            91 => Self::FileClose,
            92 => Self::FileGc,
            93 => Self::OsRemove,
            94 => Self::OsSetlocale,
            95 => Self::OsTmpname,
            96 => Self::PackageLoadLib,
            97 => Self::Load,
            98 => Self::DoFile,
            99 => Self::StringPack,
            100 => Self::StringUnpack,
            101 => Self::StringPackSize,
            102 => Self::CoroutineCreate,
            103 => Self::CoroutineResume,
            104 => Self::CoroutineYield,
            105 => Self::CoroutineStatus,
            106 => Self::CoroutineWrap,
            107 => Self::CoroutineRunning,
            108 => Self::CoroutineIsYieldable,
            109 => Self::CoroutineClose,
            110 => Self::DebugGetupvalue,
            111 => Self::DebugUpvalueid,
            112 => Self::DebugUpvaluejoin,
            113 => Self::DebugSetupvalue,
            114 => Self::DebugGetinfo,
            115 => Self::DebugGetmetatable,
            116 => Self::DebugSetmetatable,
            117 => Self::DebugTraceback,
            118 => Self::DebugSethook,
            119 => Self::DebugGethook,
            120 => Self::DebugSetuservalue,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct LuaError {
    pub message: String,
    pub stack: Vec<String>,
    /// The exact Lua value `error()`/`assert()` raised with, when known -
    /// preserved so `pcall`/`xpcall`/`coroutine.resume` hand the original
    /// value back unchanged (including its type: a table or number raised
    /// via `error(v)` used to always collapse to a stringified `message`
    /// here, losing identity). `None` for errors synthesized internally by
    /// the runtime itself (e.g. "attempt to call a nil value"), which have
    /// no separate Lua value to preserve beyond their message text.
    pub value: Option<LuaValue>,
    /// Which `Instr::Binary` operand (if any) produced a "no integer
    /// representation" bitwise/shift error - set by `binary_resolve` so the
    /// `Instr::Binary` dispatch site can look up that operand's originating
    /// register and, when it was just loaded by a `GetField`, annotate the
    /// message with `(field '<name>')` the way real Lua's `getobjname`
    /// would, without plumbing general debug-name tracking through the rest
    /// of the runtime.
    pub(super) operand_hint: Option<OperandSide>,
    /// Bytes buffered by `print`/`io.write` before this error was raised.
    /// Real Lua writes each call straight to stdout as it happens; Sol
    /// buffers output in memory for the whole run and only flushes it once
    /// the run finishes, so an uncaught error must carry whatever was
    /// buffered along with it - otherwise the caller's success-only flush
    /// (`LuaRun::output`) never runs and that output is lost entirely, even
    /// though it was already "printed" as far as the script is concerned.
    /// Populated by the `run_program*` entry points in `mod.rs`, not by
    /// individual error sites.
    pub output: Vec<u8>,
    /// Set only by `coroutine.close()`'s self-close forced abort (closing
    /// the very coroutine that is calling `close` on itself, real Lua's
    /// `luaB_close`'s `COS_RUN` case: `lua_closethread` on the running
    /// thread longjmps straight past every intervening `pcall`/`xpcall`
    /// protection level back to the `resume` boundary, rather than raising a
    /// catchable error). `unwind_error_to_marker` never stops such an error
    /// at a `pcall`/`xpcall` marker - it always propagates to the top of the
    /// coroutine's own `drive`, where `resume_coroutine` recognizes the flag
    /// and converts a clean unwind (no `<close>` handler raised its own new
    /// error) into an ordinary successful zero-value return instead of an
    /// error, matching `resume` returning `(true)` with no extra values.
    pub(super) uncatchable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OperandSide {
    Left,
    Right,
}

impl LuaError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stack: Vec::new(),
            value: None,
            operand_hint: None,
            output: Vec::new(),
            uncatchable: false,
        }
    }

    /// Records which binary operand this error's message describes, for the
    /// `Instr::Binary` dispatch site to use when annotating "no integer
    /// representation" errors with their source field name.
    pub(super) fn with_operand_hint(mut self, side: OperandSide) -> Self {
        self.operand_hint = Some(side);
        self
    }

    /// Marks this error as bypassing every `pcall`/`xpcall` marker during
    /// unwinding - see the `uncatchable` field doc. Only
    /// `coroutine.close()`'s self-close path constructs one of these.
    pub(super) fn make_uncatchable(mut self) -> Self {
        self.uncatchable = true;
        self
    }

    /// Builds an error from an explicit Lua value (`error(v)`/`assert(c, v)`),
    /// keeping `v` itself available via `value` while `message` gets a
    /// human-readable fallback for contexts that only have text (this
    /// `Display` impl, uncaught-error CLI reporting) - real Lua's own
    /// `error()` doesn't invoke `__tostring` either, so this fallback never
    /// runs user code as a side effect of raising.
    pub(super) fn raised(value: LuaValue, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            stack: Vec::new(),
            value: Some(value),
            operand_hint: None,
            output: Vec::new(),
            uncatchable: false,
        }
    }

    pub(super) fn at(mut self, function: &str) -> Self {
        self.stack.push(function.into());
        self
    }

    /// Like `at`, but for a caller that discovers *outer* call-chain levels
    /// after the innermost one has already been recorded (`unwind_error_to_marker`'s
    /// frame walk, innermost-frame-first): `at` always appends, so pushing
    /// outer frames the same way would print them (after `Display`'s/
    /// `debug.traceback`'s shared `.rev()`) before the innermost frame
    /// instead of after it. Inserting at the front instead keeps every
    /// caller's own already-recorded entries in their original relative
    /// order while still placing this new, more-outer frame beyond them
    /// once reversed.
    pub(super) fn at_outer_frame(mut self, function: &str) -> Self {
        self.stack.insert(0, function.into());
        self
    }

    /// The value `pcall`/`xpcall`/`coroutine.resume` should hand back for
    /// this error: the original raised value if known, otherwise the error
    /// message as a plain Lua string (matching real Lua's behavior for
    /// errors it synthesizes itself, which are always strings).
    pub(super) fn into_lua_value(self, heap: &RcRef<sol_core::Heap>) -> LuaValue {
        match self.value {
            Some(value) => value,
            None => LuaValue::String(CanonicalString::fresh(heap.clone(), self.message)),
        }
    }
}

impl fmt::Display for LuaError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.message)?;
        if !self.stack.is_empty() {
            write!(out, "\nstack traceback:")?;
            for frame in self.stack.iter().rev() {
                write!(out, "\n\t{frame}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for LuaError {}

/// back to a shared `base` environment on a read miss. Every loaded module gets a fresh
/// `Globals` whose `base` is the runtime's single shared root scope (never
/// the requiring module's own scope) — this is `require`'s module isolation.
/// All closures compiled from the same top-level program/module share one
/// `Globals` clone; only `require` ever creates a new one.
///
/// Every method that can touch a stored value (as opposed to bookkeeping
/// like `has_base`/`check_writable`) takes an explicit `&LuaRuntime`: the
/// table itself is now a bare `TableRef` handle into the canonical heap, and
/// reading or writing an arbitrary `LuaValue` through it always goes through
/// `LuaRuntime`'s value codec (`codec.rs`), not just a raw heap borrow.
#[derive(Clone, Debug)]
pub(super) struct Globals(Rc<GlobalsInner>);

struct GlobalsInner {
    table: TableRef,
    /// The live `_ENV` value shared by every closure compiled against this
    /// scope.  Lua permits it to be any value; global reads/writes then use
    /// ordinary indexing semantics and naturally fail for non-indexable
    /// values. `table` remains the root/module bookkeeping table for the
    /// legacy module loader while production objects migrate to sol-core.
    environment: RefCell<LuaValue>,
    constants: RefCell<HashSet<String>>,
    base: Option<Globals>,
}

impl fmt::Debug for GlobalsInner {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("GlobalsInner")
            .field("table", &self.table)
            .field("environment", &self.environment)
            .field("constants", &self.constants)
            .field("base", &self.base)
            .finish()
    }
}

impl Globals {
    pub(super) fn identity_address(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }

    /// Only called during `LuaRuntime` construction itself (`init.rs`),
    /// before a `&LuaRuntime` exists to pass - takes the heap directly
    /// rather than `&LuaRuntime` for that reason, unlike every other
    /// constructor here.
    pub(super) fn root(heap: &RcRef<sol_core::Heap>) -> Self {
        let table = TableRef::alloc(&mut heap.borrow_mut());
        Globals(Rc::new(GlobalsInner {
            table,
            environment: RefCell::new(LuaValue::Table(table)),
            constants: RefCell::new(HashSet::new()),
            base: None,
        }))
    }

    pub(super) fn module(base: &Globals, runtime: &LuaRuntime) -> Self {
        let table = TableRef::alloc(&mut runtime.canonical_heap.borrow_mut());
        Globals(Rc::new(GlobalsInner {
            table,
            environment: RefCell::new(LuaValue::Table(table)),
            constants: RefCell::new(HashSet::new()),
            base: Some(base.clone()),
        }))
    }

    /// Wraps an arbitrary Lua value as a chunk's `_ENV`. This is valid Lua:
    /// the value is only required to be a table when the chunk actually
    /// indexes a global name.
    pub(super) fn from_value(runtime: &LuaRuntime, value: LuaValue) -> Self {
        let table = match &value {
            LuaValue::Table(table) => *table,
            _ => TableRef::alloc(&mut runtime.canonical_heap.borrow_mut()),
        };
        Globals(Rc::new(GlobalsInner {
            table,
            environment: RefCell::new(value),
            constants: RefCell::new(HashSet::new()),
            base: None,
        }))
    }

    pub(super) fn as_value(&self) -> LuaValue {
        self.0.environment.borrow().clone()
    }

    /// Gives a `load()`ed chunk with no explicit fourth argument its own
    /// `_ENV` upvalue cell, starting out at this scope's current value (so
    /// ordinary global reads/writes still land in the same shared table)
    /// but independent of it (so a bare `_ENV = ...` reassignment inside the
    /// loaded chunk rebinds only that chunk's own cell, matching real Lua -
    /// `Globals::clone` is an `Rc` clone that shares one mutable cell across
    /// every closure holding it, which `load` must not do here).
    pub(super) fn snapshot_for_load(&self, runtime: &LuaRuntime) -> Self {
        let mut snapshot = Globals::from_value(runtime, self.as_value());
        Rc::get_mut(&mut snapshot.0)
            .expect("snapshot_for_load: freshly constructed Rc has no other owner yet")
            .base = self.0.base.clone();
        snapshot
    }

    /// Rebinds this scope's implicit `_ENV` upvalue. The shared inner cell
    /// means closures which already captured this scope observe the new
    /// value too, as they do in Lua.
    pub(super) fn set_value(&self, value: LuaValue) {
        *self.0.environment.borrow_mut() = value;
    }

    /// Whether this scope falls back to a `base` scope on a raw miss (real
    /// Lua has no such concept - this is Sol's own `require`-module-isolation
    /// mechanism, see `module`). A scope with a `base` never also carries a
    /// caller-supplied metatable in practice, so dispatch uses this to
    /// decide between the fast `base`-chain path and the metatable-aware
    /// `index_resolve`/`set_index_resolve` path real Lua's `_ENV.name` access
    /// actually goes through.
    pub(super) fn has_base(&self) -> bool {
        self.0.base.is_some()
    }

    /// Shared by `assign` and dispatch's metatable-aware global-write path:
    /// Sol's own `global x <const> = ...` guard, which must apply regardless
    /// of whether the write ultimately resolves through a raw `set` or a
    /// `__newindex` metamethod.
    pub(super) fn check_writable(&self, name: &str) -> LuaResult<()> {
        if self.0.constants.borrow().contains(name) {
            return Err(LuaError::new(format!(
                "attempt to assign to const variable '{name}'"
            )));
        }
        Ok(())
    }

    pub(super) fn get(&self, runtime: &LuaRuntime, name: &str) -> LuaValue {
        let value = match &*self.0.environment.borrow() {
            LuaValue::Table(table) => runtime
                .table_get_str_field(*table, name.as_bytes())
                .unwrap_or(LuaValue::Nil),
            _ => LuaValue::Nil,
        };
        if value != LuaValue::Nil {
            return value;
        }
        match &self.0.base {
            Some(base) => base.get(runtime, name),
            None => LuaValue::Nil,
        }
    }

    /// Unconditional overwrite (a `global` declaration): ignores any
    /// existing binding's constness.
    pub(super) fn define(&self, runtime: &LuaRuntime, name: &str, value: LuaValue, constant: bool) {
        let key = LuaValue::String(CanonicalString::intern(runtime.canonical_heap.clone(), name));
        runtime.table_set(self.0.table, key, value).unwrap();
        if constant {
            self.0.constants.borrow_mut().insert(name.to_string());
        } else {
            self.0.constants.borrow_mut().remove(name);
        }
    }

    /// A plain assignment to a global-resolved name: errors if an existing
    /// binding (in this scope only, never `base`) is const; otherwise
    /// updates it in place, or creates a fresh non-const binding.
    pub(super) fn assign(&self, runtime: &LuaRuntime, name: &str, value: LuaValue) -> LuaResult<()> {
        self.check_writable(name)?;
        let key = LuaValue::String(CanonicalString::intern(runtime.canonical_heap.clone(), name));
        runtime.table_set(self.0.table, key, value)?;
        Ok(())
    }
}

// Real Lua's float-to-string conversion (`lobject.c`'s `tostringbuff`) is not
// a fixed-precision `%.14g`: this pinned Lua 5.5.1 build instead generates
// the *shortest* decimal digit string that round-trips back to the same
// double (occasionally one digit longer than strictly minimal in hard cases -
// e.g. `1/3` prints with 17 digits even though 16 already round-trip - which
// is a known characteristic of Grisu-family shortest-digit generators without
// a slow-path fallback), then places the decimal point using the classic
// `%g` plain-vs-scientific rule with precision `max(digit count, 15)`
// (verified experimentally against the pinned `lua5.5` reference binary,
// since this isn't documented behavior). Only the round-trip and
// distinctness properties are load-bearing for Lua compatibility (the corpus
// checks `tonumber(tostring(x)) == x`, never exact digit strings), and
// Rust's `{:e}` formatter for `f64` already produces a shortest round-trip
// digit string, so it is reused as the digit source here rather than
// reimplementing a dtoa algorithm from scratch.
fn format_lua_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value == f64::INFINITY {
        return "inf".to_string();
    };
    if value == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust's `{:e}` formatter always emits an 'e'");
    let exponent: i32 = exponent
        .parse()
        .expect("Rust's `{:e}` formatter always emits an integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digit_count = digits.len() as i32;
    let precision = digit_count.max(15);

    if exponent < -4 || exponent >= precision {
        let mut out = String::new();
        out.push_str(sign);
        out.push_str(&digits[..1]);
        if digit_count > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exponent.abs()));
        out
    } else if exponent >= 0 {
        let integer_digits = (exponent + 1) as usize;
        let mut out = String::new();
        out.push_str(sign);
        if (digit_count as usize) <= integer_digits {
            out.push_str(&digits);
            out.push_str(&"0".repeat(integer_digits - digit_count as usize));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..integer_digits]);
            out.push('.');
            out.push_str(&digits[integer_digits..]);
        }
        out
    } else {
        let mut out = String::new();
        out.push_str(sign);
        out.push_str("0.");
        out.push_str(&"0".repeat((-exponent - 1) as usize));
        out.push_str(&digits);
        out
    }
}

impl LuaValue {
    pub(super) fn is_callable(&self) -> bool {
        matches!(
            self,
            Self::Closure(_)
                | Self::NativeFunction(_)
                | Self::Native(_)
                | Self::RegisteredNative(_)
                | Self::CFunction(_)
                | Self::GMatchIterator(_)
                | Self::CoroutineWrapper(_)
        )
    }

    pub fn truthy(&self) -> bool {
        !matches!(self, Self::Nil | Self::Bool(false))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Nil => "nil",
            Self::Bool(_) => "boolean",
            Self::Integer(_) | Self::Float(_) => "number",
            Self::String(_) => "string",
            Self::Table(_) | Self::CanonicalTable(_) => "table",
            Self::Closure(_)
            | Self::NativeFunction(_)
            | Self::Native(_)
            | Self::RegisteredNative(_)
            | Self::CFunction(_)
            | Self::GMatchIterator(_)
            | Self::CoroutineWrapper(_) => "function",
            Self::Thread(_) => "thread",
            Self::Userdata(_) | Self::LightUserdata(_) => "userdata",
        }
    }

    pub fn display_bytes(&self) -> Vec<u8> {
        match self {
            Self::Nil => b"nil".to_vec(),
            Self::Bool(value) => value.to_string().into_bytes(),
            Self::Integer(value) => value.to_string().into_bytes(),
            Self::Float(value) => format_lua_float(*value).into_bytes(),
            Self::String(value) => value.as_bytes().to_vec(),
            value @ (Self::Table(_) | Self::CanonicalTable(_)) => {
                format!("table: 0x{:x}", value.identity_address().unwrap()).into_bytes()
            }
            value @ (Self::Closure(_)
            | Self::NativeFunction(_)
            | Self::Native(_)
            | Self::RegisteredNative(_)
            | Self::CFunction(_)
            | Self::GMatchIterator(_)
            | Self::CoroutineWrapper(_)) => {
                format!("function: 0x{:x}", value.identity_address().unwrap()).into_bytes()
            }
            value @ Self::Thread(_) => {
                format!("thread: 0x{:x}", value.identity_address().unwrap()).into_bytes()
            }
            value @ (Self::Userdata(_) | Self::LightUserdata(_)) => {
                format!("userdata: 0x{:x}", value.identity_address().unwrap()).into_bytes()
            }
        }
    }

    pub(super) fn identity_address(&self) -> Option<usize> {
        match self {
            Self::String(value) => {
                // The canonical heap interns every string by content (real
                // Lua only interns short strings), so `%p`-style identity is
                // already content-stable and object-identical here.
                Some(value.object_id().raw() as usize | 1)
            }
            Self::Table(value) => Some(value.object_id().raw() as usize),
            Self::CanonicalTable(value) => Some(value.object_id().raw() as usize),
            Self::Closure(value) => Some(value.object_id().raw() as usize),
            Self::NativeFunction(value) => Some(*value as u32 as usize + 1),
            Self::Native(value) => Some(Rc::as_ptr(value) as usize),
            Self::RegisteredNative(value) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(value, &mut hasher);
                Some(std::hash::Hasher::finish(&hasher) as usize | 1)
            }
            Self::CFunction(value) => Some(value.object_id().raw() as usize),
            Self::GMatchIterator(value) => Some(value.object_id().raw() as usize),
            Self::Thread(value) | Self::CoroutineWrapper(value) => {
                Some(value.object_id().raw() as usize)
            }
            Self::Userdata(value) => Some(value.object_id().raw() as usize),
            Self::LightUserdata(identity) => Some(*identity),
            Self::Nil | Self::Bool(_) | Self::Integer(_) | Self::Float(_) => None,
        }
    }

    pub(super) fn number(&self) -> LuaResult<Number> {
        match self {
            Self::Integer(value) => Ok(Number::Integer(*value)),
            Self::Float(value) => Ok(Number::Float(*value)),
            _ => Err(LuaError::new(format!(
                "attempt to perform arithmetic on a {} value",
                self.type_name()
            ))),
        }
    }
}

impl PartialEq for LuaValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nil, Self::Nil) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Integer(a), Self::Float(b)) | (Self::Float(b), Self::Integer(a)) => {
                b.is_finite()
                    && b.fract() == 0.0
                    && *b >= i64::MIN as f64
                    && *b < -(i64::MIN as f64)
                    && *a == *b as i64
            }
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Table(a), Self::Table(b)) => a.object_id() == b.object_id(),
            (Self::CanonicalTable(a), Self::CanonicalTable(b)) => a.object_id() == b.object_id(),
            (Self::Closure(a), Self::Closure(b)) => a.object_id() == b.object_id(),
            (Self::NativeFunction(a), Self::NativeFunction(b)) => a == b,
            (Self::Native(a), Self::Native(b)) => Rc::ptr_eq(a, b),
            (Self::RegisteredNative(a), Self::RegisteredNative(b)) => a == b,
            (Self::CFunction(a), Self::CFunction(b)) => a.object_id() == b.object_id(),
            (Self::GMatchIterator(a), Self::GMatchIterator(b)) => a.object_id() == b.object_id(),
            (Self::Thread(a), Self::Thread(b)) => a.object_id() == b.object_id(),
            (Self::CoroutineWrapper(a), Self::CoroutineWrapper(b)) => {
                a.object_id() == b.object_id()
            }
            (Self::Userdata(a), Self::Userdata(b)) => a.object_id() == b.object_id(),
            (Self::LightUserdata(a), Self::LightUserdata(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Number {
    Integer(i64),
    Float(f64),
}

#[cfg(test)]
mod native_function_codec_tests {
    use super::NativeFunction;

    #[test]
    fn from_u32_round_trips_every_discriminant() {
        let mut count = 0u32;
        while let Some(function) = NativeFunction::from_u32(count) {
            assert_eq!(function as u32, count);
            count += 1;
        }
        assert_eq!(NativeFunction::from_u32(count), None);
        assert!(count > 0, "NativeFunction should have at least one variant");
    }
}
