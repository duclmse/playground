//! `sol-core` canonical-runtime plumbing kept beside `lua_runtime`'s main
//! module: the `PrototypeRegistry`/`CoroutineRegistry` side-tables
//! `LuaRuntime` owns, the typed/dynamic scalar boundary helpers, and the
//! native-callable provider constants `codec.rs` uses to encode
//! `GMatchIterator`/`CoroutineWrapper` identities. Prior to the
//! tables/closures/coroutines cutover (see
//! `docs/features/table-closure-coroutine-cutover.md`) this module also held
//! a one-way `CanonicalAdapter` that snapshotted the old `Rc`-backed
//! `LuaTable`/`LuaClosure` graph into a `sol_core::Heap`; now that
//! `LuaValue::Table`/`Closure`/`Thread` (etc.) are themselves canonical
//! handles (`TableRef`/`ClosureRef`/`ThreadRef`), there is nothing left to
//! snapshot and that adapter is gone - `codec.rs`'s `encode_value`/
//! `decode_value` are the closest surviving analog, and they operate
//! directly on the runtime's own heap rather than importing into a fresh one.

use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use sol_core::{ObjectId, Value};

use crate::lua_bytecode::Proto;

use super::{BridgeScalar, LuaCoroutine, ThreadRef};

pub(super) const LEGACY_STATE_PROVIDER: u32 = u32::MAX;
pub(super) const GMATCH_ITERATOR_FUNCTION: u32 = 0;

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

/// Boxes a typed/dynamic-boundary scalar into a canonical `Value` without
/// going through Lua's own dynamic representation. `LuaValue`'s own
/// `NativeBridge` boundary (`value.rs`) still does this conversion inline
/// rather than through here; kept and test-verified as the `sol_core`-facing
/// counterpart for when canonical `Value` itself crosses that boundary.
#[allow(dead_code)]
pub fn import_typed_scalar(kind: BridgeScalar, bits: u64) -> Value {
    sol_core::BoundaryValue::unboxed(kind, bits).boxed()
}

/// Reverses [`import_typed_scalar`], rejecting a `Value` whose runtime kind
/// does not match `kind`.
#[allow(dead_code)]
pub fn export_typed_scalar(kind: BridgeScalar, value: Value) -> Result<u64, CanonicalAdapterError> {
    sol_core::BoundaryValue::checked_unbox(value, kind)
        .ok()
        .and_then(sol_core::BoundaryValue::bits)
        .ok_or(CanonicalAdapterError::Unsupported("typed scalar mismatch"))
}

/// Interns `Rc<Proto>` identity into the portable `u32` registry key a
/// canonical closure's `ClosureObject::prototype` field expects, and
/// resolves it back to the prototype the interpreter needs to actually run
/// the closure's bytecode. One registry is meant to live for a `LuaRuntime`'s
/// entire lifetime (unlike `CanonicalAdapter::prototypes`, which is
/// per-import and only used for the one-way snapshot adapter above).
#[allow(dead_code)]
#[derive(Default)]
pub(super) struct PrototypeRegistry {
    by_identity: HashMap<usize, u32>,
    prototypes: Vec<Rc<Proto>>,
}

#[allow(dead_code)]
impl PrototypeRegistry {
    pub(super) fn intern(&mut self, proto: &Rc<Proto>) -> u32 {
        let identity = Rc::as_ptr(proto) as usize;
        if let Some(id) = self.by_identity.get(&identity) {
            return *id;
        }
        let id = u32::try_from(self.prototypes.len()).expect("more than u32::MAX Lua prototypes");
        self.by_identity.insert(identity, id);
        self.prototypes.push(proto.clone());
        id
    }

    pub(super) fn resolve(&self, id: u32) -> Option<&Rc<Proto>> {
        self.prototypes.get(id as usize)
    }
}

/// Owns each live `LuaCoroutine`'s executable state (registers, cells,
/// hooks, `dead_error`; see `docs/features/table-closure-coroutine-cutover.md`
/// §6/§11), addressed by its canonical `ThreadObject`'s own `ObjectId` rather
/// than by `Rc<LuaCoroutine>` refcounting. A registered entry's `frames`/
/// `body` count as GC roots only *conditionally*, once its own `ThreadObject`
/// id is independently reachable (`gc.rs`'s `frame_roots` builds the
/// `conditional_roots` map `sol_core::Heap::collect_major_with_conditional_roots`
/// takes from this registry's `entries()`) - so a coroutine kept alive only
/// by a reference cycle routed through its own frames is correctly
/// collected, not kept alive forever (task #13; see §11). Sweeping a
/// `Thread` object whose id has no entry here is a no-op, not an error - the
/// coroutine's canonical identity and its registry entry are removed
/// together.
#[allow(dead_code)]
#[derive(Default)]
pub(super) struct CoroutineRegistry {
    coroutines: HashMap<ObjectId, Rc<LuaCoroutine>>,
}

#[allow(dead_code)]
impl CoroutineRegistry {
    pub(super) fn insert(&mut self, thread: ThreadRef, coroutine: Rc<LuaCoroutine>) {
        self.coroutines.insert(thread.object_id(), coroutine);
    }

    pub(super) fn get(&self, thread: ThreadRef) -> Option<&Rc<LuaCoroutine>> {
        self.coroutines.get(&thread.object_id())
    }

    pub(super) fn remove(&mut self, thread: ThreadRef) -> Option<Rc<LuaCoroutine>> {
        self.coroutines.remove(&thread.object_id())
    }

    /// Every currently-registered coroutine, keyed by its own `ThreadObject`
    /// id - `gc.rs`'s `frame_roots` uses this key to build the
    /// `conditional_roots` map handed to
    /// `sol_core::Heap::collect_major_with_conditional_roots` (see this
    /// struct's own doc comment).
    pub(super) fn entries(&self) -> impl Iterator<Item = (ObjectId, &Rc<LuaCoroutine>)> {
        self.coroutines.iter().map(|(id, coroutine)| (*id, coroutine))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.coroutines.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.coroutines.len()
    }
}

#[cfg(test)]
mod tests {
    use sol_core::{Capabilities, Heap};

    use super::super::{ClosureRef, TableRef};
    use crate::lua_bytecode::Compiler;

    use super::*;

    #[test]
    fn typed_scalar_adapter_round_trips_without_boxing_inside_typed_code() {
        for (kind, bits) in [
            (BridgeScalar::I64, 42_u64),
            (BridgeScalar::F64, 3.5_f64.to_bits()),
            (BridgeScalar::Bool, 1_u64),
        ] {
            let value = import_typed_scalar(kind, bits);
            assert_eq!(export_typed_scalar(kind, value), Ok(bits));
        }
    }

    #[test]
    fn table_ref_wraps_the_matching_heap_table_methods() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let table = TableRef::alloc(&mut heap);
        let key = Value::integer(1);
        table.set(&mut heap, key, Value::integer(42)).unwrap();
        assert_eq!(table.get(&heap, key), Value::integer(42));
        assert_eq!(table.len(&heap), 1);

        let (next_key, next_value) = table.next(&mut heap, Value::NIL).unwrap().unwrap();
        assert_eq!((next_key, next_value), (key, Value::integer(42)));
        assert_eq!(table.next(&mut heap, next_key).unwrap(), None);

        let round_tripped = TableRef::new(table.object_id());
        assert_eq!(round_tripped, table);
    }

    #[test]
    fn closure_ref_allocates_over_already_allocated_upvalue_cells() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let environment_cell = heap.alloc_upvalue(Value::NIL, None);
        let closure = ClosureRef::alloc(&mut heap, 3, vec![environment_cell], 0).unwrap();
        let sol_core::HeapObject::Closure(object) = heap.object(closure.object_id()).unwrap()
        else {
            panic!("ClosureRef::alloc did not produce a canonical closure")
        };
        assert_eq!(object.prototype, 3);
        assert_eq!(object.environment, 0);
        assert_eq!(object.upvalues, vec![environment_cell]);
    }

    #[test]
    fn thread_ref_alloc_produces_a_fresh_suspended_thread_object() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let thread = ThreadRef::alloc(&mut heap);
        assert!(matches!(
            heap.object(thread.object_id()).unwrap(),
            sol_core::HeapObject::Thread(_)
        ));
        let other = ThreadRef::alloc(&mut heap);
        assert_ne!(thread, other);
    }

    #[test]
    fn prototype_registry_interns_by_identity_and_resolves_back() {
        let proto = |source: &[u8]| -> Rc<Proto> {
            let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).unwrap())
                .expect("test source parses");
            Compiler::compile_top_level(&program.functions[0]).expect("test source compiles")
        };
        let first = proto(b"function main() return 1 end");
        let second = proto(b"function main() return 2 end");

        let mut registry = PrototypeRegistry::default();
        let first_id = registry.intern(&first);
        let second_id = registry.intern(&second);
        assert_ne!(first_id, second_id);
        // Interning the same `Rc<Proto>` identity again must not mint a new id.
        assert_eq!(registry.intern(&first), first_id);

        assert!(Rc::ptr_eq(registry.resolve(first_id).unwrap(), &first));
        assert!(Rc::ptr_eq(registry.resolve(second_id).unwrap(), &second));
        assert!(registry.resolve(2).is_none());
    }

    #[test]
    fn coroutine_registry_addresses_coroutines_by_their_thread_objects_own_id() {
        let mut heap = Heap::new(Capabilities::SANDBOX);
        let first_thread = ThreadRef::alloc(&mut heap);
        let second_thread = ThreadRef::alloc(&mut heap);
        let coroutine = LuaCoroutine::main_thread();

        let mut registry = CoroutineRegistry::default();
        assert!(registry.is_empty());
        registry.insert(first_thread, coroutine.clone());
        assert_eq!(registry.len(), 1);
        assert!(registry.get(second_thread).is_none());
        assert!(Rc::ptr_eq(registry.get(first_thread).unwrap(), &coroutine));

        let removed = registry.remove(first_thread).unwrap();
        assert!(Rc::ptr_eq(&removed, &coroutine));
        assert!(registry.is_empty());
        assert!(registry.get(first_thread).is_none());
    }
}
