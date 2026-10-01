//! The calling convention shared by every dynamic-JIT stub and by compiled
//! `Proto` bodies themselves.
//!
//! Deliberately **not** a copy of the typed tier's uniform wrapper ABI
//! (`crate::codegen`'s `extern "C" fn(*const u64, i64) -> u64`), which exists
//! to erase *differing static signatures* across typed functions. Every
//! dynamic Lua value is already uniformly `sol_core::Value`-shaped, so that
//! indirection would only add overhead here. Instead, a `sol_core::Value`
//! (`tag: ValueTag`, `payload: u64`, both plain `u64`-sized fields,
//! `#[repr(C)]`, `Copy`) crosses the native/stub boundary as a pair of
//! `i64`s - one for the tag, one for the payload - since Cranelift IR has no
//! native notion of a two-field struct-by-value argument in this pinned
//! version's calling-convention support.
//!
//! Every stub takes an explicit `*mut LuaRuntime` and `*mut LuaFrame` first
//! (unlike the typed tier's apparently-global allocator entry points),
//! because `LuaRuntime` is a per-instance struct, not process-global state.

use cranelift_codegen::ir::types;
use cranelift_codegen::ir::Type;

/// The Cranelift IR type used for both halves of a `sol_core::Value` pair
/// (tag and payload), and for the `*mut LuaRuntime` / `*mut LuaFrame`
/// pointer arguments every stub takes. First real use lands in item 3,
/// once `lower.rs` starts building real `FunctionBuilder` signatures.
#[allow(dead_code)]
pub const WORD: Type = types::I64;
