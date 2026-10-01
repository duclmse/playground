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
use crate::lua_bytecode::Instr;
use crate::lua_runtime::frame::{BinaryResolution, IndexResolution, LuaFrame, SetIndexResolution};
use crate::lua_runtime::ic::IcKind;
use crate::lua_runtime::util::{float_for_limit, name_const};
use crate::lua_runtime::{LuaRuntime, LuaValue};

use super::abi::WORD;

/// Registers every stub symbol `lower.rs` is allowed to reference by name.
pub fn register(builder: &mut JITBuilder) {
    builder.symbol("dynjit_safepoint", dynjit_safepoint as *const u8);
    builder.symbol("dynjit_for_prep", dynjit_for_prep as *const u8);
    builder.symbol("dynjit_for_loop", dynjit_for_loop as *const u8);
    builder.symbol("dynjit_binary", dynjit_binary as *const u8);
    builder.symbol("dynjit_get_field", dynjit_get_field as *const u8);
    builder.symbol("dynjit_set_field", dynjit_set_field as *const u8);
    builder.symbol("dynjit_get_global", dynjit_get_global as *const u8);
    builder.symbol("dynjit_set_global", dynjit_set_global as *const u8);
    builder.symbol("dynjit_get_index", dynjit_get_index as *const u8);
    builder.symbol("dynjit_set_index", dynjit_set_index as *const u8);
    builder.symbol("dynjit_get_upval", dynjit_get_upval as *const u8);
    builder.symbol("dynjit_set_upval", dynjit_set_upval as *const u8);
    builder.symbol("dynjit_get_environment", dynjit_get_environment as *const u8);
    builder.symbol("dynjit_set_environment", dynjit_set_environment as *const u8);
    builder.symbol("dynjit_new_table", dynjit_new_table as *const u8);
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
    /// Work item 4's new stubs all share one signature,
    /// `(rt, frame, regs, pc) -> i64` - each re-derives its own instruction's
    /// operands from `frame.proto.instrs[pc]` rather than marshaling them
    /// across the FFI boundary (see `lower.rs`'s `lower_stub_instr`).
    pub(super) get_field: FuncId,
    pub(super) set_field: FuncId,
    pub(super) get_global: FuncId,
    pub(super) set_global: FuncId,
    pub(super) get_index: FuncId,
    pub(super) set_index: FuncId,
    pub(super) get_upval: FuncId,
    pub(super) set_upval: FuncId,
    pub(super) get_environment: FuncId,
    pub(super) set_environment: FuncId,
    pub(super) new_table: FuncId,
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

    let mut common_sig = Signature::new(call_conv);
    common_sig.params.push(AbiParam::new(WORD)); // rt
    common_sig.params.push(AbiParam::new(WORD)); // frame
    common_sig.params.push(AbiParam::new(WORD)); // regs
    common_sig.params.push(AbiParam::new(types::I64)); // pc
    common_sig.returns.push(AbiParam::new(types::I64));
    let declare_common = |module: &mut dyn Module, name: &str| -> Result<FuncId, String> {
        module
            .declare_function(name, Linkage::Import, &common_sig)
            .map_err(|e| e.to_string())
    };
    let get_field = declare_common(module, "dynjit_get_field")?;
    let set_field = declare_common(module, "dynjit_set_field")?;
    let get_global = declare_common(module, "dynjit_get_global")?;
    let set_global = declare_common(module, "dynjit_set_global")?;
    let get_index = declare_common(module, "dynjit_get_index")?;
    let set_index = declare_common(module, "dynjit_set_index")?;
    let get_upval = declare_common(module, "dynjit_get_upval")?;
    let set_upval = declare_common(module, "dynjit_set_upval")?;
    let get_environment = declare_common(module, "dynjit_get_environment")?;
    let set_environment = declare_common(module, "dynjit_set_environment")?;
    let new_table = declare_common(module, "dynjit_new_table")?;

    Ok(StubFuncs {
        safepoint,
        for_prep,
        for_loop,
        binary,
        get_field,
        set_field,
        get_global,
        set_global,
        get_index,
        set_index,
        get_upval,
        set_upval,
        get_environment,
        set_environment,
        new_table,
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

// Work item 4's stubs below all share one shape: re-derive the instruction's
// own operands from `frame.proto.instrs[pc]` (never marshaled across the
// FFI boundary - see this module's own doc comment and `lower.rs`'s
// `lower_stub_instr`), mirror the matching `dispatch_step` arm
// (`lua_runtime/dispatch/bytecode.rs`) as closely as possible, and deopt
// (`0`) on any IC-cache-miss/metamethod-call/error outcome - every one of
// those outcomes is reached before this stub has written anything, so
// deopting and letting the interpreter redo the instruction from scratch is
// always correct. `NewTable` is the only stub here that allocates; see its
// own doc comment for why it alone needs a dual write into both the native
// `regs` array and `frame.regs` (every other stub here only ever copies or
// mutates an already-independently-rooted heap reference, so it needs no
// special GC-safety handling beyond what item 3 already established).
//
// `NewClosure` and the captured-register (`frame.cells`) mechanism are
// deliberately out of scope for this increment - both are deferred to a
// follow-up within item 4 - so `is_eligible` still requires
// `captured_cell_count == 0`, which also means every register access below
// is a plain flat-slot access (cell-aware `reg_get`/`reg_set` would only
// matter for a captured register, and there are none in an eligible
// `Proto`).

/// `Instr::GetField`'s U8 inline-cache fast path, then `field_probe_raw`,
/// then the full `index_resolve` chain - mirrors `dispatch_step`'s own
/// `Instr::GetField` arm. Deopts (rather than replicating
/// `annotate_index_error`) on any error; the interpreter raises the exact
/// right annotated error itself when it redoes this instruction.
pub extern "C" fn dynjit_get_field(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::GetField(dst, base, name) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_get_field only called for Instr::GetField")
    };
    let name_bytes = name_const(proto, *name);
    let base_value = match rt.decode_value(unsafe { *regs.add(*base as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let base_table = match &base_value {
        LuaValue::Table(table) => Some(*table),
        _ => None,
    };
    let cached = match base_table {
        Some(table) => match rt.field_cache_get(
            &proto.field_cache[pc as usize],
            table,
            name_bytes.as_slice(),
            IcKind::Field,
        ) {
            Ok(value) => value,
            Err(_) => return 0,
        },
        None => None,
    };
    let value = match cached {
        Some(value) => value,
        None => {
            let key = LuaValue::String(rt.intern_str(name_bytes.as_slice()));
            let probed = match base_table {
                Some(table) => {
                    let Ok(encoded_key) = rt.encode_value(&key) else {
                        return 0;
                    };
                    match rt.field_probe_raw(
                        &proto.field_cache[pc as usize],
                        table,
                        encoded_key,
                        name_bytes.as_slice(),
                        IcKind::Field,
                    ) {
                        Ok(value) => value,
                        Err(_) => return 0,
                    }
                }
                None => None,
            };
            match probed {
                Some(value) => value,
                None => match rt.index_resolve(base_value, key) {
                    Ok(IndexResolution::Value(value)) => value,
                    Ok(IndexResolution::Call { .. }) | Err(_) => return 0,
                },
            }
        }
    };
    match rt.encode_value(&value) {
        Ok(encoded) => {
            unsafe { *regs.add(*dst as usize) = encoded };
            1
        }
        Err(_) => 0,
    }
}

/// `Instr::SetField`'s U8 inline-cache fast path, then `field_write_raw`,
/// then the full `set_index_resolve` chain - mirrors `dispatch_step`'s own
/// `Instr::SetField` arm.
pub extern "C" fn dynjit_set_field(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::SetField(base, name, src) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_set_field only called for Instr::SetField")
    };
    let name_bytes = name_const(proto, *name);
    let base_value = match rt.decode_value(unsafe { *regs.add(*base as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let value = match rt.decode_value(unsafe { *regs.add(*src as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let base_table = match &base_value {
        LuaValue::Table(table) => Some(*table),
        _ => None,
    };
    let cached = match base_table {
        Some(table) => match rt.field_cache_set(
            &proto.field_cache[pc as usize],
            table,
            name_bytes.as_slice(),
            value.clone(),
            IcKind::Field,
        ) {
            Ok(hit) => hit,
            Err(_) => return 0,
        },
        None => false,
    };
    if !cached {
        let key = LuaValue::String(rt.intern_str(name_bytes.as_slice()));
        let written = match base_table {
            Some(table) => {
                let Ok(encoded_key) = rt.encode_value(&key) else {
                    return 0;
                };
                match rt.field_write_raw(
                    &proto.field_cache[pc as usize],
                    table,
                    encoded_key,
                    name_bytes.as_slice(),
                    value.clone(),
                    IcKind::Field,
                ) {
                    Ok(hit) => hit,
                    Err(_) => return 0,
                }
            }
            None => false,
        };
        if !written {
            match rt.set_index_resolve(base_value, key, value, Some(frame_ref)) {
                Ok(SetIndexResolution::Done) => {}
                Ok(SetIndexResolution::Call { .. }) | Err(_) => return 0,
            }
        }
    }
    1
}

/// `Instr::GetGlobal` - mirrors `dispatch_step`'s own arm: a `has_base()`
/// scope (Sol's `require`-sandboxed modules) takes the plain `base`-chain
/// path directly; otherwise the same cache/probe/`index_resolve` fast path
/// as `dynjit_get_field`, against `global_cache` and the current `_ENV`
/// table.
pub extern "C" fn dynjit_get_global(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::GetGlobal(dst, name) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_get_global only called for Instr::GetGlobal")
    };
    let value = if frame_ref.globals.has_base() {
        frame_ref.globals.get(rt, name)
    } else {
        let base_value = frame_ref.globals.as_value();
        let base_table = match &base_value {
            LuaValue::Table(table) => Some(*table),
            _ => None,
        };
        let name_bytes = name.as_bytes();
        let cached = match base_table {
            Some(table) => match rt.field_cache_get(
                &proto.global_cache[pc as usize],
                table,
                name_bytes,
                IcKind::Global,
            ) {
                Ok(value) => value,
                Err(_) => return 0,
            },
            None => None,
        };
        match cached {
            Some(value) => value,
            None => {
                let key = LuaValue::String(rt.intern_str(name_bytes));
                let probed = match base_table {
                    Some(table) => {
                        let Ok(encoded_key) = rt.encode_value(&key) else {
                            return 0;
                        };
                        match rt.field_probe_raw(
                            &proto.global_cache[pc as usize],
                            table,
                            encoded_key,
                            name_bytes,
                            IcKind::Global,
                        ) {
                            Ok(value) => value,
                            Err(_) => return 0,
                        }
                    }
                    None => None,
                };
                match probed {
                    Some(value) => value,
                    None => match rt.index_resolve(base_value, key) {
                        Ok(IndexResolution::Value(value)) => value,
                        Ok(IndexResolution::Call { .. }) | Err(_) => return 0,
                    },
                }
            }
        }
    };
    match rt.encode_value(&value) {
        Ok(encoded) => {
            unsafe { *regs.add(*dst as usize) = encoded };
            1
        }
        Err(_) => 0,
    }
}

/// `Instr::SetGlobal` - mirrors `dispatch_step`'s own arm: `declare`
/// (unconditional `global` binding), then a `has_base()` scope's plain
/// `assign`, then the cache/probe/`set_index_resolve` fast path against
/// `global_cache`.
pub extern "C" fn dynjit_set_global(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::SetGlobal(name, src, constant, declare) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_set_global only called for Instr::SetGlobal")
    };
    let value = match rt.decode_value(unsafe { *regs.add(*src as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    if *declare {
        frame_ref.globals.define(rt, name, value, *constant);
    } else if frame_ref.globals.has_base() {
        if frame_ref.globals.assign(rt, name, value).is_err() {
            return 0;
        }
    } else {
        if frame_ref.globals.check_writable(name).is_err() {
            return 0;
        }
        let base_value = frame_ref.globals.as_value();
        let base_table = match &base_value {
            LuaValue::Table(table) => Some(*table),
            _ => None,
        };
        let name_bytes = name.as_bytes();
        let cached = match base_table {
            Some(table) => match rt.field_cache_set(
                &proto.global_cache[pc as usize],
                table,
                name_bytes,
                value.clone(),
                IcKind::Global,
            ) {
                Ok(hit) => hit,
                Err(_) => return 0,
            },
            None => false,
        };
        if !cached {
            let key = LuaValue::String(rt.intern_str(name_bytes));
            let written = match base_table {
                Some(table) => {
                    let Ok(encoded_key) = rt.encode_value(&key) else {
                        return 0;
                    };
                    match rt.field_write_raw(
                        &proto.global_cache[pc as usize],
                        table,
                        encoded_key,
                        name_bytes,
                        value.clone(),
                        IcKind::Global,
                    ) {
                        Ok(hit) => hit,
                        Err(_) => return 0,
                    }
                }
                None => false,
            };
            if !written {
                match rt.set_index_resolve(base_value, key, value, Some(frame_ref)) {
                    Ok(SetIndexResolution::Done) => {}
                    Ok(SetIndexResolution::Call { .. }) | Err(_) => return 0,
                }
            }
        }
    }
    1
}

/// `Instr::GetIndex` - a runtime key, so no inline cache; a plain
/// `index_resolve` call, mirroring `dispatch_step`'s own arm.
pub extern "C" fn dynjit_get_index(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::GetIndex(dst, base, index) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_get_index only called for Instr::GetIndex")
    };
    let base_value = match rt.decode_value(unsafe { *regs.add(*base as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let key = match rt.decode_value(unsafe { *regs.add(*index as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let value = match rt.index_resolve(base_value, key) {
        Ok(IndexResolution::Value(value)) => value,
        Ok(IndexResolution::Call { .. }) | Err(_) => return 0,
    };
    match rt.encode_value(&value) {
        Ok(encoded) => {
            unsafe { *regs.add(*dst as usize) = encoded };
            1
        }
        Err(_) => 0,
    }
}

/// `Instr::SetIndex` - the write counterpart to `dynjit_get_index`.
pub extern "C" fn dynjit_set_index(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::SetIndex(base, index, src) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_set_index only called for Instr::SetIndex")
    };
    let base_value = match rt.decode_value(unsafe { *regs.add(*base as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let key = match rt.decode_value(unsafe { *regs.add(*index as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let value = match rt.decode_value(unsafe { *regs.add(*src as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    match rt.set_index_resolve(base_value, key, value, Some(frame_ref)) {
        Ok(SetIndexResolution::Done) => 1,
        Ok(SetIndexResolution::Call { .. }) | Err(_) => 0,
    }
}

/// `Instr::GetUpval` - reads an already-resolved upvalue cell
/// (`frame.upvals`, unrelated to the captured-*register* `frame.cells`
/// mechanism this increment stays out of - see this module's own doc
/// comment above).
pub extern "C" fn dynjit_get_upval(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::GetUpval(dst, idx) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_get_upval only called for Instr::GetUpval")
    };
    let id = frame_ref.upvals[*idx as usize].get();
    let value = match rt.upvalue_get(id) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    match rt.encode_value(&value) {
        Ok(encoded) => {
            unsafe { *regs.add(*dst as usize) = encoded };
            1
        }
        Err(_) => 0,
    }
}

/// `Instr::SetUpval` - the write counterpart to `dynjit_get_upval`.
pub extern "C" fn dynjit_set_upval(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::SetUpval(idx, src) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_set_upval only called for Instr::SetUpval")
    };
    let value = match rt.decode_value(unsafe { *regs.add(*src as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    let id = frame_ref.upvals[*idx as usize].get();
    match rt.upvalue_set(id, value) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

/// `Instr::GetEnvironment` - reads the current `_ENV` value (always already
/// live via `frame.globals`/`closure_globals`, never a fresh allocation).
pub extern "C" fn dynjit_get_environment(
    rt: *mut LuaRuntime,
    frame: *mut LuaFrame,
    regs: *mut Value,
    pc: i64,
) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::GetEnvironment(dst) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_get_environment only called for Instr::GetEnvironment")
    };
    let value = frame_ref.globals.as_value();
    match rt.encode_value(&value) {
        Ok(encoded) => {
            unsafe { *regs.add(*dst as usize) = encoded };
            1
        }
        Err(_) => 0,
    }
}

/// `Instr::SetEnvironment` - rebinds `_ENV`'s shared cell in place; cannot
/// fail (`Globals::set_value` is an infallible `RefCell` overwrite).
pub extern "C" fn dynjit_set_environment(
    rt: *mut LuaRuntime,
    frame: *mut LuaFrame,
    regs: *mut Value,
    pc: i64,
) -> i64 {
    let rt = unsafe { &mut *rt };
    let frame_ref = unsafe { &*frame };
    let proto = &frame_ref.proto;
    let Instr::SetEnvironment(src) = &proto.instrs[pc as usize] else {
        unreachable!("dynjit_set_environment only called for Instr::SetEnvironment")
    };
    let value = match rt.decode_value(unsafe { *regs.add(*src as usize) }) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    frame_ref.globals.set_value(value);
    1
}

/// `Instr::NewTable` - the only stub in this increment that allocates.
/// `LuaRuntime::new_table` charges the allocation budget (the only point a
/// GC collection can be triggered) strictly *before* the table itself is
/// allocated, so the new table can never be collected before this stub has
/// a chance to root it - seeing this stub run at all means the table
/// already exists and needs rooting now. Unlike every other stub here, this
/// one therefore writes the new value into *both* the native `regs` array
/// (for subsequent native instructions in the same call) *and*
/// `frame.regs` directly (the actual GC root `frame_roots`/
/// `push_lua_frame_roots` walk) - a single dual write immediately after
/// allocation succeeds is sufficient; no other instruction in this
/// increment ever needs the same treatment because none of them allocate.
pub extern "C" fn dynjit_new_table(rt: *mut LuaRuntime, frame: *mut LuaFrame, regs: *mut Value, pc: i64) -> i64 {
    let rt = unsafe { &mut *rt };
    let dst = {
        let frame_ref = unsafe { &*frame };
        let Instr::NewTable(dst) = &frame_ref.proto.instrs[pc as usize] else {
            unreachable!("dynjit_new_table only called for Instr::NewTable")
        };
        *dst
    };
    let table = {
        let frame_ref = unsafe { &*frame };
        match rt.new_table(Some(frame_ref)) {
            Ok(table) => table,
            Err(_) => return 0,
        }
    };
    let value = LuaValue::Table(table);
    let encoded = match rt.encode_value(&value) {
        Ok(encoded) => encoded,
        Err(_) => return 0,
    };
    unsafe {
        let frame_mut = &mut *frame;
        frame_mut.regs[dst as usize] = value;
        *regs.add(dst as usize) = encoded;
    }
    1
}
