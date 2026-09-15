//! Portable semantic core for Sol's unified Lua-compatible runtime.
//!
//! This crate deliberately has no native-code-generator or host-OS
//! dependencies. U2 introduces the canonical value, object, root, and tracing
//! model here; later milestones move the existing interpreters onto it.

mod capabilities;
mod heap;
mod value;

pub use capabilities::Capabilities;
pub use heap::{
    ClosureObject, Collection, CollectionKind, ErrorObject, FinalizerState, GcGeneration, Heap,
    HeapError, HeapObject, NativeCallableObject, ObjectHeader, ObjectKind, RootId, StackMap,
    StackMapError, TableKey, TableObject, ThreadObject, ThreadStatus, UpvalueObject,
    UserdataObject, WeakHandle,
};
pub use value::{ObjectId, Value, ValueTag};
