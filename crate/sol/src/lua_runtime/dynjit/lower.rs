//! Per-`Instr` Cranelift IR lowering for the dynamic JIT.
//!
//! Work item 3: a call-free, capture-free `Proto` body compiles to one
//! Cranelift function, one basic block per bytecode `pc` (`blocks[pc]`,
//! `blocks[0]` the entry), with every register access a direct `load`/
//! `store` against the native `*mut sol_core::Value` array `lower_proto`'s
//! own signature takes (see `abi::NativeFn`'s doc) at that register's fixed
//! `index * 16` byte offset - never a Cranelift `Variable`, so there is no
//! SSA phi-resolution dependency on block-sealing order; every block is
//! simply sealed once, after the whole function body has been built.
//!
//! Every exit this item's instruction set can produce - a normal `Return`,
//! or anything native code can't/shouldn't resolve itself (a `Neg`/`BitNot`/
//! `IntegerBinary` guard failure, an unconditional `Binary` deopt, a
//! `ForPrep`/`ForLoop` edge case the stub reports, or a safepoint budget
//! trip) - goes through one of exactly two paths out of the compiled
//! function, both via the three `out_*` pointer parameters and the `i64`
//! return value: `1` (normal return: `*out_base`/`*out_count` name the
//! already-in-place result range) or `0` (deopt: `*out_pc` names the
//! bytecode `pc` to resume interpreting from). `LuaRuntime::run_native`
//! (`lua_runtime/dispatch.rs`), this function's only caller, handles both.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, FuncRef, InstBuilder, MemFlagsData, Signature, TrapCode,
    Value as ClifValue,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::JITModule;
use cranelift_module::{FuncId, Linkage, Module};

use crate::ast::BinaryOp;
use crate::lua_bytecode::{Const, Instr, Proto};
use sol_core::ValueCount;

use super::abi::{TAG_BOOLEAN, TAG_FLOAT, TAG_INTEGER, TAG_NIL, WORD};
use super::stubs::StubFuncs;

/// Real Lua/Sol bytecode compilation emits trailing scope-exit cleanup
/// (`DetachCell`/`LoadNil`, sometimes a second, dead `Return`) *after* the
/// last reachable `Return` in `proto.instrs` - that tail is never actually
/// executed (every control-flow path already exited via a real `Return`
/// before reaching it), but it is still physically present, so `lower_instr`
/// still builds a block for it. `fallthrough` traps instead of guessing a
/// resume target for that tail's own (nonexistent) `pc + 1` block, so a wrong
/// reachability assumption fails loudly instead of resuming the interpreter
/// at a bogus pc.
const TRAP_UNREACHABLE_TAIL: TrapCode = TrapCode::unwrap_user(1);

/// Whether `proto` is eligible for promotion. The captured-register *access*
/// machinery (`store_value`'s `sync_cell_out`, `lower_new_local`, the
/// `DetachCell` arm, and `stubs.rs`'s `dynjit_cell_set`/
/// `dynjit_cell_set_fresh`/`dynjit_detach_cell`) and `NewClosure` itself
/// (`dynjit_new_closure`) are both lowered now, so a `Proto` with
/// `captured_cell_count != 0` no longer needs excluding up front - every
/// instruction such a `Proto` can contain, including the `NewClosure` that
/// necessarily creates the capture (the only instruction that ever marks a
/// register captured, `lua_bytecode/mod.rs`'s `resolve`), has a real
/// lowering below, so the whitelist scan is sufficient on its own. Work item
/// 5 lifts the call-free/fixed-in-bounds-`Return`-only restriction items 3-4
/// enforced here: `Call`/`TailCall`/`TForCall`/`CloseSlots` always deopt to
/// the interpreter at their own `pc` (see `lower_instr`'s doc on those arms),
/// and an out-of-bounds or `Open`-count `Return` does the same - both reuse
/// the interpreter's own already-correct call/return/close machinery
/// unchanged rather than needing new native-side protocol, so neither needs
/// to disqualify the whole `Proto` any more.
pub(super) fn is_eligible(proto: &Proto) -> bool {
    if proto.instrs.is_empty() {
        return false;
    }
    proto.instrs.iter().all(|instr| match instr {
        Instr::LoadConst(_, k) => !matches!(proto.consts.get(*k as usize), Some(Const::Str(_))),
        Instr::LoadNil(_)
        | Instr::LoadBool(_, _)
        | Instr::Move(_, _)
        | Instr::NewLocal(_, _, _)
        | Instr::DetachCell(_)
        | Instr::Not(_, _)
        | Instr::Neg(_, _)
        | Instr::BitNot(_, _)
        | Instr::Binary(_, _, _, _)
        | Instr::IntegerBinary(_, _, _, _)
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_, _)
        | Instr::JumpIfTrue(_, _)
        | Instr::ForPrep(_, _)
        | Instr::ForLoop(_, _)
        | Instr::GetField(_, _, _)
        | Instr::SetField(_, _, _)
        | Instr::GetGlobal(_, _)
        | Instr::SetGlobal(_, _, _, _)
        | Instr::GetIndex(_, _, _)
        | Instr::SetIndex(_, _, _)
        | Instr::GetUpval(_, _)
        | Instr::SetUpval(_, _)
        | Instr::GetEnvironment(_)
        | Instr::SetEnvironment(_)
        | Instr::NewTable(_)
        | Instr::NewClosure(_, _)
        | Instr::Call(_, _, _)
        | Instr::TailCall(_, _)
        | Instr::TForCall(_, _)
        | Instr::TForLoop(_, _)
        | Instr::MarkClose(_, _)
        | Instr::CloseSlots(_)
        | Instr::Return(_, _) => true,
        _ => false,
    })
}

struct Lowerer<'a, 'b> {
    builder: FunctionBuilder<'b>,
    proto: &'a Proto,
    register_count: usize,
    rt: ClifValue,
    frame: ClifValue,
    regs: ClifValue,
    out_base: ClifValue,
    out_count: ClifValue,
    blocks: Vec<Block>,
    deopt_block: Block,
    extra_blocks: Vec<Block>,
    safepoint_ref: FuncRef,
    for_prep_ref: FuncRef,
    for_loop_ref: FuncRef,
    binary_ref: FuncRef,
    get_field_ref: FuncRef,
    set_field_ref: FuncRef,
    get_global_ref: FuncRef,
    set_global_ref: FuncRef,
    get_index_ref: FuncRef,
    set_index_ref: FuncRef,
    get_upval_ref: FuncRef,
    set_upval_ref: FuncRef,
    get_environment_ref: FuncRef,
    set_environment_ref: FuncRef,
    new_table_ref: FuncRef,
    new_closure_ref: FuncRef,
    mark_close_ref: FuncRef,
    cell_set_ref: FuncRef,
    cell_set_fresh_ref: FuncRef,
    detach_cell_ref: FuncRef,
}

impl<'a, 'b> Lowerer<'a, 'b> {
    fn reg_offset(reg: u16) -> i32 {
        (reg as i32) * 16
    }

    fn load_tag(&mut self, reg: u16) -> ClifValue {
        self.builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), self.regs, Self::reg_offset(reg))
    }

    fn load_payload(&mut self, reg: u16) -> ClifValue {
        self.builder.ins().load(
            types::I64,
            MemFlagsData::trusted(),
            self.regs,
            Self::reg_offset(reg) + 8,
        )
    }

    fn store_flat(&mut self, reg: u16, tag: ClifValue, payload: ClifValue) {
        let off = Self::reg_offset(reg);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), tag, self.regs, off);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), payload, self.regs, off + 8);
    }

    /// Raw flat store (`store_flat`) plus write-through cell sync
    /// (`sync_cell_out`) for any register `is_captured` - the ordinary write
    /// path used by every instruction except `Instr::NewLocal`'s captured-
    /// `dst` case (`lower_new_local`'s own doc comment: that one needs a
    /// *fresh* cell rather than a write-through of an existing one).
    fn store_value(&mut self, reg: u16, tag: ClifValue, payload: ClifValue) {
        self.store_flat(reg, tag, payload);
        self.sync_cell_out(reg);
    }

    /// Statically known at lowering time - `Proto::captured_registers` is
    /// resolved once, at compile time, from the source's own closure
    /// structure (`lua_bytecode/mod.rs`'s `resolve`/`mark_captured`), so this
    /// needs no runtime branch, just a lookup against the `Proto` this
    /// `Lowerer` is already compiling.
    fn is_captured(&self, reg: u16) -> bool {
        self.proto.captured_registers.get(reg as usize).copied().unwrap_or(false)
    }

    /// Write-through for a statically-capturable register: pushes whatever a
    /// flat store just wrote into `regs[reg]` into that register's cell, if
    /// it currently has one (`dynjit_cell_set`, which itself re-checks
    /// `frame.cells[reg]` at runtime and no-ops if absent - see its own doc
    /// comment for why a statically-captured register can still be
    /// cell-less in the `DetachCell`->`NewLocal` bracket). A no-op call for
    /// an uncaptured register never happens at all, short-circuited by
    /// `is_captured` below.
    fn sync_cell_out(&mut self, reg: u16) {
        if !self.is_captured(reg) {
            return;
        }
        let reg_arg = self.builder.ins().iconst(types::I64, reg as i64);
        self.builder
            .ins()
            .call(self.cell_set_ref, &[self.rt, self.frame, self.regs, reg_arg]);
    }

    fn store_const(&mut self, reg: u16, tag: i64, payload: i64) {
        let t = self.builder.ins().iconst(types::I64, tag);
        let p = self.builder.ins().iconst(types::I64, payload);
        self.store_value(reg, t, p);
    }

    fn copy_reg(&mut self, dst: u16, src: u16) {
        let t = self.load_tag(src);
        let p = self.load_payload(src);
        self.store_value(dst, t, p);
    }

    /// `LuaValue::truthy`'s native-register equivalent: false iff the tag is
    /// `Nil`, or the tag is `Boolean` and the payload is zero; a bare `i64`
    /// of `0`/`1` (not a Cranelift `b1`; this pinned version's `brif` takes
    /// an integer condition, matching every other truthiness test already in
    /// `codegen.rs`).
    fn truthy(&mut self, reg: u16) -> ClifValue {
        let tag = self.load_tag(reg);
        let payload = self.load_payload(reg);
        let is_nil = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_NIL);
        let is_bool = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_BOOLEAN);
        let payload_zero = self.builder.ins().icmp_imm_s(IntCC::Equal, payload, 0);
        let bool_false = self.builder.ins().band(is_bool, payload_zero);
        let falsy = self.builder.ins().bor(is_nil, bool_false);
        self.builder.ins().bxor_imm_s(falsy, 1)
    }

    fn new_block(&mut self) -> Block {
        let block = self.builder.create_block();
        self.extra_blocks.push(block);
        block
    }

    /// A block whose entire body is an unreachable trap - used wherever a
    /// branch target is a `pc + 1` block that doesn't exist because `pc` is
    /// in the unreachable tail `fallthrough`'s own doc comment describes.
    /// Unlike `fallthrough` (used where the missing target is a plain
    /// fall-through, so the trap can just replace the `jump`), a
    /// conditional-jump's false-branch target is itself a `Block` operand to
    /// `brif`, so it needs an actual block to point at, not a bare
    /// instruction - this builds one and restores the caller's current block
    /// before returning.
    fn trap_block(&mut self) -> Block {
        let current = self
            .builder
            .current_block()
            .expect("called while still building the branching instruction's own block");
        let block = self.new_block();
        self.builder.switch_to_block(block);
        self.builder.ins().trap(TRAP_UNREACHABLE_TAIL);
        self.builder.switch_to_block(current);
        block
    }

    fn pc_const(&mut self, pc: usize) -> ClifValue {
        self.builder.ins().iconst(types::I32, pc as i64)
    }

    /// `pc` as the `i64` call argument every item-4 stub takes (distinct from
    /// `pc_const`'s `i32`, which is only ever used as the deopt block's own
    /// resume-point parameter).
    fn pc_arg(&mut self, pc: usize) -> ClifValue {
        self.builder.ins().iconst(types::I64, pc as i64)
    }

    /// Shared lowering for every item-4 instruction: all of them share the
    /// `(rt, frame, regs, pc) -> i64` stub signature (`stubs.rs`'s own doc),
    /// re-deriving their operands from `frame.proto.instrs[pc]` on the Rust
    /// side rather than marshaling them across the FFI boundary - so lowering
    /// them is just "call the stub, deopt unless it reports success."
    /// `out_reg`: the register (if any) whose own write happens inside the
    /// stub itself - directly into `regs[reg]`, not through `store_value` -
    /// so a captured `out_reg`'s cell still needs an explicit `sync_cell_out`
    /// after the stub returns success (`GetField`/`GetGlobal`/`GetIndex`/
    /// `GetUpval`/`GetEnvironment`/`NewTable`; `None` for every stub that
    /// writes a table/global/upvalue slot rather than a register, or
    /// `MarkClose`, which writes no new value at all).
    fn lower_stub_instr(
        &mut self,
        pc: usize,
        func_ref: FuncRef,
        fallthrough_target: Option<Block>,
        out_reg: Option<u16>,
    ) {
        let pc_arg = self.pc_arg(pc);
        let call = self
            .builder
            .ins()
            .call(func_ref, &[self.rt, self.frame, self.regs, pc_arg]);
        let result = self.builder.inst_results(call)[0];
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, 1);
        let pcv = self.pc_const(pc);
        let cont = self.new_block();
        self.builder
            .ins()
            .brif(ok, cont, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(cont);
        if let Some(reg) = out_reg {
            self.sync_cell_out(reg);
        }
        self.fallthrough(fallthrough_target);
    }

    /// Emits a call to `dynjit_safepoint`; returns the block execution
    /// continues in once the budget check passes. On exhaustion, jumps
    /// straight to `deopt_block` with `pc` as the resume point - resuming
    /// interpretation here re-does the exact same safepoint check and either
    /// continues or raises the right error, so this is sound even though
    /// native code never reaches the backward-branch target in that case.
    fn emit_safepoint(&mut self, pc: usize) -> Block {
        let call = self.builder.ins().call(self.safepoint_ref, &[self.rt, self.frame]);
        let result = self.builder.inst_results(call)[0];
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, 0);
        let pcv = self.pc_const(pc);
        let cont = self.new_block();
        self.builder
            .ins()
            .brif(ok, cont, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        cont
    }

    fn lower_instr(&mut self, pc: usize) {
        let instr = self.proto.instrs[pc].clone();
        let fallthrough_target = self.blocks.get(pc + 1).copied();
        match &instr {
            Instr::LoadConst(dst, k) => {
                match &self.proto.consts[*k as usize] {
                    Const::Nil => self.store_const(*dst, TAG_NIL, 0),
                    Const::Bool(v) => self.store_const(*dst, TAG_BOOLEAN, *v as i64),
                    Const::Integer(v) => self.store_const(*dst, TAG_INTEGER, *v),
                    Const::Float(v) => self.store_const(*dst, TAG_FLOAT, v.to_bits() as i64),
                    Const::Str(_) => unreachable!("is_eligible excludes string constants"),
                }
                self.fallthrough(fallthrough_target);
            }
            Instr::LoadNil(dst) => {
                self.store_const(*dst, TAG_NIL, 0);
                self.fallthrough(fallthrough_target);
            }
            Instr::LoadBool(dst, v) => {
                self.store_const(*dst, TAG_BOOLEAN, *v as i64);
                self.fallthrough(fallthrough_target);
            }
            Instr::Move(dst, src) => {
                self.copy_reg(*dst, *src);
                self.fallthrough(fallthrough_target);
            }
            Instr::NewLocal(dst, src, _) => {
                self.lower_new_local(pc, *dst, *src, fallthrough_target);
            }
            Instr::DetachCell(reg) => {
                if self.is_captured(*reg) {
                    let reg_arg = self.builder.ins().iconst(types::I64, *reg as i64);
                    self.builder.ins().call(self.detach_cell_ref, &[self.frame, reg_arg]);
                }
                self.fallthrough(fallthrough_target);
            }
            Instr::Not(dst, src) => {
                let truthy = self.truthy(*src);
                let negated = self.builder.ins().bxor_imm_s(truthy, 1);
                let payload = self.builder.ins().uextend(types::I64, negated);
                let tag = self.builder.ins().iconst(types::I64, TAG_BOOLEAN);
                self.store_value(*dst, tag, payload);
                self.fallthrough(fallthrough_target);
            }
            Instr::Neg(dst, src) => {
                let tag = self.load_tag(*src);
                let is_int = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_INTEGER);
                let is_float = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_FLOAT);
                let is_numeric = self.builder.ins().bor(is_int, is_float);
                let pcv = self.pc_const(pc);
                let fast = self.new_block();
                self.builder
                    .ins()
                    .brif(is_numeric, fast, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
                self.builder.switch_to_block(fast);
                let payload = self.load_payload(*src);
                let int_result = self.builder.ins().ineg(payload);
                let float_bits = self.builder.ins().bitcast(types::F64, MemFlagsData::new(), payload);
                let negf = self.builder.ins().fneg(float_bits);
                let float_result = self.builder.ins().bitcast(types::I64, MemFlagsData::new(), negf);
                let result = self.builder.ins().select(is_int, int_result, float_result);
                let int_tag = self.builder.ins().iconst(types::I64, TAG_INTEGER);
                let float_tag = self.builder.ins().iconst(types::I64, TAG_FLOAT);
                let result_tag = self.builder.ins().select(is_int, int_tag, float_tag);
                self.store_value(*dst, result_tag, result);
                self.fallthrough(fallthrough_target);
            }
            Instr::BitNot(dst, src) => {
                let tag = self.load_tag(*src);
                let is_int = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_INTEGER);
                let pcv = self.pc_const(pc);
                let fast = self.new_block();
                self.builder
                    .ins()
                    .brif(is_int, fast, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
                self.builder.switch_to_block(fast);
                let payload = self.load_payload(*src);
                let result = self.builder.ins().bnot(payload);
                let tagc = self.builder.ins().iconst(types::I64, TAG_INTEGER);
                self.store_value(*dst, tagc, result);
                self.fallthrough(fallthrough_target);
            }
            Instr::Binary(op, dst, left, right) => {
                self.lower_binary(pc, *op, *dst, *left, *right, fallthrough_target);
            }
            // Note: `IntegerBinary`'s own `store_value` call (unchanged,
            // below) already picks up captured-`dst` write-through for free.
            Instr::IntegerBinary(op, dst, left, right) => {
                let left_tag = self.load_tag(*left);
                let right_tag = self.load_tag(*right);
                let left_ok = self.builder.ins().icmp_imm_s(IntCC::Equal, left_tag, TAG_INTEGER);
                let right_ok = self.builder.ins().icmp_imm_s(IntCC::Equal, right_tag, TAG_INTEGER);
                let both_ok = self.builder.ins().band(left_ok, right_ok);
                let pcv = self.pc_const(pc);
                let fast = self.new_block();
                self.builder
                    .ins()
                    .brif(both_ok, fast, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
                self.builder.switch_to_block(fast);
                let left_v = self.load_payload(*left);
                let right_v = self.load_payload(*right);
                let result = match op {
                    BinaryOp::Add => self.builder.ins().iadd(left_v, right_v),
                    BinaryOp::Sub => self.builder.ins().isub(left_v, right_v),
                    BinaryOp::Mul => self.builder.ins().imul(left_v, right_v),
                    _ => unreachable!("the U4 plan emits only integer add/sub/mul"),
                };
                let tagc = self.builder.ins().iconst(types::I64, TAG_INTEGER);
                self.store_value(*dst, tagc, result);
                self.fallthrough(fallthrough_target);
            }
            Instr::Jump(delta) => {
                self.lower_unconditional_jump(pc, *delta);
            }
            Instr::JumpIfFalse(reg, delta) => {
                self.lower_conditional_jump(pc, *reg, *delta, false, fallthrough_target);
            }
            Instr::JumpIfTrue(reg, delta) => {
                self.lower_conditional_jump(pc, *reg, *delta, true, fallthrough_target);
            }
            Instr::Return(base, count) => {
                // A fixed, in-bounds result range fits this ABI's contiguous
                // `[out_base, out_base + out_count)` native fast path; an
                // `Open` count or an out-of-bounds `Fixed` one needs the
                // interpreter's own Nil-padding/`top`-tracking `Return`
                // handling (`dispatch/bytecode.rs`'s own `Instr::Return` arm)
                // instead, so it deopts at this instruction's own `pc` - safe
                // and correct because nothing has been written yet.
                let in_bounds_fixed = match count {
                    ValueCount::Fixed(c) => (*base as usize) + (*c as usize) <= self.register_count,
                    ValueCount::Open => false,
                };
                if in_bounds_fixed {
                    let ValueCount::Fixed(count) = count else {
                        unreachable!("checked above")
                    };
                    let base_v = self.builder.ins().iconst(types::I64, *base as i64);
                    let count_v = self.builder.ins().iconst(types::I64, *count as i64);
                    self.builder
                        .ins()
                        .store(MemFlagsData::trusted(), base_v, self.out_base, 0);
                    self.builder
                        .ins()
                        .store(MemFlagsData::trusted(), count_v, self.out_count, 0);
                    let one = self.builder.ins().iconst(types::I64, 1);
                    self.builder.ins().return_(&[one]);
                } else {
                    let pcv = self.pc_const(pc);
                    self.builder.ins().jump(self.deopt_block, &[BlockArg::Value(pcv)]);
                }
            }
            Instr::ForPrep(base, delta) => {
                self.lower_for_prep(pc, *base, *delta);
            }
            Instr::ForLoop(base, delta) => {
                self.lower_for_loop(pc, *base, *delta);
            }
            Instr::GetField(dst, _, _) => {
                self.lower_stub_instr(pc, self.get_field_ref, fallthrough_target, Some(*dst));
            }
            Instr::SetField(_, _, _) => {
                self.lower_stub_instr(pc, self.set_field_ref, fallthrough_target, None);
            }
            Instr::GetGlobal(dst, _) => {
                self.lower_stub_instr(pc, self.get_global_ref, fallthrough_target, Some(*dst));
            }
            Instr::SetGlobal(_, _, _, _) => {
                self.lower_stub_instr(pc, self.set_global_ref, fallthrough_target, None);
            }
            Instr::GetIndex(dst, _, _) => {
                self.lower_stub_instr(pc, self.get_index_ref, fallthrough_target, Some(*dst));
            }
            Instr::SetIndex(_, _, _) => {
                self.lower_stub_instr(pc, self.set_index_ref, fallthrough_target, None);
            }
            Instr::GetUpval(dst, _) => {
                self.lower_stub_instr(pc, self.get_upval_ref, fallthrough_target, Some(*dst));
            }
            Instr::SetUpval(_, _) => {
                self.lower_stub_instr(pc, self.set_upval_ref, fallthrough_target, None);
            }
            Instr::GetEnvironment(dst) => {
                self.lower_stub_instr(pc, self.get_environment_ref, fallthrough_target, Some(*dst));
            }
            Instr::SetEnvironment(_) => {
                self.lower_stub_instr(pc, self.set_environment_ref, fallthrough_target, None);
            }
            Instr::NewTable(dst) => {
                self.lower_stub_instr(pc, self.new_table_ref, fallthrough_target, Some(*dst));
            }
            Instr::NewClosure(dst, _) => {
                self.lower_stub_instr(pc, self.new_closure_ref, fallthrough_target, Some(*dst));
            }
            Instr::MarkClose(_, _) => {
                self.lower_stub_instr(pc, self.mark_close_ref, fallthrough_target, None);
            }
            // `Call`/`TailCall`/`TForCall`/`CloseSlots` can all invoke
            // arbitrary Lua (a callee, a `__close` metamethod) and none of
            // that arbitrary execution can safely run while this native
            // frame's non-allocating registers are live only in the native
            // `regs` array and not yet reflected in `frame.regs` (see
            // `dynjit`'s module doc on the GC-rooting invariant item 3
            // established). Rather than building new park-before-call
            // machinery to keep that invariant across a nested call, these
            // unconditionally deopt to the interpreter at their own `pc` -
            // `run_native`'s universal regs flush (`dispatch.rs`) means
            // `frame.regs` is already fully correct by the time the
            // interpreter resumes, so this reuses 100% of its existing,
            // already-correct call/return/close/error/yield handling with no
            // new stub or protocol. The cost is a performance cliff right
            // after any such instruction (native code can never resume after
            // one in the same invocation - see this module's own follow-up
            // note in `is_eligible`'s doc) rather than resuming natively once
            // an ordinary call returns; that tradeoff is deliberate for this
            // baseline tier, not an oversight.
            Instr::Call(_, _, _) | Instr::TailCall(_, _) | Instr::TForCall(_, _) | Instr::CloseSlots(_) => {
                let pcv = self.pc_const(pc);
                self.builder.ins().jump(self.deopt_block, &[BlockArg::Value(pcv)]);
            }
            Instr::TForLoop(base, delta) => {
                self.lower_tfor_loop(pc, *base, *delta, fallthrough_target);
            }
            _ => unreachable!("is_eligible excludes every other Instr variant"),
        }
    }

    /// `Instr::NewLocal`'s native lowering: copies `src`'s current value into
    /// `dst`'s flat slot via `store_flat` directly (never through
    /// `store_value`'s write-through cell sync - `dst` has no *existing*
    /// cell to write through here, by construction: `NewLocal` always
    /// introduces a fresh binding, mirroring `reg_set_fresh`'s own
    /// distinction from `reg_set`, `util.rs`). Only when `dst` is captured
    /// does it then give it a *fresh* cell via `dynjit_cell_set_fresh`,
    /// which deopts on allocation-budget exhaustion - nothing has been
    /// written to any cell yet in that case, so redoing `NewLocal` from the
    /// interpreter is exactly correct.
    fn lower_new_local(&mut self, pc: usize, dst: u16, src: u16, fallthrough_target: Option<Block>) {
        let t = self.load_tag(src);
        let p = self.load_payload(src);
        self.store_flat(dst, t, p);
        if !self.is_captured(dst) {
            self.fallthrough(fallthrough_target);
            return;
        }
        let reg_arg = self.builder.ins().iconst(types::I64, dst as i64);
        let call = self
            .builder
            .ins()
            .call(self.cell_set_fresh_ref, &[self.rt, self.frame, self.regs, reg_arg]);
        let result = self.builder.inst_results(call)[0];
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, 1);
        let pcv = self.pc_const(pc);
        let cont = self.new_block();
        self.builder
            .ins()
            .brif(ok, cont, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(cont);
        self.fallthrough(fallthrough_target);
    }

    fn fallthrough(&mut self, target: Option<Block>) {
        match target {
            Some(target) => {
                self.builder.ins().jump(target, &[]);
            }
            None => {
                self.builder.ins().trap(TRAP_UNREACHABLE_TAIL);
            }
        }
    }

    fn lower_unconditional_jump(&mut self, pc: usize, delta: i32) {
        let target_pc = (pc as i32 + delta) as usize;
        if delta < 0 {
            let cont = self.emit_safepoint(pc);
            self.builder.switch_to_block(cont);
        }
        self.builder.ins().jump(self.blocks[target_pc], &[]);
    }

    fn lower_conditional_jump(
        &mut self,
        pc: usize,
        reg: u16,
        delta: i32,
        jump_if_true: bool,
        fallthrough_target: Option<Block>,
    ) {
        let truthy = self.truthy(reg);
        let take_branch = if jump_if_true {
            truthy
        } else {
            self.builder.ins().bxor_imm_s(truthy, 1)
        };
        let target_pc = (pc as i32 + delta) as usize;
        let fallthrough_blk = match fallthrough_target {
            Some(block) => block,
            None => self.trap_block(),
        };
        if delta < 0 {
            let taken = self.new_block();
            self.builder
                .ins()
                .brif(take_branch, taken, &[], fallthrough_blk, &[]);
            self.builder.switch_to_block(taken);
            let cont = self.emit_safepoint(pc);
            self.builder.switch_to_block(cont);
            self.builder.ins().jump(self.blocks[target_pc], &[]);
        } else {
            self.builder
                .ins()
                .brif(take_branch, self.blocks[target_pc], &[], fallthrough_blk, &[]);
        }
    }

    /// `Instr::Binary`'s general case - calls `dynjit_binary`
    /// (`stubs.rs`, whose own doc comment explains why `Concat` always
    /// deopts rather than being handled here) and either falls through on a
    /// primitive result or deopts for anything that needs the interpreter
    /// (a metamethod call, a coercion failure, or `Concat`).
    fn lower_binary(
        &mut self,
        pc: usize,
        op: BinaryOp,
        dst: u16,
        left: u16,
        right: u16,
        fallthrough_target: Option<Block>,
    ) {
        let dst_v = self.builder.ins().iconst(types::I64, dst as i64);
        let left_v = self.builder.ins().iconst(types::I64, left as i64);
        let right_v = self.builder.ins().iconst(types::I64, right as i64);
        let op_v = self.builder.ins().iconst(types::I64, op as u8 as i64);
        let call = self.builder.ins().call(
            self.binary_ref,
            &[self.rt, self.regs, dst_v, left_v, right_v, op_v],
        );
        let result = self.builder.inst_results(call)[0];
        let ok = self.builder.ins().icmp_imm_s(IntCC::Equal, result, 1);
        let pcv = self.pc_const(pc);
        let cont = self.new_block();
        self.builder
            .ins()
            .brif(ok, cont, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(cont);
        self.sync_cell_out(dst);
        self.fallthrough(fallthrough_target);
    }

    fn lower_for_prep(&mut self, pc: usize, base: u16, delta: i32) {
        let base_v = self.builder.ins().iconst(types::I64, base as i64);
        let call = self
            .builder
            .ins()
            .call(self.for_prep_ref, &[self.rt, self.frame, self.regs, base_v]);
        let outcome = self.builder.inst_results(call)[0];
        let is_skip = self.builder.ins().icmp_imm_s(IntCC::Equal, outcome, 2);
        let is_fall = self.builder.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
        let ok = self.builder.ins().bor(is_skip, is_fall);
        let pcv = self.pc_const(pc);
        let proceed = self.new_block();
        self.builder
            .ins()
            .brif(ok, proceed, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(proceed);
        let target_pc = (pc as i32 + delta) as usize;
        let fallthrough_pc = pc + 1;
        self.builder
            .ins()
            .brif(is_skip, self.blocks[target_pc], &[], self.blocks[fallthrough_pc], &[]);
    }

    fn lower_for_loop(&mut self, pc: usize, base: u16, delta: i32) {
        let base_v = self.builder.ins().iconst(types::I64, base as i64);
        let call = self
            .builder
            .ins()
            .call(self.for_loop_ref, &[self.rt, self.frame, self.regs, base_v]);
        let outcome = self.builder.inst_results(call)[0];
        let is_continue = self.builder.ins().icmp_imm_s(IntCC::Equal, outcome, 2);
        let is_end = self.builder.ins().icmp_imm_s(IntCC::Equal, outcome, 1);
        let ok = self.builder.ins().bor(is_continue, is_end);
        let pcv = self.pc_const(pc);
        let proceed = self.new_block();
        self.builder
            .ins()
            .brif(ok, proceed, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(proceed);
        let target_pc = (pc as i32 + delta) as usize;
        let fallthrough_pc = pc + 1;
        self.builder
            .ins()
            .brif(is_continue, self.blocks[target_pc], &[], self.blocks[fallthrough_pc], &[]);
    }

    /// `Instr::TForLoop` - mirrors `dispatch_step`'s own arm: if the
    /// generic-for iterator's first result (`base + 3`) isn't `Nil`, copy it
    /// into the loop control register (`base + 2`) and branch back; otherwise
    /// fall through and let the loop end. The backward branch charges a
    /// safepoint exactly like `ForLoop`'s own back-edge, since this is the
    /// back-edge of a generic `for` loop whose body can otherwise run
    /// natively indefinitely between `TForCall` deopts.
    fn lower_tfor_loop(&mut self, pc: usize, base: u16, delta: i32, fallthrough_target: Option<Block>) {
        let tag = self.load_tag(base + 3);
        let is_nil = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_NIL);
        let not_nil = self.builder.ins().bxor_imm_s(is_nil, 1);
        let target_pc = (pc as i32 + delta) as usize;
        let fallthrough_blk = match fallthrough_target {
            Some(block) => block,
            None => self.trap_block(),
        };
        let taken = self.new_block();
        self.builder.ins().brif(not_nil, taken, &[], fallthrough_blk, &[]);
        self.builder.switch_to_block(taken);
        self.copy_reg(base + 2, base + 3);
        if delta < 0 {
            let cont = self.emit_safepoint(pc);
            self.builder.switch_to_block(cont);
        }
        self.builder.ins().jump(self.blocks[target_pc], &[]);
    }
}

/// Declares and defines `name` as a new function in `module`, lowering
/// `proto`'s body per `lower_instr` above. Caller (`DynJit::promote`) must
/// have already confirmed `is_eligible(proto)`.
pub(super) fn lower_proto(
    module: &mut JITModule,
    builder_ctx: &mut FunctionBuilderContext,
    stubs: &StubFuncs,
    proto: &Proto,
    name: &str,
) -> Result<FuncId, String> {
    let frontend_config = module.target_config();
    let call_conv = frontend_config.default_call_conv;
    let mut sig = Signature::new(call_conv);
    for _ in 0..6 {
        sig.params.push(AbiParam::new(WORD));
    }
    sig.returns.push(AbiParam::new(types::I64));

    let func_id = module
        .declare_function(name, Linkage::Local, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = module.make_context();
    ctx.func.signature = sig;

    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);

        let safepoint_ref = module.declare_func_in_func(stubs.safepoint, builder.func);
        let for_prep_ref = module.declare_func_in_func(stubs.for_prep, builder.func);
        let for_loop_ref = module.declare_func_in_func(stubs.for_loop, builder.func);
        let binary_ref = module.declare_func_in_func(stubs.binary, builder.func);
        let get_field_ref = module.declare_func_in_func(stubs.get_field, builder.func);
        let set_field_ref = module.declare_func_in_func(stubs.set_field, builder.func);
        let get_global_ref = module.declare_func_in_func(stubs.get_global, builder.func);
        let set_global_ref = module.declare_func_in_func(stubs.set_global, builder.func);
        let get_index_ref = module.declare_func_in_func(stubs.get_index, builder.func);
        let set_index_ref = module.declare_func_in_func(stubs.set_index, builder.func);
        let get_upval_ref = module.declare_func_in_func(stubs.get_upval, builder.func);
        let set_upval_ref = module.declare_func_in_func(stubs.set_upval, builder.func);
        let get_environment_ref = module.declare_func_in_func(stubs.get_environment, builder.func);
        let set_environment_ref = module.declare_func_in_func(stubs.set_environment, builder.func);
        let new_table_ref = module.declare_func_in_func(stubs.new_table, builder.func);
        let new_closure_ref = module.declare_func_in_func(stubs.new_closure, builder.func);
        let mark_close_ref = module.declare_func_in_func(stubs.mark_close, builder.func);
        let cell_set_ref = module.declare_func_in_func(stubs.cell_set, builder.func);
        let cell_set_fresh_ref = module.declare_func_in_func(stubs.cell_set_fresh, builder.func);
        let detach_cell_ref = module.declare_func_in_func(stubs.detach_cell, builder.func);

        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        let params = builder.block_params(entry).to_vec();
        let (rt, frame, regs, out_pc, out_base, out_count) =
            (params[0], params[1], params[2], params[3], params[4], params[5]);

        let n = proto.instrs.len();
        let blocks: Vec<Block> = (0..n).map(|_| builder.create_block()).collect();
        let deopt_block = builder.create_block();
        builder.append_block_param(deopt_block, types::I32);

        builder.switch_to_block(entry);
        builder.ins().jump(blocks[0], &[]);

        let mut lowerer = Lowerer {
            builder,
            proto,
            register_count: proto.metadata.registers as usize,
            rt,
            frame,
            regs,
            out_base,
            out_count,
            blocks: blocks.clone(),
            deopt_block,
            extra_blocks: Vec::new(),
            safepoint_ref,
            for_prep_ref,
            for_loop_ref,
            binary_ref,
            get_field_ref,
            set_field_ref,
            get_global_ref,
            set_global_ref,
            get_index_ref,
            set_index_ref,
            get_upval_ref,
            set_upval_ref,
            get_environment_ref,
            set_environment_ref,
            new_table_ref,
            new_closure_ref,
            mark_close_ref,
            cell_set_ref,
            cell_set_fresh_ref,
            detach_cell_ref,
        };

        for pc in 0..n {
            lowerer.builder.switch_to_block(lowerer.blocks[pc]);
            lowerer.lower_instr(pc);
        }

        lowerer.builder.switch_to_block(deopt_block);
        let pc_param = lowerer.builder.block_params(deopt_block)[0];
        let pc_wide = lowerer.builder.ins().uextend(types::I64, pc_param);
        lowerer
            .builder
            .ins()
            .store(MemFlagsData::trusted(), pc_wide, out_pc, 0);
        let zero = lowerer.builder.ins().iconst(types::I64, 0);
        lowerer.builder.ins().return_(&[zero]);

        let mut all_blocks = vec![entry, deopt_block];
        all_blocks.extend(lowerer.blocks.iter().copied());
        all_blocks.extend(lowerer.extra_blocks.iter().copied());
        for b in all_blocks {
            lowerer.builder.seal_block(b);
        }
        lowerer.builder.finalize(frontend_config);
    }

    module
        .define_function(func_id, &mut ctx)
        .map_err(|e| e.to_string())?;
    module.clear_context(&mut ctx);
    Ok(func_id)
}
