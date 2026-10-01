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
pub const WORD: Type = types::I64;

/// `sol_core::Value`'s tag discriminants, as the raw `i64` immediates
/// Cranelift IR compares register tag words against (`ValueTag` is
/// `#[repr(u64)]` with these exact values - see `crate::lua_runtime::codec`'s
/// own reliance on the same layout). Kept here, not re-derived from
/// `ValueTag` via `as i64` at every call site, so every tag comparison in
/// `lower.rs` reads as a named constant instead of a magic number.
pub const TAG_NIL: i64 = 0;
pub const TAG_BOOLEAN: i64 = 1;
pub const TAG_INTEGER: i64 = 2;
pub const TAG_FLOAT: i64 = 3;

/// A compiled `Proto` body's native entry point signature (see `lower.rs`'s
/// module doc for the full calling convention this mirrors): takes the
/// owning `LuaRuntime`/`LuaFrame` (for stub calls that need them), a flat
/// `*mut sol_core::Value` register array matching `frame.regs` 1:1 in
/// length and order, and three output slots the callee fills in before
/// returning - `out_pc` (deopt resume point), `out_base`/`out_count`
/// (normal-return value range, as register indices into the same array).
/// Returns `1` for a normal return (read `out_base`/`out_count`) or `0` for
/// a deopt (read `out_pc`, then resume interpretation there via
/// `LuaRuntime::dispatch_step`) - see `LuaRuntime::run_native`
/// (`lua_runtime/dispatch.rs`), this signature's only caller.
pub type NativeFn = extern "C" fn(
    *mut crate::lua_runtime::LuaRuntime,
    *mut crate::lua_runtime::frame::LuaFrame,
    *mut sol_core::Value,
    *mut i64,
    *mut i64,
    *mut i64,
) -> i64;
