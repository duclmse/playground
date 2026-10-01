//! Shared SSA IR scaffolding (U10 work item 1: see
//! `docs/features/milestones/u10-optimizing-jit-osr.md`).
//!
//! Lifts a bounded subset of `lua_bytecode::Proto` bytecode - constants,
//! unary/binary arithmetic and comparisons, and `if`/`while`-shaped control
//! flow built from `Jump`/`JumpIfFalse`/`JumpIfTrue` - into a block-structured
//! SSA form. Anything outside that subset (calls, tables, globals, upvalues,
//! numeric/generic `for`, captured registers, ...) lowers to an
//! `Inst::Unsupported`/`Terminator::Unsupported` sentinel and marks the
//! lifted `Function` as not fully lifted, rather than guessing at semantics
//! it does not yet model.
//!
//! This module has no optimizing consumer yet - no proof propagation, no
//! guard insertion, no Cranelift lowering. It exists so later U10 work items
//! (proof propagation, guard/deopt lowering, the optimizing JIT backend) have
//! a shared IR shape to build on, and so the bytecode-to-SSA lift itself can
//! be verified independently, by comparing a trivial SSA interpreter
//! (test-only, below) against the real bytecode interpreter's output on the
//! same compiled `Proto`.
//!
//! SSA construction is a simplified two-phase variant of Braun et al.'s
//! algorithm: because every block's predecessor list can be computed in one
//! static pass before any SSA value exists (`Proto`'s jump deltas are fixed
//! at compile time, unlike a compiler discovering edges on the fly), there is
//! no need for the "incomplete blocks + sealing" machinery that paper uses
//! for a CFG discovered incrementally. Phase A pre-allocates one `Phi` value
//! per register at every block with more than one predecessor (or whose sole
//! predecessor is reached only via a back edge); Phase B forward-scans blocks
//! in increasing `pc` order, seeding each block's register map either from
//! its pre-allocated phis or by direct copy from its single, already-
//! processed predecessor; a final pass resolves every phi's per-predecessor
//! operands from each predecessor's now-complete exit map.

use std::collections::{BTreeSet, HashMap};

use sol_core::{ValueCount, ValueTag};

use crate::ast::{BinaryOp, UnaryOp};
use crate::lua_bytecode::{Const, Instr, Proto, Reg};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A fact a later pass (U10 work item 2) may attach to a `ValueId`. Always
/// `Proof::None` out of `lift_proto` - this item only reserves the shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Proof {
    None,
    TagProven(ValueTag),
    ShapeProven { object_generation: u32 },
    RangeProven { lo: i64, hi: i64 },
}

#[derive(Debug, Clone)]
pub enum Inst {
    Const(Const),
    Unary(UnaryOp, ValueId),
    Binary(BinaryOp, ValueId, ValueId),
    /// A register `Phi` pre-allocated for a join or back-edge target block;
    /// `lift_proto`'s final pass fills `incomings` with one
    /// `(predecessor BlockId, ValueId)` per predecessor.
    Phi { register: Reg, incomings: Vec<(BlockId, ValueId)> },
    /// A bytecode instruction at this `pc` the bounded lifter does not model.
    /// Any register it would have written keeps its prior `ValueId` in the
    /// lifted register map, which is simply wrong for a program that
    /// actually depends on that write - callers must check
    /// `Function::fully_lifted` before trusting anything past this point.
    Unsupported(usize),
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Jump(BlockId),
    Branch { cond: ValueId, when_true: BlockId, when_false: BlockId },
    Return(Option<ValueId>),
    /// Falls off the end of `Proto::instrs` with no explicit `Return` -
    /// should not occur in practice (the compiler always emits a trailing
    /// `Return`), kept only as a defensive terminator shape.
    ImplicitReturn,
    /// A `Return` with a `ValueCount` shape (zero/one value only is
    /// modeled) or a mid-block instruction this lifter does not recognize as
    /// a block terminator.
    Unsupported(usize),
}

#[derive(Debug, Clone, Default)]
pub struct Block {
    pub insts: Vec<(ValueId, Inst)>,
    pub terminator: Option<Terminator>,
}

#[derive(Debug, Clone, Default)]
pub struct Function {
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub proofs: HashMap<ValueId, Proof>,
    /// False as soon as any instruction or terminator outside the bounded
    /// subset is encountered. A `Function` with `fully_lifted == false` must
    /// not be fed to the toy interpreter below (or, later, to any real
    /// consumer) - its register map past the first unsupported point is not
    /// trustworthy.
    pub fully_lifted: bool,
}

/// Block topology only (which block(s) a block exits to), computed before
/// any SSA value exists - `Proto`'s jump deltas/`ValueCount`s fix this shape
/// independent of register content.
enum TerminatorShape {
    Jump(BlockId),
    BranchIfFalse(Reg, BlockId, BlockId),
    BranchIfTrue(Reg, BlockId, BlockId),
    Return(Reg, ValueCount),
    ImplicitReturn,
}

struct ValueIdAlloc(u32);

impl ValueIdAlloc {
    fn next(&mut self) -> ValueId {
        let id = ValueId(self.0);
        self.0 += 1;
        id
    }
}

fn jump_target(pc: usize, delta: i32) -> usize {
    (pc as i32 + delta) as usize
}

/// Every `pc` that starts a new basic block: `0`, every `Jump`/
/// `JumpIfFalse`/`JumpIfTrue` target, and the `pc` immediately following one
/// of those three or a `Return` (control flow forks or ends there).
fn compute_block_starts(instrs: &[Instr]) -> Vec<usize> {
    let mut leaders = BTreeSet::new();
    leaders.insert(0);
    for (pc, instr) in instrs.iter().enumerate() {
        match instr {
            Instr::Jump(delta) => {
                leaders.insert(jump_target(pc, *delta));
                if pc + 1 < instrs.len() {
                    leaders.insert(pc + 1);
                }
            }
            Instr::JumpIfFalse(_, delta) | Instr::JumpIfTrue(_, delta) => {
                leaders.insert(jump_target(pc, *delta));
                if pc + 1 < instrs.len() {
                    leaders.insert(pc + 1);
                }
            }
            Instr::Return(..) => {
                if pc + 1 < instrs.len() {
                    leaders.insert(pc + 1);
                }
            }
            _ => {}
        }
    }
    leaders.into_iter().collect()
}

fn pc_to_block(block_starts: &[usize], pc: usize) -> BlockId {
    let idx = block_starts.partition_point(|&start| start <= pc) - 1;
    BlockId(idx as u32)
}

fn terminator_shape(
    block_starts: &[usize],
    instrs: &[Instr],
    block_idx: usize,
    last_pc: usize,
) -> TerminatorShape {
    match &instrs[last_pc] {
        Instr::Jump(delta) => TerminatorShape::Jump(pc_to_block(block_starts, jump_target(last_pc, *delta))),
        Instr::JumpIfFalse(reg, delta) => {
            let target = pc_to_block(block_starts, jump_target(last_pc, *delta));
            let fallthrough = BlockId((block_idx + 1) as u32);
            // Jumps when the condition is false; falls through when true.
            TerminatorShape::BranchIfFalse(*reg, fallthrough, target)
        }
        Instr::JumpIfTrue(reg, delta) => {
            let target = pc_to_block(block_starts, jump_target(last_pc, *delta));
            let fallthrough = BlockId((block_idx + 1) as u32);
            TerminatorShape::BranchIfTrue(*reg, target, fallthrough)
        }
        Instr::Return(reg, count) => TerminatorShape::Return(*reg, *count),
        _ => {
            if block_idx + 1 < block_starts.len() {
                TerminatorShape::Jump(BlockId((block_idx + 1) as u32))
            } else {
                TerminatorShape::ImplicitReturn
            }
        }
    }
}

/// Lifts a bounded subset of `proto.instrs` to SSA form. See the module doc
/// for exactly what is modeled; everything else becomes an `Unsupported`
/// sentinel and clears `Function::fully_lifted`.
pub fn lift_proto(proto: &Proto) -> Function {
    let instrs = &proto.instrs;
    let block_starts = compute_block_starts(instrs);
    let num_blocks = block_starts.len();
    let block_end = |i: usize| -> usize {
        if i + 1 < num_blocks {
            block_starts[i + 1]
        } else {
            instrs.len()
        }
    };

    // Terminator shapes (block topology) don't depend on SSA values, so they
    // can be computed before any value exists.
    let shapes: Vec<TerminatorShape> = (0..num_blocks)
        .map(|i| terminator_shape(&block_starts, instrs, i, block_end(i) - 1))
        .collect();

    let mut successors: Vec<Vec<BlockId>> = vec![Vec::new(); num_blocks];
    for (i, shape) in shapes.iter().enumerate() {
        match shape {
            TerminatorShape::Jump(target) => successors[i].push(*target),
            TerminatorShape::BranchIfFalse(_, t, f) | TerminatorShape::BranchIfTrue(_, t, f) => {
                successors[i].push(*t);
                successors[i].push(*f);
            }
            TerminatorShape::Return(..) | TerminatorShape::ImplicitReturn => {}
        }
    }
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); num_blocks];
    for (i, succs) in successors.iter().enumerate() {
        for succ in succs {
            preds[succ.0 as usize].push(BlockId(i as u32));
        }
    }

    let is_phi_block = |b: usize| -> bool {
        if b == 0 {
            return false;
        }
        match preds[b].as_slice() {
            [only] => block_starts[only.0 as usize] > block_starts[b],
            _ => true,
        }
    };

    let num_registers = proto.metadata.registers as usize;
    let mut alloc = ValueIdAlloc(0);
    let mut blocks: Vec<Block> = (0..num_blocks).map(|_| Block::default()).collect();
    let mut fully_lifted = true;

    // Phase A: pre-allocate one phi value per register at every phi block,
    // and record each phi's `ValueId` in that block's entry map.
    let mut entry_map: Vec<HashMap<Reg, ValueId>> = vec![HashMap::new(); num_blocks];
    for b in 0..num_blocks {
        if is_phi_block(b) {
            let mut map = HashMap::with_capacity(num_registers);
            for r in 0..num_registers {
                let id = alloc.next();
                blocks[b].insts.push((id, Inst::Phi { register: r as Reg, incomings: Vec::new() }));
                map.insert(r as Reg, id);
            }
            entry_map[b] = map;
        }
    }
    // Block 0 is never a phi block (no predecessors); seed every register to
    // a fresh `nil`, matching Lua's "an unset register reads as nil".
    {
        let mut map = HashMap::with_capacity(num_registers);
        for r in 0..num_registers {
            let id = alloc.next();
            blocks[0].insts.push((id, Inst::Const(Const::Nil)));
            map.insert(r as Reg, id);
        }
        entry_map[0] = map;
    }

    // Phase B: forward scan in increasing pc (== block index) order. A
    // non-phi block's sole predecessor always has a strictly lower index (by
    // `is_phi_block`'s own back-edge check), so it is always already
    // processed here.
    let mut exit_map: Vec<HashMap<Reg, ValueId>> = vec![HashMap::new(); num_blocks];
    for b in 0..num_blocks {
        let mut current: HashMap<Reg, ValueId> = if b == 0 || is_phi_block(b) {
            entry_map[b].clone()
        } else {
            exit_map[preds[b][0].0 as usize].clone()
        };

        let start = block_starts[b];
        let end = block_end(b);
        for pc in start..end {
            match &instrs[pc] {
                Instr::LoadConst(dst, idx) => {
                    let value = proto.consts[*idx as usize].clone();
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Const(value)));
                    current.insert(*dst, id);
                }
                Instr::LoadNil(dst) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Const(Const::Nil)));
                    current.insert(*dst, id);
                }
                Instr::LoadBool(dst, value) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Const(Const::Bool(*value))));
                    current.insert(*dst, id);
                }
                Instr::Move(dst, src) => {
                    let id = current[src];
                    current.insert(*dst, id);
                }
                Instr::NewLocal(dst, src, _) => {
                    if proto.captured_registers.get(*dst as usize).copied().unwrap_or(false) {
                        blocks[b].insts.push((alloc.next(), Inst::Unsupported(pc)));
                        fully_lifted = false;
                    } else {
                        let id = current[src];
                        current.insert(*dst, id);
                    }
                }
                Instr::DetachCell(reg) => {
                    if proto.captured_registers.get(*reg as usize).copied().unwrap_or(false) {
                        blocks[b].insts.push((alloc.next(), Inst::Unsupported(pc)));
                        fully_lifted = false;
                    }
                    // A no-op on an uncaptured register, same as the
                    // bytecode interpreter's own handling.
                }
                Instr::Not(dst, src) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Unary(UnaryOp::Not, current[src])));
                    current.insert(*dst, id);
                }
                Instr::Neg(dst, src) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Unary(UnaryOp::Neg, current[src])));
                    current.insert(*dst, id);
                }
                Instr::BitNot(dst, src) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Unary(UnaryOp::BitNot, current[src])));
                    current.insert(*dst, id);
                }
                Instr::Binary(op, dst, lhs, rhs) | Instr::IntegerBinary(op, dst, lhs, rhs) => {
                    let id = alloc.next();
                    blocks[b].insts.push((id, Inst::Binary(*op, current[lhs], current[rhs])));
                    current.insert(*dst, id);
                }
                Instr::Jump(_) | Instr::JumpIfFalse(..) | Instr::JumpIfTrue(..) | Instr::Return(..) => {
                    // Terminator-shaped instructions never write a register
                    // and are only ever the last instruction of their block
                    // (by construction of `compute_block_starts`); handled
                    // below via `shapes[b]`, not here.
                }
                _ => {
                    blocks[b].insts.push((alloc.next(), Inst::Unsupported(pc)));
                    fully_lifted = false;
                }
            }
        }

        let terminator = match &shapes[b] {
            TerminatorShape::Jump(target) => Terminator::Jump(*target),
            TerminatorShape::BranchIfFalse(reg, when_true, when_false)
            | TerminatorShape::BranchIfTrue(reg, when_true, when_false) => Terminator::Branch {
                cond: current[reg],
                when_true: *when_true,
                when_false: *when_false,
            },
            TerminatorShape::Return(reg, count) => match count {
                ValueCount::Fixed(0) => Terminator::Return(None),
                ValueCount::Fixed(1) => Terminator::Return(Some(current[reg])),
                _ => {
                    fully_lifted = false;
                    Terminator::Unsupported(end - 1)
                }
            },
            TerminatorShape::ImplicitReturn => Terminator::ImplicitReturn,
        };
        blocks[b].terminator = Some(terminator);
        exit_map[b] = current;
    }

    // Resolve every phi's per-predecessor operand from each predecessor's
    // now-complete exit map.
    for b in 0..num_blocks {
        if !is_phi_block(b) {
            continue;
        }
        for (_, inst) in blocks[b].insts.iter_mut() {
            if let Inst::Phi { register, incomings } = inst {
                for pred in &preds[b] {
                    let value = exit_map[pred.0 as usize][register];
                    incomings.push((*pred, value));
                }
            }
        }
    }

    Function {
        blocks,
        entry: BlockId(0),
        proofs: HashMap::new(),
        fully_lifted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    use crate::lua_bytecode::Compiler;

    fn compile(source: &[u8]) -> Rc<Proto> {
        let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).unwrap())
            .expect("test source parses");
        Compiler::compile_top_level(&program.functions[0]).expect("test source compiles")
    }

    /// A trivial SSA interpreter: test-only, used solely to check that
    /// `lift_proto`'s output re-derives the same answer as the real bytecode
    /// interpreter for a program restricted to the bounded subset. Not a
    /// consumer any later work item should build on.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum IrValue {
        Nil,
        Bool(bool),
        Int(i64),
        Float(f64),
    }

    impl IrValue {
        fn truthy(self) -> bool {
            !matches!(self, IrValue::Nil | IrValue::Bool(false))
        }
    }

    fn from_const(c: &Const) -> IrValue {
        match c {
            Const::Nil => IrValue::Nil,
            Const::Bool(b) => IrValue::Bool(*b),
            Const::Integer(i) => IrValue::Int(*i),
            Const::Float(f) => IrValue::Float(*f),
            Const::Str(_) => panic!("sol_ir toy interpreter: bounded subset excludes strings"),
        }
    }

    fn as_f64(v: IrValue) -> f64 {
        match v {
            IrValue::Int(i) => i as f64,
            IrValue::Float(f) => f,
            _ => panic!("sol_ir toy interpreter: expected a number"),
        }
    }

    fn as_i64(v: IrValue) -> i64 {
        match v {
            IrValue::Int(i) => i,
            IrValue::Float(f) => f as i64,
            _ => panic!("sol_ir toy interpreter: expected a number"),
        }
    }

    fn eval_unary(op: UnaryOp, a: IrValue) -> IrValue {
        match op {
            UnaryOp::Neg => match a {
                IrValue::Int(i) => IrValue::Int(-i),
                IrValue::Float(f) => IrValue::Float(-f),
                _ => panic!("sol_ir toy interpreter: Neg on a non-number"),
            },
            UnaryOp::Not => IrValue::Bool(!a.truthy()),
            UnaryOp::BitNot => IrValue::Int(!as_i64(a)),
        }
    }

    fn numeric_cmp(a: IrValue, b: IrValue) -> std::cmp::Ordering {
        match (a, b) {
            (IrValue::Int(x), IrValue::Int(y)) => x.cmp(&y),
            _ => as_f64(a)
                .partial_cmp(&as_f64(b))
                .expect("sol_ir toy interpreter: NaN comparison"),
        }
    }

    fn numeric_eq(a: IrValue, b: IrValue) -> bool {
        match (a, b) {
            (IrValue::Int(x), IrValue::Int(y)) => x == y,
            (IrValue::Bool(x), IrValue::Bool(y)) => x == y,
            (IrValue::Nil, IrValue::Nil) => true,
            (IrValue::Int(_) | IrValue::Float(_), IrValue::Int(_) | IrValue::Float(_)) => as_f64(a) == as_f64(b),
            _ => false,
        }
    }

    fn eval_binary(op: BinaryOp, a: IrValue, b: IrValue) -> IrValue {
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Mod | BinaryOp::FloorDiv => {
                if let (IrValue::Int(x), IrValue::Int(y)) = (a, b) {
                    return IrValue::Int(match op {
                        BinaryOp::Add => x.wrapping_add(y),
                        BinaryOp::Sub => x.wrapping_sub(y),
                        BinaryOp::Mul => x.wrapping_mul(y),
                        BinaryOp::Mod => x.rem_euclid(y),
                        BinaryOp::FloorDiv => x.div_euclid(y),
                        _ => unreachable!(),
                    });
                }
                let (x, y) = (as_f64(a), as_f64(b));
                IrValue::Float(match op {
                    BinaryOp::Add => x + y,
                    BinaryOp::Sub => x - y,
                    BinaryOp::Mul => x * y,
                    BinaryOp::Mod => x - (x / y).floor() * y,
                    BinaryOp::FloorDiv => (x / y).floor(),
                    _ => unreachable!(),
                })
            }
            BinaryOp::Div => IrValue::Float(as_f64(a) / as_f64(b)),
            BinaryOp::Pow => IrValue::Float(as_f64(a).powf(as_f64(b))),
            BinaryOp::BitAnd => IrValue::Int(as_i64(a) & as_i64(b)),
            BinaryOp::BitOr => IrValue::Int(as_i64(a) | as_i64(b)),
            BinaryOp::BitXor => IrValue::Int(as_i64(a) ^ as_i64(b)),
            BinaryOp::Shl => IrValue::Int(as_i64(a) << as_i64(b)),
            BinaryOp::Shr => IrValue::Int(((as_i64(a) as u64) >> as_i64(b)) as i64),
            BinaryOp::Concat => panic!("sol_ir toy interpreter: Concat is outside the bounded subset"),
            BinaryOp::Eq => IrValue::Bool(numeric_eq(a, b)),
            BinaryOp::NotEq => IrValue::Bool(!numeric_eq(a, b)),
            BinaryOp::Lt => IrValue::Bool(numeric_cmp(a, b) == std::cmp::Ordering::Less),
            BinaryOp::Le => IrValue::Bool(numeric_cmp(a, b) != std::cmp::Ordering::Greater),
            BinaryOp::Gt => IrValue::Bool(numeric_cmp(a, b) == std::cmp::Ordering::Greater),
            BinaryOp::Ge => IrValue::Bool(numeric_cmp(a, b) != std::cmp::Ordering::Less),
            BinaryOp::And => if a.truthy() { b } else { a },
            BinaryOp::Or => if a.truthy() { a } else { b },
        }
    }

    fn interpret(func: &Function) -> IrValue {
        assert!(func.fully_lifted, "fixture exceeds the bounded subset this toy interpreter models");
        let mut values: HashMap<ValueId, IrValue> = HashMap::new();
        let mut current = func.entry;
        let mut prev: Option<BlockId> = None;
        loop {
            let block = &func.blocks[current.0 as usize];
            for (vid, inst) in &block.insts {
                let v = match inst {
                    Inst::Const(c) => from_const(c),
                    Inst::Unary(op, a) => eval_unary(*op, values[a]),
                    Inst::Binary(op, a, b) => eval_binary(*op, values[a], values[b]),
                    Inst::Phi { incomings, .. } => {
                        let from = prev.expect("phi block reached with no predecessor recorded");
                        incomings
                            .iter()
                            .find(|(pred, _)| *pred == from)
                            .map(|(_, v)| values[v])
                            .expect("missing phi incoming for the predecessor actually taken")
                    }
                    Inst::Unsupported(pc) => {
                        panic!("sol_ir toy interpreter hit Unsupported at pc {pc}")
                    }
                };
                values.insert(*vid, v);
            }
            match block.terminator.as_ref().expect("block built with no terminator") {
                Terminator::Jump(target) => {
                    prev = Some(current);
                    current = *target;
                }
                Terminator::Branch { cond, when_true, when_false } => {
                    let next = if values[cond].truthy() { *when_true } else { *when_false };
                    prev = Some(current);
                    current = next;
                }
                Terminator::Return(value) => {
                    return value.map(|id| values[&id]).unwrap_or(IrValue::Nil);
                }
                Terminator::ImplicitReturn => return IrValue::Nil,
                Terminator::Unsupported(pc) => {
                    panic!("sol_ir toy interpreter hit an unsupported terminator at pc {pc}")
                }
            }
        }
    }

    fn run_bytecode(source: &[u8]) -> IrValue {
        let run = crate::lua_runtime::run_source(source).expect("fixture runs under the bytecode interpreter");
        match run.value {
            crate::lua_runtime::LuaValue::Nil => IrValue::Nil,
            crate::lua_runtime::LuaValue::Bool(b) => IrValue::Bool(b),
            crate::lua_runtime::LuaValue::Integer(i) => IrValue::Int(i),
            crate::lua_runtime::LuaValue::Float(f) => IrValue::Float(f),
            other => panic!("fixture returned a non-numeric, non-boolean value: {other:?}"),
        }
    }

    fn assert_lift_matches_interpreter(source: &[u8]) {
        let proto = compile(source);
        let func = lift_proto(&proto);
        assert!(func.fully_lifted, "fixture should stay within the bounded subset");
        let lifted = interpret(&func);
        let interpreted = run_bytecode(source);
        assert_eq!(lifted, interpreted, "lifted SSA diverged from the bytecode interpreter");
    }

    #[test]
    fn straight_line_arithmetic_lifts_and_matches() {
        assert_lift_matches_interpreter(b"local a = 3 local b = 4 return a * b + 1");
    }

    #[test]
    fn if_else_merge_point_becomes_a_phi() {
        assert_lift_matches_interpreter(
            b"local x = 7 local y
              if x > 5 then y = 1 else y = 2 end
              return y",
        );
        assert_lift_matches_interpreter(
            b"local x = 2 local y
              if x > 5 then y = 1 else y = 2 end
              return y",
        );
    }

    #[test]
    fn while_loop_backedge_becomes_a_phi() {
        assert_lift_matches_interpreter(
            b"local s = 0 local i = 1
              while i <= 10 do
                s = s + i
                i = i + 1
              end
              return s",
        );
    }

    #[test]
    fn while_loop_that_never_runs_still_matches() {
        assert_lift_matches_interpreter(
            b"local s = 0 local i = 1
              while i <= 0 do
                s = s + i
                i = i + 1
              end
              return s",
        );
    }

    #[test]
    fn unsupported_instruction_clears_fully_lifted() {
        let proto = compile(b"return {}");
        let func = lift_proto(&proto);
        assert!(!func.fully_lifted, "NewTable is outside the bounded subset");
    }
}
