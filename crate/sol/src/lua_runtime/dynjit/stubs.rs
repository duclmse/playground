//! Runtime entry points `DynJit`-compiled code calls out to for anything
//! that isn't worth (or isn't safe to) inline as raw Cranelift IR - slow
//! paths, allocation, field/global access, and (from Work item 5 onward)
//! the park-before-call protocol itself.
//!
//! Registered once, in `DynJit::new`, via `JITBuilder::symbol` - mirrors
//! `crate::jit::Jit::new`'s own symbol-registration block
//! (`crate::jit`'s module, `with_flags`/`symbol` calls), but against a
//! disjoint symbol namespace: these stubs are keyed to `LuaRuntime`/
//! `sol_core::Heap`, not the typed tier's own object model, so the two
//! `JITModule` instances never need to share or disambiguate names.

use cranelift_codegen::ir::{types, AbiParam, Signature};
use cranelift_jit::JITBuilder;
use cranelift_module::{FuncId, Linkage, Module};
use sol_core::{Value, ValueTag};

use crate::ast::BinaryOp;
use crate::lua_runtime::frame::{BinaryResolution, LuaFrame};
use crate::lua_runtime::util::float_for_limit;
use crate::lua_runtime::LuaRuntime;

use super::abi::WORD;

/// Registers every stub symbol `lower.rs` is allowed to reference by name.
pub fn register(builder: &mut JITBuilder) {
    builder.symbol("dynjit_safepoint", dynjit_safepoint as *const u8);
    builder.symbol("dynjit_for_prep", dynjit_for_prep as *const u8);
    builder.symbol("dynjit_for_loop", dynjit_for_loop as *const u8);
    builder.symbol("dynjit_binary", dynjit_binary as *const u8);
}

/// Every stub's `FuncId` within `DynJit`'s own `JITModule`, declared once in
/// `DynJit::new` (mirrors `crate::codegen::RuntimeFuncs`/`declare_runtime`'s
/// own shape) and reused via `Module::declare_func_in_func` by every
/// `lower_proto` call rather than re-declared per compiled `Proto`.
pub(super) struct StubFuncs {
    pub(super) safepoint: FuncId,
    pub(super) for_prep: FuncId,
    pub(super) for_loop: FuncId,
    pub(super) binary: FuncId,
}

pub(super) fn declare(module: &mut dyn Module) -> Result<StubFuncs, String> {
    let call_conv = module.target_config().default_call_conv;

    let mut safepoint_sig = Signature::new(call_conv);
    safepoint_sig.params.push(AbiParam::new(WORD));
    safepoint_sig.params.push(AbiParam::new(WORD));
    safepoint_sig.returns.push(AbiParam::new(types::I64));
    let safepoint = module
        .declare_function("dynjit_safepoint", Linkage::Import, &safepoint_sig)
        .map_err(|e| e.to_string())?;

    let mut for_prep_sig = Signature::new(call_conv);
    for_prep_sig.params.push(AbiParam::new(WORD));
    for_prep_sig.params.push(AbiParam::new(types::I64));
    for_prep_sig.returns.push(AbiParam::new(types::I64));
    let for_prep = module
        .declare_function("dynjit_for_prep", Linkage::Import, &for_prep_sig)
        .map_err(|e| e.to_string())?;

    let mut for_loop_sig = Signature::new(call_conv);
    for_loop_sig.params.push(AbiParam::new(WORD));
    for_loop_sig.params.push(AbiParam::new(WORD));
    for_loop_sig.params.push(AbiParam::new(WORD));
    for_loop_sig.params.push(AbiParam::new(types::I64));
    for_loop_sig.returns.push(AbiParam::new(types::I64));
    let for_loop = module
        .declare_function("dynjit_for_loop", Linkage::Import, &for_loop_sig)
        .map_err(|e| e.to_string())?;

    let mut binary_sig = Signature::new(call_conv);
    binary_sig.params.push(AbiParam::new(WORD)); // rt
    binary_sig.params.push(AbiParam::new(WORD)); // regs
    binary_sig.params.push(AbiParam::new(types::I64)); // dst
    binary_sig.params.push(AbiParam::new(types::I64)); // left
    binary_sig.params.push(AbiParam::new(types::I64)); // right
    binary_sig.params.push(AbiParam::new(types::I64)); // op
    binary_sig.returns.push(AbiParam::new(types::I64));
    let binary = module
        .declare_function("dynjit_binary", Linkage::Import, &binary_sig)
        .map_err(|e| e.to_string())?;

    Ok(StubFuncs {
        safepoint,
        for_prep,
        for_loop,
        binary,
    })
}

fn value_as_f64(v: Value) -> Option<f64> {
    match v.tag() {
        ValueTag::Integer => v.as_integer().map(|i| i as f64),
        ValueTag::Float => v.as_float(),
        _ => None,
    }
}

/// A loop backward-branch's safepoint call (`Instr::Jump`/`JumpIfFalse`/
/// `JumpIfTrue` with a negative delta) - the native-code equivalent of one
/// `LuaRuntime::tick` call per `tick`-interval for a pure compute loop with
/// no other call site. `frame.regs` must already be in sync (true
/// throughout item 3's eligible bodies, which never mutate a register to
/// hold a value the heap doesn't already know about - see `dynjit`'s module
/// doc). Returns `0` to continue, `1` to signal the caller must deopt
/// (budget exhausted; the interpreter re-raises the exact right error from
/// the resumed `pc`).
pub extern "C" fn dynjit_safepoint(rt: *mut LuaRuntime, frame: *mut LuaFrame) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame = unsafe { &*frame };
    match rt.jit_safepoint(frame) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// `Instr::ForPrep`'s full coercion/bounds logic (mirrors
/// `dispatch_step`'s own `Instr::ForPrep` arm, `lua_runtime/dispatch/bytecode.rs`),
/// restricted to the registers a numeric `for` loop's hidden
/// `[start, stop, step, var]` block occupies (`base..base+4`), none of
/// which are ever captured in an item-3-eligible `Proto` (captured
/// registers disqualify a `Proto` from promotion until item 4) - so every
/// write here is a plain slot write, with no `reg_set_fresh` cell
/// allocation to charge.
///
/// Returns `0` to deopt (a non-numeric bound, or a zero step - let the
/// interpreter raise the exact right error message), `1` if the loop body
/// should run (fall through), or `2` if the loop never runs (jump past it).
pub extern "C" fn dynjit_for_prep(regs: *mut Value, base: i64) -> i64 {
    let base = base as usize;
    unsafe {
        let start = *regs.add(base);
        let stop = *regs.add(base + 1);
        let step = *regs.add(base + 2);
        if start.tag() == ValueTag::Integer && step.tag() == ValueTag::Integer {
            let start_i = start.as_integer().unwrap();
            let step_i = step.as_integer().unwrap();
            if step_i == 0 {
                return 0;
            }
            let stop_i = match stop.tag() {
                ValueTag::Integer => stop.as_integer(),
                ValueTag::Float => float_for_limit(stop.as_float().unwrap(), step_i > 0),
                _ => return 0,
            };
            let Some(stop_i) = stop_i else {
                return 2;
            };
            let cont = if step_i > 0 {
                start_i <= stop_i
            } else {
                start_i >= stop_i
            };
            if !cont {
                return 2;
            }
            *regs.add(base + 1) = Value::integer(stop_i);
            *regs.add(base + 3) = Value::integer(start_i);
            1
        } else {
            let (Some(start_f), Some(stop_f), Some(step_f)) =
                (value_as_f64(start), value_as_f64(stop), value_as_f64(step))
            else {
                return 0;
            };
            if step_f == 0.0 {
                return 0;
            }
            *regs.add(base) = Value::float(start_f);
            *regs.add(base + 1) = Value::float(stop_f);
            *regs.add(base + 2) = Value::float(step_f);
            let cont = if step_f > 0.0 {
                start_f <= stop_f
            } else {
                start_f >= stop_f
            };
            if !cont {
                return 2;
            }
            *regs.add(base + 3) = Value::float(start_f);
            1
        }
    }
}

/// `Instr::ForLoop`'s advance/bounds-check logic (mirrors `dispatch_step`'s
/// own `Instr::ForLoop` arm). Unlike `dynjit_for_prep`, a continuing
/// iteration also charges one `jit_safepoint` call - the per-iteration
/// cadence a loop backward branch gets everywhere else in item 3's
/// lowering (see `dynjit_safepoint`'s own doc).
///
/// Returns `0` to deopt (non-numeric control registers, or integer
/// overflow stepping past the loop; also budget exhaustion from the
/// trailing safepoint - any of these are correctly handled by resuming the
/// interpreter from this exact `ForLoop` instruction, which redoes the same
/// check and either raises the right error or (for a transient budget
/// trip) simply continues), `1` if the loop has ended (fall through), or
/// `2` if the loop continues (jump back by `delta`).
pub extern "C" fn dynjit_for_loop(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, base: i64) -> i64 {
    let base = base as usize;
    unsafe {
        let current = *regs.add(base);
        match current.tag() {
            ValueTag::Float => {
                let (Some(stop), Some(step)) = (
                    value_as_f64(*regs.add(base + 1)),
                    value_as_f64(*regs.add(base + 2)),
                ) else {
                    return 0;
                };
                let next = current.as_float().unwrap() + step;
                let cont = if step > 0.0 { next <= stop } else { next >= stop };
                if !cont {
                    return 1;
                }
                *regs.add(base) = Value::float(next);
                *regs.add(base + 3) = Value::float(next);
            }
            ValueTag::Integer => {
                let (Some(stop), Some(step)) = (
                    (*regs.add(base + 1)).as_integer(),
                    (*regs.add(base + 2)).as_integer(),
                ) else {
                    return 0;
                };
                let Some(next) = current.as_integer().unwrap().checked_add(step) else {
                    return 1;
                };
                let cont = if step > 0 { next <= stop } else { next >= stop };
                if !cont {
                    return 1;
                }
                *regs.add(base) = Value::integer(next);
                *regs.add(base + 3) = Value::integer(next);
            }
            _ => return 0,
        }
    }
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    match rt.jit_safepoint(frame_ref) {
        Ok(()) => 2,
        Err(_) => 0,
    }
}

/// Decodes `lower.rs`'s `op as u8 as i64` encoding (`BinaryOp` is
/// `#[repr(u8)]`, see `crate::ast`) back into the enum, in declaration
/// order.
fn binary_op_from_u8(op: u8) -> BinaryOp {
    use BinaryOp::*;
    const OPS: [BinaryOp; 21] = [
        Add, Sub, Mul, Div, Mod, FloorDiv, Pow, Concat, BitAnd, BitOr, BitXor, Shl, Shr, Eq,
        NotEq, Lt, Le, Gt, Ge, And, Or,
    ];
    OPS[op as usize]
}

/// `Instr::Binary`'s general-case stub - everything `IntegerBinary` doesn't
/// inline (string/float operands, comparisons, bitwise, and any operator
/// that might need a metamethod). Reuses `LuaRuntime::binary_resolve`
/// (`dispatch.rs`), the exact same logic `dispatch_step`'s own `Instr::Binary`
/// arm calls, rather than reimplementing Lua's coercion/metamethod-lookup
/// rules in raw IR.
///
/// Deliberately excludes `BinaryOp::Concat`: its primitive (non-metamethod)
/// result is a freshly heap-allocated Lua string, and item 3's whole
/// native-register design (see `dynjit`'s module doc and `lower.rs`'s own
/// doc comment) depends on native code never fabricating a new heap
/// reference - `frame.regs` is only re-synced from the native `regs` array
/// once the whole native call returns, not per-stub-call, so a fresh
/// reference written here would be invisible to a GC root walk triggered by
/// any later stub call in the same native invocation. Deopting for `Concat`
/// instead costs nothing beyond an extra interpreter step for that one
/// instruction and keeps the invariant exact rather than approximate.
///
/// Every other operator's primitive result is a scalar (`Bool`/`Integer`/
/// `Float`) - safe to write directly into `regs[dst]` with no rooting
/// concern. Returns `1` on a primitive result, `0` to deopt (a metamethod
/// call is needed, the operands don't coerce, or the operation itself
/// errors) - the interpreter redoes the exact same `Instr::Binary` from
/// scratch in every `0` case, which is correct precisely because nothing
/// was written to any register before that point.
pub extern "C" fn dynjit_binary(
    rt: *mut LuaRuntime,
    regs: *mut Value,
    dst: i64,
    left: i64,
    right: i64,
    op: i64,
) -> i64 {
    let op = binary_op_from_u8(op as u8);
    if matches!(op, BinaryOp::Concat) {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    let (left_v, right_v) = unsafe { (*regs.add(left as usize), *regs.add(right as usize)) };
    let Ok(left_lv) = rt.decode_value(left_v) else {
        return 0;
    };
    let Ok(right_lv) = rt.decode_value(right_v) else {
        return 0;
    };
    match rt.binary_resolve(op, left_lv, right_lv) {
        Ok(BinaryResolution::Value(result)) => match rt.encode_value(&result) {
            Ok(encoded) => {
                unsafe { *regs.add(dst as usize) = encoded };
                1
            }
            Err(_) => 0,
        },
        Ok(BinaryResolution::Call { .. }) | Err(_) => 0,
    }
}
