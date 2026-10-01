//! U10 work item 3: a second, more specialized Cranelift lowering built on
//! top of U9's baseline tier (`lower.rs`), driven by `sol_ir`'s proof
//! propagation (U10 work items 1-2) rather than raw `Instr` pattern matching
//! alone.
//!
//! Shares `lower.rs`'s exact ABI (`super::abi::NativeFn`), flat per-register
//! `*mut sol_core::Value` array, and per-pc-block Cranelift structure -
//! `LuaRuntime::run_native` (`lua_runtime/dispatch.rs`) already treats any
//! `NativeFn` pointer uniformly regardless of which tier produced it, so
//! nothing there needed to change for this module to exist. What differs is
//! *instruction selection*: `sol_ir::lift_proto` + `sol_ir::propagate_proofs`
//! run once, up front, over `proto`'s whole body; wherever that pass has
//! already statically proven an operand's tag - a fact derived purely from
//! this `Proto`'s own constant/arithmetic dataflow, not a speculative
//! runtime profile - the runtime tag-check branch `lower.rs`'s
//! `Instr::Binary`/`Instr::IntegerBinary`/`Instr::Neg`/`Instr::BitNot` arms
//! always emit is skipped entirely, since it can never fail. There is
//! deliberately no `Guard`/deopt-on-misspeculation machinery here:
//! `sol_ir::Inst::Guard` stays reserved (see that module's own doc) for a
//! later item that threads real runtime profile data (e.g. `Proto`'s own
//! `field_cache`/`global_cache` inline-cache hit rates) into speculative,
//! falsifiable proofs - everything proven here is unconditionally true, so
//! skipping the check is a pure optimization, not a speculation.
//!
//! Eligibility (`is_eligible` below) is intentionally a strict *subset* of
//! `lower::is_eligible`'s own whitelist: it requires `sol_ir::lift_proto` to
//! report `fully_lifted`, and `sol_ir::lift_proto` only models
//! `LoadConst`(non-string)/`LoadNil`/`LoadBool`/`Move`/uncaptured
//! `NewLocal`/`DetachCell`, `Not`/`Neg`/`BitNot`, `Binary`/`IntegerBinary`,
//! `Jump`/`JumpIfFalse`/`JumpIfTrue`, and a `Return` of zero or one value -
//! no field/global/table/upvalue access, no calls, no `for` loops, no
//! captured registers (a captured register implies some `NewClosure`
//! created the capture, and `NewClosure` is outside `sol_ir`'s modeled
//! subset, so `fully_lifted` is already false whenever any register is
//! captured). Extending `sol_ir::lift_proto` to model those is future work,
//! not attempted here - see `docs/features/milestones/u10-optimizing-jit-osr.md`.
//! A direct consequence: every register in an eligible `Proto` is
//! statically known *uncaptured*, so this module's lowering never needs the
//! cell-sync stubs `lower.rs`'s `store_value`/`sync_cell_out` call.

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
use crate::sol_ir::{self, Proof};
use sol_core::{ValueCount, ValueTag};

use super::abi::{TAG_BOOLEAN, TAG_FLOAT, TAG_INTEGER, TAG_NIL, WORD};
use super::stubs::StubFuncs;

/// See `lower.rs`'s identical constant for why `fallthrough` traps rather
/// than guessing a resume target for bytecode's trailing dead tail.
const TRAP_UNREACHABLE_TAIL: TrapCode = TrapCode::unwrap_user(1);

/// Whether `proto` is eligible for this tier - see the module doc. Lifting
/// twice (once here, once in `lower_proto`) is deliberate: this tier is
/// only ever attempted on an already-`Native` `Proto` that has stayed hot a
/// second time (`LuaRuntime::try_optimize`), so it runs far less often than
/// `lower::is_eligible`'s own per-promotion check, and keeping the two
/// lifts independent avoids threading a half-built `sol_ir::Function`
/// through `DynJit`'s own `promote`/`optimize` dispatch.
pub(super) fn is_eligible(proto: &Proto) -> bool {
    if proto.instrs.is_empty() {
        return false;
    }
    if !sol_ir::lift_proto(proto).fully_lifted {
        return false;
    }
    // `sol_ir::lift_proto` happily lifts a string `LoadConst` (it only
    // needs the constant's *tag* for `Proof::TagProven(Object)`), but this
    // tier's leaf-only ABI has no interning/allocation story, same
    // restriction as `lower::is_eligible`'s own first arm.
    !proto.instrs.iter().any(|instr| {
        matches!(
            instr,
            Instr::LoadConst(_, k) if matches!(proto.consts.get(*k as usize), Some(Const::Str(_)))
        )
    })
}

/// For a bytecode `pc` that `sol_ir` recorded as value-producing
/// (`Function::value_at_pc`), the already-propagated `Proof`s of its
/// operand(s) - one for a `Unary`, two (lhs, rhs) for a `Binary` - or
/// `None` if `pc` isn't a `Unary`/`Binary` (every other value-producing
/// instruction has nothing worth proof-checking at codegen time: a `Const`
/// is already a literal, and `Phi` never arises here since this tier walks
/// `proto.instrs` directly rather than `sol_ir`'s block graph).
fn operand_proofs(func: &sol_ir::Function, pc: usize) -> Option<Vec<Proof>> {
    let (_, inst) = func.value_at_pc.get(&pc)?;
    match inst {
        sol_ir::Inst::Unary(_, a) => Some(vec![func.proofs.get(a).copied().unwrap_or(Proof::None)]),
        sol_ir::Inst::Binary(_, a, b) => Some(vec![
            func.proofs.get(a).copied().unwrap_or(Proof::None),
            func.proofs.get(b).copied().unwrap_or(Proof::None),
        ]),
        _ => None,
    }
}

fn proven_tag(proof: Proof) -> Option<ValueTag> {
    match proof {
        Proof::TagProven(tag) => Some(tag),
        _ => None,
    }
}

struct Lowerer<'a, 'b> {
    builder: FunctionBuilder<'b>,
    proto: &'a Proto,
    func: &'a sol_ir::Function,
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
    binary_ref: FuncRef,
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

    /// No cell write-through needed - see the module doc: every register in
    /// an eligible `Proto` is statically uncaptured.
    fn store_value(&mut self, reg: u16, tag: ClifValue, payload: ClifValue) {
        let off = Self::reg_offset(reg);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), tag, self.regs, off);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), payload, self.regs, off + 8);
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

    /// Identical to `lower.rs`'s own `truthy` - see that doc comment.
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

    fn operand_proofs(&self, pc: usize) -> Option<Vec<Proof>> {
        operand_proofs(self.func, pc)
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
            // Never a captured `dst` - see the module doc - so this is
            // always the plain alias `lower.rs`'s own `lower_new_local`
            // falls back to when `dst` isn't captured.
            Instr::NewLocal(dst, src, _) => {
                self.copy_reg(*dst, *src);
                self.fallthrough(fallthrough_target);
            }
            // Never a captured `reg` - a complete no-op, same as the
            // interpreter's own handling of an uncaptured `DetachCell`.
            Instr::DetachCell(_) => {
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
                self.lower_neg(pc, *dst, *src, fallthrough_target);
            }
            Instr::BitNot(dst, src) => {
                self.lower_bitnot(pc, *dst, *src, fallthrough_target);
            }
            Instr::Binary(op, dst, left, right) | Instr::IntegerBinary(op, dst, left, right) => {
                self.lower_binary(pc, *op, *dst, *left, *right, fallthrough_target);
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
            _ => unreachable!("is_eligible excludes every other Instr variant"),
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

    /// `Neg`'s proof-specialized lowering: when `sol_ir` has already proven
    /// the operand's tag, the runtime `is_int`/`is_float` check
    /// `lower.rs`'s own `Neg` arm always emits can never fail, so it's
    /// skipped outright in favor of the one branch proofs guarantee is
    /// taken. Otherwise falls back to `lower.rs`'s exact unproven-case
    /// lowering (tag check, `select` between the integer/float result).
    fn lower_neg(&mut self, pc: usize, dst: u16, src: u16, fallthrough_target: Option<Block>) {
        let proven = self
            .operand_proofs(pc)
            .and_then(|proofs| proofs.first().copied())
            .and_then(proven_tag);
        match proven {
            Some(ValueTag::Integer) => {
                let payload = self.load_payload(src);
                let result = self.builder.ins().ineg(payload);
                let tag = self.builder.ins().iconst(types::I64, TAG_INTEGER);
                self.store_value(dst, tag, result);
                self.fallthrough(fallthrough_target);
            }
            Some(ValueTag::Float) => {
                let payload = self.load_payload(src);
                let float_bits = self.builder.ins().bitcast(types::F64, MemFlagsData::new(), payload);
                let negf = self.builder.ins().fneg(float_bits);
                let float_result = self.builder.ins().bitcast(types::I64, MemFlagsData::new(), negf);
                let tag = self.builder.ins().iconst(types::I64, TAG_FLOAT);
                self.store_value(dst, tag, float_result);
                self.fallthrough(fallthrough_target);
            }
            _ => {
                let tag = self.load_tag(src);
                let is_int = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_INTEGER);
                let is_float = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_FLOAT);
                let is_numeric = self.builder.ins().bor(is_int, is_float);
                let pcv = self.pc_const(pc);
                let fast = self.new_block();
                self.builder
                    .ins()
                    .brif(is_numeric, fast, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
                self.builder.switch_to_block(fast);
                let payload = self.load_payload(src);
                let int_result = self.builder.ins().ineg(payload);
                let float_bits = self.builder.ins().bitcast(types::F64, MemFlagsData::new(), payload);
                let negf = self.builder.ins().fneg(float_bits);
                let float_result = self.builder.ins().bitcast(types::I64, MemFlagsData::new(), negf);
                let result = self.builder.ins().select(is_int, int_result, float_result);
                let int_tag = self.builder.ins().iconst(types::I64, TAG_INTEGER);
                let float_tag = self.builder.ins().iconst(types::I64, TAG_FLOAT);
                let result_tag = self.builder.ins().select(is_int, int_tag, float_tag);
                self.store_value(dst, result_tag, result);
                self.fallthrough(fallthrough_target);
            }
        }
    }

    /// `BitNot`'s proof-specialized lowering - same shape as `lower_neg`.
    fn lower_bitnot(&mut self, pc: usize, dst: u16, src: u16, fallthrough_target: Option<Block>) {
        let proven = self
            .operand_proofs(pc)
            .and_then(|proofs| proofs.first().copied())
            .and_then(proven_tag);
        if proven == Some(ValueTag::Integer) {
            let payload = self.load_payload(src);
            let result = self.builder.ins().bnot(payload);
            let tag = self.builder.ins().iconst(types::I64, TAG_INTEGER);
            self.store_value(dst, tag, result);
            self.fallthrough(fallthrough_target);
            return;
        }
        let tag = self.load_tag(src);
        let is_int = self.builder.ins().icmp_imm_s(IntCC::Equal, tag, TAG_INTEGER);
        let pcv = self.pc_const(pc);
        let fast = self.new_block();
        self.builder
            .ins()
            .brif(is_int, fast, &[], self.deopt_block, &[BlockArg::Value(pcv)]);
        self.builder.switch_to_block(fast);
        let payload = self.load_payload(src);
        let result = self.builder.ins().bnot(payload);
        let tagc = self.builder.ins().iconst(types::I64, TAG_INTEGER);
        self.store_value(dst, tagc, result);
        self.fallthrough(fallthrough_target);
    }

    /// `Binary`/`IntegerBinary`'s proof-specialized lowering: when both
    /// operands are proven `Integer` and `op` is one `IntegerBinary`'s own
    /// fast lane already models (`Add`/`Sub`/`Mul`, see `lower.rs`), the
    /// runtime "are both operands integer" check can never fail, so it's
    /// skipped and the integer arithmetic runs unconditionally. Every other
    /// op/proof combination - including a proven-Integer pair with some
    /// other operator, where the *result's* tag is proven but whether the
    /// primitive (non-metamethod) path even applies is not - falls back to
    /// `dynjit_binary`, identical to `lower.rs`'s own `lower_binary`.
    fn lower_binary(
        &mut self,
        pc: usize,
        op: BinaryOp,
        dst: u16,
        left: u16,
        right: u16,
        fallthrough_target: Option<Block>,
    ) {
        let both_proven_integer = matches!(op, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)
            && self
                .operand_proofs(pc)
                .map(|proofs| proofs.iter().all(|p| proven_tag(*p) == Some(ValueTag::Integer)))
                .unwrap_or(false);
        if both_proven_integer {
            let left_v = self.load_payload(left);
            let right_v = self.load_payload(right);
            let result = match op {
                BinaryOp::Add => self.builder.ins().iadd(left_v, right_v),
                BinaryOp::Sub => self.builder.ins().isub(left_v, right_v),
                BinaryOp::Mul => self.builder.ins().imul(left_v, right_v),
                _ => unreachable!("checked above"),
            };
            let tag = self.builder.ins().iconst(types::I64, TAG_INTEGER);
            self.store_value(dst, tag, result);
            self.fallthrough(fallthrough_target);
            return;
        }
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
        self.fallthrough(fallthrough_target);
    }
}

/// Declares and defines `name` as a new function in `module`, lowering
/// `proto`'s body per `Lowerer::lower_instr` above. Caller (`DynJit::optimize`)
/// must have already confirmed `is_eligible(proto)`.
pub(super) fn lower_proto(
    module: &mut JITModule,
    builder_ctx: &mut FunctionBuilderContext,
    stubs: &StubFuncs,
    proto: &Proto,
    name: &str,
) -> Result<FuncId, String> {
    let mut func = sol_ir::lift_proto(proto);
    sol_ir::propagate_proofs(&mut func);

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
        let binary_ref = module.declare_func_in_func(stubs.binary, builder.func);

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
            func: &func,
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
            binary_ref,
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
