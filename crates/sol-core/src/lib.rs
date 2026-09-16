//! Portable semantic core for Sol's unified Lua-compatible runtime.
//!
//! This crate deliberately has no native-code-generator or host-OS
//! dependencies. U2 introduces the canonical value, object, root, and tracing
//! model here; later milestones move the existing interpreters onto it.

mod abi;
mod capabilities;
mod heap;
mod value;

pub use abi::{
    CallKind, CallOutcome, CallRequest, CallSite, ExecutablePrototype, ExecutionTier,
    FrameHeader, FrameState, FunctionArity, FunctionDescriptor, FunctionId, FunctionRegistry,
    FunctionRegistryError, NativeCallableId, PrototypeMetadata, SourceLocation, SourceMap,
    ValueCount, ValueCountError,
};
pub use capabilities::Capabilities;
pub use heap::{
    ClosureObject, Collection, CollectionKind, ErrorObject, FinalizerState, GcGeneration, Heap,
    HeapError, HeapObject, NativeCallableObject, ObjectHeader, ObjectKind, RootId, StackMap,
    StackMapError, TableKey, TableObject, ThreadObject, ThreadStatus, UpvalueObject,
    UserdataObject, WeakHandle,
};
pub use value::{BoundaryTypeError, BoundaryValue, ObjectId, ScalarKind, Value, ValueTag};
