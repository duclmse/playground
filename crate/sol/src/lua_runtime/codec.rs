//! Bidirectional `LuaValue` <-> `sol_core::Value` codec, used wherever a
//! value crosses into or out of canonical heap-owned storage (table entries,
//! upvalue cells, thread stacks) - see
//! `docs/features/table-closure-coroutine-cutover.md` §2.
//!
//! `Table`/`Closure`/`Thread`/`CoroutineWrapper`/`GMatchIterator` already
//! *are* a `Copy` `ObjectId` newtype, and `String`/`CanonicalTable`/
//! `Userdata`/`CFunction` already carry one behind an `Rc<CanonicalObjectRoot>`
//! - encoding every one of those is a bare `Value::object(id)`. Only
//! `NativeFunction`, `LightUserdata`, and `RegisteredNative` have no
//! `ObjectId` of their own; each is allocated onto
//! `HeapObject::NativeCallable` (and memoized, so repeated encodes of the
//! same source value keep one canonical identity) the first time it is
//! encoded. Decoding a bare `ObjectId` recovers which of these three a
//! `NativeCallable` object represents by its reserved `provider`: `0` for
//! `NativeFunction`, `LEGACY_STATE_PROVIDER` (disambiguated by `function`)
//! for `GMatchIterator`/`CoroutineWrapper`, the runtime's own
//! `light_userdata_provider` for `LightUserdata`, and a
//! `registered_native_providers`-reserved provider for `RegisteredNative`;
//! any other provider is an ordinary `CFunction`.

use sol_core::{HeapObject, NativeCallableId, ObjectId, Value, ValueTag};

use super::canonical::{COROUTINE_WRAPPER_FUNCTION, GMATCH_ITERATOR_FUNCTION, LEGACY_STATE_PROVIDER};
use super::*;

impl LuaRuntime {
    pub(super) fn encode_value(&self, value: &LuaValue) -> LuaResult<Value> {
        Ok(match value {
            LuaValue::Nil => Value::NIL,
            LuaValue::Bool(value) => Value::boolean(*value),
            LuaValue::Integer(value) => Value::integer(*value),
            LuaValue::Float(value) => Value::float(*value),
            LuaValue::String(value) => Value::object(value.object_id()),
            LuaValue::Table(value) => Value::object(value.object_id()),
            LuaValue::CanonicalTable(value) => Value::object(value.object_id()),
            LuaValue::Closure(value) => Value::object(value.object_id()),
            LuaValue::NativeFunction(function) => {
                Value::object(self.encode_native_function(*function))
            }
            LuaValue::Native(_) => {
                return Err(LuaError::new(
                    "internal error: an unconverted native bridge reached the canonical codec \
                     (init.rs must convert Native to RegisteredNative before installation)",
                ));
            }
            LuaValue::RegisteredNative(callable) => {
                Value::object(self.encode_registered_native(*callable))
            }
            LuaValue::CFunction(value) => Value::object(value.object_id()),
            LuaValue::GMatchIterator(value) => Value::object(value.object_id()),
            LuaValue::Thread(value) => Value::object(value.object_id()),
            LuaValue::CoroutineWrapper(value) => Value::object(value.object_id()),
            LuaValue::Userdata(value) => Value::object(value.object_id()),
            LuaValue::LightUserdata(bits) => Value::object(self.encode_light_userdata(*bits)),
        })
    }

    fn encode_native_function(&self, function: NativeFunction) -> ObjectId {
        let key = function as u32;
        if let Some(id) = self.native_function_objects.borrow().get(&key) {
            return *id;
        }
        // Provider zero is the portable Lua standard library, matching
        // `canonical::CanonicalAdapter::import_native_function`.
        let mut heap = self.canonical_heap.borrow_mut();
        let id = heap.alloc_native_callable(0, key, Vec::new());
        // Permanently rooted, like `c_registry` - this memoization table has
        // no eviction, so the object it names must never be collected out
        // from under it (a `collect_major_with_roots` sweep has no other way
        // to know this id is still meaningful).
        heap.add_root(Value::object(id));
        self.native_function_objects.borrow_mut().insert(key, id);
        id
    }

    fn encode_light_userdata(&self, bits: usize) -> ObjectId {
        if let Some(id) = self.c_light_userdata.borrow().get(&bits) {
            return *id;
        }
        let mut heap = self.canonical_heap.borrow_mut();
        let id = heap.alloc_native_callable(
            self.light_userdata_provider,
            0,
            vec![Value::integer(bits as i64)],
        );
        // See `encode_native_function`'s permanent-root comment.
        heap.add_root(Value::object(id));
        self.c_light_userdata.borrow_mut().insert(bits, id);
        self.c_light_userdata_reverse.borrow_mut().insert(id, bits);
        id
    }

    fn encode_registered_native(&self, callable: NativeCallableId) -> ObjectId {
        if let Some(id) = self.registered_native_objects.borrow().get(&callable) {
            return *id;
        }
        let existing = self
            .registered_native_providers
            .borrow()
            .get(&callable.provider)
            .copied();
        let real_provider = match existing {
            Some(provider) => provider,
            None => {
                let provider = self
                    .canonical_heap
                    .borrow_mut()
                    .reserve_native_provider()
                    .expect("native provider id space exhausted");
                self.registered_native_providers
                    .borrow_mut()
                    .insert(callable.provider, provider);
                self.registered_native_providers_reverse
                    .borrow_mut()
                    .insert(provider, callable.provider);
                provider
            }
        };
        let mut heap = self.canonical_heap.borrow_mut();
        let id = heap.alloc_native_callable(real_provider, callable.function, Vec::new());
        // See `encode_native_function`'s permanent-root comment.
        heap.add_root(Value::object(id));
        self.registered_native_objects
            .borrow_mut()
            .insert(callable, id);
        id
    }

    pub(super) fn decode_value(&self, value: Value) -> LuaResult<LuaValue> {
        Ok(match value.tag() {
            ValueTag::Nil => LuaValue::Nil,
            ValueTag::Boolean => LuaValue::Bool(value.as_bool().unwrap_or(false)),
            ValueTag::Integer => LuaValue::Integer(value.as_integer().unwrap_or(0)),
            ValueTag::Float => LuaValue::Float(value.as_float().unwrap_or(0.0)),
            ValueTag::Object => {
                let id = value
                    .as_object()
                    .expect("ValueTag::Object always carries an ObjectId");
                self.decode_object(id)?
            }
        })
    }

    fn decode_object(&self, id: ObjectId) -> LuaResult<LuaValue> {
        let (kind_summary, native_callable) = {
            let heap = self.canonical_heap.borrow();
            let object = heap
                .object(id)
                .map_err(|error| LuaError::new(format!("internal error: {error}")))?;
            match object {
                HeapObject::Table(_) => (DecodeKind::Table, None),
                HeapObject::Closure(_) => (DecodeKind::Closure, None),
                HeapObject::Thread(_) => (DecodeKind::Thread, None),
                HeapObject::String(_) => (DecodeKind::String, None),
                HeapObject::Userdata(_) => (DecodeKind::Userdata, None),
                HeapObject::NativeCallable(callable) => (
                    DecodeKind::NativeCallable,
                    Some((callable.provider, callable.function, callable.captures.first().copied())),
                ),
                HeapObject::Upvalue(_) | HeapObject::Error(_) => {
                    return Err(LuaError::new(
                        "internal error: attempted to decode a non-value heap object",
                    ));
                }
            }
        };
        Ok(match kind_summary {
            DecodeKind::Table => LuaValue::Table(TableRef::new(id)),
            DecodeKind::Closure => LuaValue::Closure(ClosureRef::new(id)),
            DecodeKind::Thread => LuaValue::Thread(ThreadRef::new(id)),
            DecodeKind::String => {
                LuaValue::String(CanonicalString::root_existing(self.canonical_heap.clone(), id))
            }
            DecodeKind::Userdata => LuaValue::Userdata(CanonicalUserdata::root_existing(
                self.canonical_heap.clone(),
                id,
            )),
            DecodeKind::NativeCallable => {
                let (provider, function, capture0) =
                    native_callable.expect("NativeCallable kind always carries its fields");
                self.decode_native_callable(id, provider, function, capture0)?
            }
        })
    }

    fn decode_native_callable(
        &self,
        id: ObjectId,
        provider: u32,
        function: u32,
        capture0: Option<Value>,
    ) -> LuaResult<LuaValue> {
        if provider == 0 {
            let native = NativeFunction::from_u32(function).ok_or_else(|| {
                LuaError::new("internal error: unknown NativeFunction discriminant")
            })?;
            return Ok(LuaValue::NativeFunction(native));
        }
        if provider == LEGACY_STATE_PROVIDER {
            return Ok(match function {
                GMATCH_ITERATOR_FUNCTION => LuaValue::GMatchIterator(GMatchRef::new(id)),
                COROUTINE_WRAPPER_FUNCTION => LuaValue::CoroutineWrapper(ThreadRef::new(id)),
                _ => {
                    return Err(LuaError::new(
                        "internal error: unknown legacy-state NativeCallable function id",
                    ));
                }
            });
        }
        if provider == self.light_userdata_provider {
            let bits = capture0
                .and_then(|value| value.as_integer())
                .ok_or_else(|| LuaError::new("internal error: malformed LightUserdata capture"))?;
            return Ok(LuaValue::LightUserdata(bits as usize));
        }
        if let Some(legacy_provider) = self
            .registered_native_providers_reverse
            .borrow()
            .get(&provider)
        {
            return Ok(LuaValue::RegisteredNative(NativeCallableId::new(
                *legacy_provider,
                function,
            )));
        }
        Ok(LuaValue::CFunction(CanonicalCFunction::root_existing(
            self.canonical_heap.clone(),
            id,
        )))
    }
}

/// The heap-borrow-scoped classification `decode_object` extracts before
/// releasing its `Heap` borrow, so the rest of decoding (which may itself
/// need to borrow the heap again, e.g. `root_existing`) never nests borrows.
enum DecodeKind {
    Table,
    Closure,
    Thread,
    String,
    Userdata,
    NativeCallable,
}
