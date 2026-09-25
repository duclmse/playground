//! Core value representation for the dynamic `.lua` runtime: `LuaValue`
//! and the types it is built from (tables, closures, native bridges, keys,
//! errors, global scopes). See `lua_runtime` module docs for the split
//! rationale.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;

use crate::lua_bytecode::Proto;

use super::*;

/// Shorthand for the runtime's pervasive shared-mutable-cell pattern:
/// reference-counted, aliasable ownership over a heap-allocated `T` (tables,
/// closure upvalue cells, `gmatch` iterator state, ...).
pub(super) type RcRef<T> = Rc<RefCell<T>>;

/// A non-owning counterpart to `RcRef<T>`, used by the GC's tracking registries
/// (`weak_tables`, `gc_tables`) so holding a reference there never keeps the
/// referent alive by itself.
pub(super) type WeakRef<T> = Weak<RefCell<T>>;

/// A closure's upvalue-cell storage: one slot per captured variable, `None`
/// where a slot has been recycled from the frame pool but not yet initialized
/// for the current call.
pub(super) type Cells = Vec<Option<RcRef<LuaValue>>>;

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
/// persistent C handles). Not yet used by `LuaValue`/`LuaKey` — see
/// docs/features/table-closure-coroutine-cutover.md §8 for why the flip has
/// to land together with `ClosureRef`/`ThreadRef` rather than on its own.
// `#[allow(dead_code)]` throughout this ref-type/wrapper-method group: this
// is prerequisite plumbing (docs/features/table-closure-coroutine-cutover.md
// §8 step 2), unit-tested directly against `sol_core::Heap` below, but not
// yet wired into `LuaValue`/`LuaKey` or any call site — that has to land as
// the single coordinated flip in step 4.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct TableRef(sol_core::ObjectId);

/// The `ClosureRef` counterpart to `TableRef`; see its doc comment.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ClosureRef(sol_core::ObjectId);

/// The `ThreadRef` counterpart to `TableRef`; see its doc comment.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ThreadRef(sol_core::ObjectId);

#[allow(dead_code)]
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
        heap.table_get(self.0, key)
            .expect("TableRef must address a live table object")
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    Table(RcRef<LuaTable>),
    /// Canonical table used by the embedding registry and userdata metatables
    /// while legacy interpreter tables are migrated incrementally.
    CanonicalTable(CanonicalTable),
    Closure(Rc<LuaClosure>),
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
    GMatchIterator(RcRef<GMatchState>),
    /// A coroutine created by `coroutine.create`. See `LuaCoroutine`.
    Thread(Rc<LuaCoroutine>),
    /// The callable wrapper `coroutine.wrap` returns: calling it resumes the
    /// underlying coroutine directly, propagating an error raised inside the
    /// coroutine as a real Lua error instead of `coroutine.resume`'s
    /// `(false, message)` pair - matching real Lua's `coroutine.wrap`.
    CoroutineWrapper(Rc<LuaCoroutine>),
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
}

/// A table key. Real Lua allows any non-nil, non-NaN value as a key,
/// including tables and functions - those compare and hash by *identity*
/// (`Rc` pointer), not by structural content, matching `LuaValue`'s own
/// `PartialEq` for these variants. `PartialEq`/`Eq`/`Hash` are implemented
/// by hand below rather than derived, since deriving them on the `Rc`
/// payload would hash/compare by dereferenced content instead.
#[derive(Clone, Debug)]
pub(super) enum LuaKey {
    Bool(bool),
    Integer(i64),
    Float(u64),
    String(CanonicalString),
    Table(RcRef<LuaTable>),
    CanonicalTable(CanonicalTable),
    Closure(Rc<LuaClosure>),
    NativeFunction(NativeFunction),
    Native(Rc<NativeBridge>),
    RegisteredNative(sol_core::NativeCallableId),
    CFunction(CanonicalCFunction),
    GMatchIterator(RcRef<GMatchState>),
    Thread(Rc<LuaCoroutine>),
    CoroutineWrapper(Rc<LuaCoroutine>),
    Userdata(CanonicalUserdata),
    LightUserdata(usize),
}

impl PartialEq for LuaKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Table(a), Self::Table(b)) => Rc::ptr_eq(a, b),
            (Self::CanonicalTable(a), Self::CanonicalTable(b)) => a.object_id() == b.object_id(),
            (Self::Closure(a), Self::Closure(b)) => Rc::ptr_eq(a, b),
            (Self::NativeFunction(a), Self::NativeFunction(b)) => a == b,
            (Self::Native(a), Self::Native(b)) => Rc::ptr_eq(a, b),
            (Self::RegisteredNative(a), Self::RegisteredNative(b)) => a == b,
            (Self::CFunction(a), Self::CFunction(b)) => a.object_id() == b.object_id(),
            (Self::GMatchIterator(a), Self::GMatchIterator(b)) => Rc::ptr_eq(a, b),
            (Self::Thread(a), Self::Thread(b)) => Rc::ptr_eq(a, b),
            (Self::CoroutineWrapper(a), Self::CoroutineWrapper(b)) => Rc::ptr_eq(a, b),
            (Self::Userdata(a), Self::Userdata(b)) => a.object_id() == b.object_id(),
            (Self::LightUserdata(a), Self::LightUserdata(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for LuaKey {}

impl std::hash::Hash for LuaKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bool(value) => value.hash(state),
            Self::Integer(value) => value.hash(state),
            Self::Float(value) => value.hash(state),
            Self::String(value) => value.hash(state),
            Self::Table(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::CanonicalTable(value) => value.object_id().hash(state),
            Self::Closure(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::NativeFunction(value) => value.hash(state),
            Self::Native(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::RegisteredNative(value) => value.hash(state),
            Self::CFunction(value) => value.object_id().hash(state),
            Self::GMatchIterator(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::Thread(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::CoroutineWrapper(value) => (Rc::as_ptr(value) as usize).hash(state),
            Self::Userdata(value) => value.object_id().hash(state),
            Self::LightUserdata(value) => value.hash(state),
        }
    }
}

#[derive(Debug, Default)]
pub struct LuaTable {
    pub(super) array: Vec<LuaValue>,
    /// A cached, incrementally-maintained border (`len`'s fast path): the
    /// largest known `n` with `array[0..n]` all non-nil. `#` is undefined by
    /// the Lua manual for a table with holes (any border is a legal
    /// answer), but real Lua's actual choice for a given construction
    /// history is externally observable (e.g. from a table built through
    /// `SetArrayMulti`/`SetArrayItem`), so this tracks the same border a
    /// dense, append-only history naturally produces rather than a
    /// generic-but-possibly-different one a fresh binary search over the
    /// whole array could return. See `set` for how it's kept in sync in
    /// O(1) amortized per write, and `recompute_array_border` for the rare
    /// paths (direct array mutation) that must fall back to an explicit
    /// scan.
    pub(super) array_border: usize,
    // An insertion-order-preserving map, not a plain `HashMap`: `next`
    // resumes traversal by re-locating the last-returned key in a freshly
    // fetched snapshot of this map and continuing from there (see
    // `LuaRuntime::next`). A plain `HashMap` can silently reorder existing
    // entries on `insert` even when overwriting an already-present key (its
    // capacity-growth check runs before it knows whether the key already
    // exists), which would let already-visited entries reappear after the
    // "current" position or strand not-yet-visited entries before it -
    // corrupting `pairs`/`next` traversal without any visible error.
    // `IndexMap` never repositions an existing key on overwrite.
    pub(super) hash: IndexMap<LuaKey, LuaValue>,
    pub(super) metatable: Option<RcRef<LuaTable>>,
    /// Incremented by every raw mutation and metatable replacement. Dynamic
    /// inline caches can guard this value without changing table semantics.
    pub(super) version: u64,
    /// Set once this table's `__gc` metamethod (if any) has been called by
    /// `collect_cycles`, so a finalizer never runs twice for the same table.
    pub(super) finalized: bool,
    /// Cumulative allocation-budget bytes `charge_new_table_entry` has
    /// charged for this table's own field growth (beyond its fixed header,
    /// which `charge_allocation(size_of::<LuaTable>())` already covers at
    /// creation). `collect_cycles` credits this back alongside the header
    /// when reclaiming the table, so a script that keeps a table's fields
    /// churning under `collectgarbage()` doesn't leak budget it never
    /// actually keeps allocated - see `set_index_resolve`/`raw_set_index`.
    pub(super) charged_bytes: usize,
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

#[derive(Clone, Debug)]
pub struct LuaClosure {
    pub(super) proto: Rc<Proto>,
    /// `RefCell`-wrapped so the cycle collector (see `collect_cycles`) can
    /// clear a closure's captured upvalues once it determines the closure
    /// is part of an unreachable reference cycle - breaking the cycle's
    /// `Rc` links so normal refcounting reclaims the rest of it.
    pub(super) upvals: RefCell<Vec<RcRef<LuaValue>>>,
    pub(super) globals: Globals,
}

/// Internal state for a `string.gmatch` iterator: the subject/pattern bytes
/// it was created with, and the byte offset to resume searching from.
#[derive(Debug)]
pub struct GMatchState {
    pub(super) source: Rc<Vec<u8>>,
    pub(super) pattern: Rc<Vec<u8>>,
    pub(super) position: usize,
    pub(super) last_end: Option<usize>,
}

/// A dynamic Lua global environment backed by an ordinary `LuaTable`, falling
/// back to a shared `base` environment on a read miss. Every loaded module gets a fresh
/// `Globals` whose `base` is the runtime's single shared root scope (never
/// the requiring module's own scope) — this is `require`'s module isolation.
/// All closures compiled from the same top-level program/module share one
/// `Globals` clone; only `require` ever creates a new one.
#[derive(Clone, Debug)]
pub(super) struct Globals(Rc<GlobalsInner>);

struct GlobalsInner {
    heap: RcRef<sol_core::Heap>,
    table: RcRef<LuaTable>,
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

    pub(super) fn root(heap: RcRef<sol_core::Heap>) -> Self {
        let table = Rc::new(RefCell::new(LuaTable::default()));
        Globals(Rc::new(GlobalsInner {
            heap,
            table: table.clone(),
            environment: RefCell::new(LuaValue::Table(table)),
            constants: RefCell::new(HashSet::new()),
            base: None,
        }))
    }

    pub(super) fn module(base: &Globals) -> Self {
        let table = Rc::new(RefCell::new(LuaTable::default()));
        Globals(Rc::new(GlobalsInner {
            heap: base.0.heap.clone(),
            table: table.clone(),
            environment: RefCell::new(LuaValue::Table(table)),
            constants: RefCell::new(HashSet::new()),
            base: Some(base.clone()),
        }))
    }

    /// Wraps an arbitrary Lua value as a chunk's `_ENV`. This is valid Lua:
    /// the value is only required to be a table when the chunk actually
    /// indexes a global name.
    pub(super) fn from_value(heap: RcRef<sol_core::Heap>, value: LuaValue) -> Self {
        let table = match &value {
            LuaValue::Table(table) => table.clone(),
            _ => Rc::new(RefCell::new(LuaTable::default())),
        };
        Globals(Rc::new(GlobalsInner {
            heap,
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
    pub(super) fn snapshot_for_load(&self) -> Self {
        let mut snapshot = Globals::from_value(self.0.heap.clone(), self.as_value());
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

    pub(super) fn get(&self, name: &str) -> LuaValue {
        let key = LuaValue::String(CanonicalString::intern(self.0.heap.clone(), name));
        let value = match &*self.0.environment.borrow() {
            LuaValue::Table(table) => table.borrow().get(&key).unwrap(),
            _ => LuaValue::Nil,
        };
        if value != LuaValue::Nil {
            return value;
        }
        match &self.0.base {
            Some(base) => base.get(name),
            None => LuaValue::Nil,
        }
    }

    /// Unconditional overwrite (a `global` declaration): ignores any
    /// existing binding's constness.
    pub(super) fn define(&self, name: &str, value: LuaValue, constant: bool) {
        let key = LuaValue::String(CanonicalString::intern(self.0.heap.clone(), name));
        self.0.table.borrow_mut().set(key, value).unwrap();
        if constant {
            self.0.constants.borrow_mut().insert(name.to_string());
        } else {
            self.0.constants.borrow_mut().remove(name);
        }
    }

    /// A plain assignment to a global-resolved name: errors if an existing
    /// binding (in this scope only, never `base`) is const; otherwise
    /// updates it in place, or creates a fresh non-const binding.
    pub(super) fn assign(&self, name: &str, value: LuaValue) -> LuaResult<()> {
        self.check_writable(name)?;
        let key = LuaValue::String(CanonicalString::intern(self.0.heap.clone(), name));
        self.0.table.borrow_mut().set(key, value)?;
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
            Self::Table(value) => Some(Rc::as_ptr(value) as usize),
            Self::CanonicalTable(value) => Some(value.object_id().raw() as usize),
            Self::Closure(value) => Some(Rc::as_ptr(value) as usize),
            Self::NativeFunction(value) => Some(*value as u32 as usize + 1),
            Self::Native(value) => Some(Rc::as_ptr(value) as usize),
            Self::RegisteredNative(value) => {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(value, &mut hasher);
                Some(std::hash::Hasher::finish(&hasher) as usize | 1)
            }
            Self::CFunction(value) => Some(value.object_id().raw() as usize),
            Self::GMatchIterator(value) => Some(Rc::as_ptr(value) as usize),
            Self::Thread(value) | Self::CoroutineWrapper(value) => Some(Rc::as_ptr(value) as usize),
            Self::Userdata(value) => Some(value.object_id().raw() as usize),
            Self::LightUserdata(identity) => Some(*identity),
            Self::Nil | Self::Bool(_) | Self::Integer(_) | Self::Float(_) => None,
        }
    }

    fn key(&self) -> LuaResult<LuaKey> {
        match self {
            Self::Bool(value) => Ok(LuaKey::Bool(*value)),
            Self::Integer(value) => Ok(LuaKey::Integer(*value)),
            Self::Float(value) if value.is_nan() => Err(LuaError::new("table index is NaN")),
            // `i64::MAX as f64` rounds up to 2^63, one past the largest
            // representable integer, so the upper bound must be strict -
            // see the identical fix and rationale in `natives.rs`'s
            // `MathToInteger` handler.
            Self::Float(value)
                if value.is_finite()
                    && value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value < -(i64::MIN as f64) =>
            {
                Ok(LuaKey::Integer(*value as i64))
            }
            Self::Float(value) => Ok(LuaKey::Float(value.to_bits())),
            Self::String(value) => Ok(LuaKey::String(value.clone())),
            Self::Table(value) => Ok(LuaKey::Table(value.clone())),
            Self::CanonicalTable(value) => Ok(LuaKey::CanonicalTable(value.clone())),
            Self::Closure(value) => Ok(LuaKey::Closure(value.clone())),
            Self::NativeFunction(value) => Ok(LuaKey::NativeFunction(*value)),
            Self::Native(value) => Ok(LuaKey::Native(value.clone())),
            Self::RegisteredNative(value) => Ok(LuaKey::RegisteredNative(*value)),
            Self::CFunction(value) => Ok(LuaKey::CFunction(value.clone())),
            Self::GMatchIterator(value) => Ok(LuaKey::GMatchIterator(value.clone())),
            Self::Thread(value) => Ok(LuaKey::Thread(value.clone())),
            Self::CoroutineWrapper(value) => Ok(LuaKey::CoroutineWrapper(value.clone())),
            Self::Userdata(value) => Ok(LuaKey::Userdata(value.clone())),
            Self::LightUserdata(value) => Ok(LuaKey::LightUserdata(*value)),
            Self::Nil => Err(LuaError::new("table index is nil")),
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
            (Self::Table(a), Self::Table(b)) => Rc::ptr_eq(a, b),
            (Self::CanonicalTable(a), Self::CanonicalTable(b)) => a.object_id() == b.object_id(),
            (Self::Closure(a), Self::Closure(b)) => Rc::ptr_eq(a, b),
            (Self::NativeFunction(a), Self::NativeFunction(b)) => a == b,
            (Self::Native(a), Self::Native(b)) => Rc::ptr_eq(a, b),
            (Self::RegisteredNative(a), Self::RegisteredNative(b)) => a == b,
            (Self::CFunction(a), Self::CFunction(b)) => a.object_id() == b.object_id(),
            (Self::GMatchIterator(a), Self::GMatchIterator(b)) => Rc::ptr_eq(a, b),
            (Self::Thread(a), Self::Thread(b)) => Rc::ptr_eq(a, b),
            (Self::CoroutineWrapper(a), Self::CoroutineWrapper(b)) => Rc::ptr_eq(a, b),
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

impl LuaTable {
    pub(super) fn get(&self, key: &LuaValue) -> LuaResult<LuaValue> {
        if let Some(index) = positive_array_index(key) {
            return Ok(self.array.get(index - 1).cloned().unwrap_or(LuaValue::Nil));
        }
        // Real Lua only raises "table index is nil"/"table index is NaN"
        // for a *write* (`luaH_newkey`); a read with such a key (or any
        // other key `key()` can't represent) simply can't be present and
        // returns nil, same as any other absent key.
        Ok(key
            .key()
            .ok()
            .and_then(|key| self.hash.get(&key).cloned())
            .unwrap_or(LuaValue::Nil))
    }

    /// Reads a string-keyed field (metamethod-name lookups: `__mode`,
    /// `__gc`, ...) by content, without needing a live canonical heap handle
    /// to construct an interned `LuaKey::String` for an ordinary `get`. A
    /// linear scan is fine here - real tables have only a handful of
    /// metamethod fields, and this only runs off the GC's cold sweep path.
    pub(super) fn get_str_field(&self, name: &[u8]) -> Option<LuaValue> {
        self.hash.iter().find_map(|(key, value)| match key {
            LuaKey::String(key) if key.as_bytes() == name => Some(value.clone()),
            _ => None,
        })
    }

    pub(super) fn set(&mut self, key: LuaValue, value: LuaValue) -> LuaResult<()> {
        if let Some(index) = positive_array_index(&key) {
            let index = index - 1;
            if index >= self.array.len() {
                self.array.resize(index + 1, LuaValue::Nil);
            }
            let is_nil = value == LuaValue::Nil;
            self.array[index] = value;
            if !is_nil {
                // Filling exactly the slot right past the confirmed
                // non-nil prefix extends it - the common `t[#t + 1] = v`
                // append idiom hits this every time, keeping `len` O(1)
                // amortized for it. A `while` (not `if`) because a prior
                // out-of-order write past the border may already have left
                // later slots non-nil too.
                if index == self.array_border {
                    self.array_border += 1;
                    while self.array_border < self.array.len()
                        && self.array[self.array_border] != LuaValue::Nil
                    {
                        self.array_border += 1;
                    }
                }
            } else if index < self.array_border {
                // The confirmed prefix can no longer include `index`, but
                // everything strictly before it is still confirmed.
                self.array_border = index;
            }
            self.version = self.version.wrapping_add(1);
            return Ok(());
        }
        let key = key.key()?;
        if value == LuaValue::Nil {
            // Real Lua guarantees `next` may resume correctly even when the
            // traversal has just set the *current* key to nil (the Lua
            // manual explicitly permits "set[ting] existing fields to
            // nil" mid-traversal). Actually removing the map entry here
            // would erase its slot entirely, so a later `next(t, key)`
            // resuming from it could no longer locate its position and
            // would wrongly raise "invalid key to 'next'" (see
            // `LuaTable::next`). Leaving a nil-valued tombstone in place
            // keeps the slot locatable; `entries`/`len`/`get` already treat
            // a nil value as "not present" so this is otherwise invisible.
            // A key that was never present is left absent rather than
            // inserted as a no-op tombstone.
            if self.hash.contains_key(&key) {
                self.hash.insert(key, LuaValue::Nil);
            }
        } else {
            self.hash.insert(key, value);
        }
        self.version = self.version.wrapping_add(1);
        Ok(())
    }

    /// A "border" (any `n` with `t[n] ~= nil` and `t[n+1] == nil`, per the
    /// Lua manual's `#` operator, which is left undefined when the array
    /// part has holes): `array_border` is maintained incrementally by
    /// `set` precisely so this is O(1), never a linear scan - `t[#t + 1] =
    /// v` in a loop is a common idiom that must not make the loop O(n^2).
    pub(super) fn len(&self) -> usize {
        self.array_border
    }

    /// Recomputes `array_border` from scratch by scanning for the longest
    /// non-nil prefix. Only needed after code that mutates `array` directly
    /// instead of going through `set` (e.g. GC's weak-table pruning), which
    /// is rare enough that an O(n) rescan there is fine.
    pub(super) fn recompute_array_border(&mut self) {
        self.array_border = self
            .array
            .iter()
            .take_while(|value| **value != LuaValue::Nil)
            .count();
    }

    pub(super) fn entries(&self, array_only: bool) -> Vec<(LuaValue, LuaValue)> {
        let mut entries = self
            .array
            .iter()
            .enumerate()
            .filter(|(_, value)| *value != &LuaValue::Nil)
            .map(|(index, value)| (LuaValue::Integer(index as i64 + 1), value.clone()))
            .collect::<Vec<_>>();
        if !array_only {
            entries.extend(
                self.hash
                    .iter()
                    .filter(|(_, value)| *value != &LuaValue::Nil)
                    .map(|(key, value)| (key.value(), value.clone())),
            );
        }
        entries
    }

    /// Like `entries(false)`, but keeps nil-valued array slots and hash
    /// tombstones instead of filtering them out. `LuaTable::next` (the
    /// engine behind `next`/`pairs`) needs this unfiltered view to relocate
    /// a key that was live when it was last yielded and has since been set
    /// to nil - a pattern real Lua explicitly allows during traversal.
    pub(super) fn entries_with_tombstones(&self) -> Vec<(LuaValue, LuaValue)> {
        let mut entries = self
            .array
            .iter()
            .enumerate()
            .map(|(index, value)| (LuaValue::Integer(index as i64 + 1), value.clone()))
            .collect::<Vec<_>>();
        entries.extend(
            self.hash
                .iter()
                .map(|(key, value)| (key.value(), value.clone())),
        );
        entries
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

fn positive_array_index(value: &LuaValue) -> Option<usize> {
    // Sparse integer keys belong in the hash part. Treating every positive
    // i64 as a vector offset lets `t[math.maxinteger] = value` overflow a
    // Rust allocation instead of behaving like an ordinary Lua table key.
    const MAX_DENSE_ARRAY_INDEX: i64 = 1 << 20;
    let index = match value {
        LuaValue::Integer(index) => *index,
        LuaValue::Float(index)
            if index.is_finite()
                && index.fract() == 0.0
                && *index >= 1.0
                && *index <= i64::MAX as f64 =>
        {
            *index as i64
        }
        _ => return None,
    };
    (index > 0 && index <= MAX_DENSE_ARRAY_INDEX).then_some(index as usize)
}

/// Reads `(weak_keys, weak_values)` off a metatable's `__mode` field.
/// Real Lua matches "k"/"v" as substrings of an arbitrary `__mode` string
/// (so `"kv"` and `"vk"` both mean both), not an exact match.
pub(super) fn table_weak_mode(metatable: &RcRef<LuaTable>) -> (bool, bool) {
    match metatable.borrow().get_str_field(b"__mode") {
        Some(LuaValue::String(mode)) => (
            mode.as_bytes().contains(&b'k'),
            mode.as_bytes().contains(&b'v'),
        ),
        _ => (false, false),
    }
}

/// The table's `__gc` metamethod, if its metatable defines one as a callable
/// value. Used by `collect_cycles` to finalize a table right before sweeping
/// it.
pub(super) fn table_finalizer(table: &RcRef<LuaTable>) -> Option<LuaValue> {
    let metatable = table.borrow().metatable.clone()?;
    let value = metatable.borrow().get_str_field(b"__gc")?;
    match value {
        value @ (LuaValue::Closure(_)
        | LuaValue::NativeFunction(_)
        | LuaValue::Native(_)
        | LuaValue::RegisteredNative(_)) => Some(value),
        _ => None,
    }
}

/// True iff `value` is a reference type whose *only* remaining strong
/// reference is the one this weak table itself is holding - i.e. nothing
/// else in the program can still reach it, so a weak table must not keep
/// it alive. Scalars and strings are never weakly collected (there's no
/// separate identity/allocation to prune here, since this engine doesn't
/// intern strings).
fn reference_is_uniquely_held(value: &LuaValue) -> bool {
    match value {
        LuaValue::Table(rc) => Rc::strong_count(rc) == 1,
        LuaValue::Closure(rc) => Rc::strong_count(rc) == 1,
        LuaValue::Native(rc) => Rc::strong_count(rc) == 1,
        LuaValue::GMatchIterator(rc) => Rc::strong_count(rc) == 1,
        // A suspended coroutine owns a persistent `Once` continuation in
        // its saved frame stack.  That continuation retains the coroutine's
        // runtime handle while it is suspended, so unlike the other
        // reference values a weak-table-only thread/wrapper has two strong
        // references here: the table slot and that implementation detail.
        // A live Lua reference adds at least one more.  Treat the former as
        // weak-only, otherwise a `__mode = "v"` cache can keep every yielded
        // `coroutine.wrap` result alive forever.
        LuaValue::Thread(rc) => Rc::strong_count(rc) <= 2,
        LuaValue::CoroutineWrapper(rc) => Rc::strong_count(rc) <= 2,
        _ => false,
    }
}

fn key_is_uniquely_held(key: &LuaKey) -> bool {
    match key {
        LuaKey::Table(rc) => Rc::strong_count(rc) == 1,
        LuaKey::Closure(rc) => Rc::strong_count(rc) == 1,
        LuaKey::Native(rc) => Rc::strong_count(rc) == 1,
        LuaKey::GMatchIterator(rc) => Rc::strong_count(rc) == 1,
        LuaKey::Thread(rc) => Rc::strong_count(rc) == 1,
        LuaKey::CoroutineWrapper(rc) => Rc::strong_count(rc) == 1,
        _ => false,
    }
}

/// Pointer identity of `value`, if it's a cycle-collector candidate type
/// (`Table`/`Closure` - the only reference types that can themselves hold an
/// `Rc` to another candidate and thus participate in a cycle). Used by
/// `LuaRuntime::collect_cycles` to build inter-candidate edges; the pointer
/// value is only ever used as a `HashMap`/`HashSet` key against other
/// candidates' own `Rc::as_ptr`, never dereferenced.
pub(super) fn candidate_ptr(value: &LuaValue) -> Option<usize> {
    match value {
        LuaValue::Table(rc) => Some(Rc::as_ptr(rc) as usize),
        LuaValue::Closure(rc) => Some(Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

pub(super) fn candidate_key_ptr(key: &LuaKey) -> Option<usize> {
    match key {
        LuaKey::Table(rc) => Some(Rc::as_ptr(rc) as usize),
        LuaKey::Closure(rc) => Some(Rc::as_ptr(rc) as usize),
        _ => None,
    }
}

/// Removes weakly-held entries from a single table, per its own `__mode`.
/// Array-part removals become `Nil` (removing an array slot outright would
/// shift every later index); hash-part removals drop the entry entirely,
/// matching how `LuaTable::set` already treats a `Nil` value as a deletion.
pub(super) fn prune_weak_table(table: &mut LuaTable, weak_keys: bool, weak_values: bool) {
    if weak_values {
        let mut pruned_any = false;
        for slot in table.array.iter_mut() {
            if reference_is_uniquely_held(slot) {
                *slot = LuaValue::Nil;
                pruned_any = true;
            }
        }
        if pruned_any {
            table.recompute_array_border();
        }
    }
    if weak_keys || weak_values {
        table.hash.retain(|key, value| {
            !((weak_keys && key_is_uniquely_held(key))
                || (weak_values && reference_is_uniquely_held(value)))
        });
    }
    table.version = table.version.wrapping_add(1);
}

impl LuaKey {
    fn value(&self) -> LuaValue {
        match self {
            Self::Bool(value) => LuaValue::Bool(*value),
            Self::Integer(value) => LuaValue::Integer(*value),
            Self::Float(value) => LuaValue::Float(f64::from_bits(*value)),
            Self::String(value) => LuaValue::String(value.clone()),
            Self::Table(value) => LuaValue::Table(value.clone()),
            Self::CanonicalTable(value) => LuaValue::CanonicalTable(value.clone()),
            Self::Closure(value) => LuaValue::Closure(value.clone()),
            Self::NativeFunction(value) => LuaValue::NativeFunction(*value),
            Self::Native(value) => LuaValue::Native(value.clone()),
            Self::RegisteredNative(value) => LuaValue::RegisteredNative(*value),
            Self::CFunction(value) => LuaValue::CFunction(value.clone()),
            Self::GMatchIterator(value) => LuaValue::GMatchIterator(value.clone()),
            Self::Thread(value) => LuaValue::Thread(value.clone()),
            Self::CoroutineWrapper(value) => LuaValue::CoroutineWrapper(value.clone()),
            Self::Userdata(value) => LuaValue::Userdata(value.clone()),
            Self::LightUserdata(value) => LuaValue::LightUserdata(*value),
        }
    }
}
