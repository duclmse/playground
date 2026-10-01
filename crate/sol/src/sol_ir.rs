//! Shared SSA IR scaffolding (U10 work items 1-2: see
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
//! `propagate_proofs` populates `Function::proofs` from constant tags and
//! arithmetic/comparison combination rules (work item 2). `Inst::Guard`,
//! `GuardFact`, and `DeoptSnapshot` are also work item 2's, but their
//! construction from real bytecode is deferred: a tag guard would need U8's
//! `BoundedCache` profile data threaded into this lifter, which is work item
//! 3's job (`opt_lower.rs`), not this one's. `hoist_loop_invariant_guards`
//! and `fuse_redundant_guards` are real, tested IR transformations over
//! whatever `Inst::Guard`s a `Function` already contains - exercised in this
//! module's tests against hand-built graphs, per the plan's own verification
//! text, independent of who eventually inserts the first real guard.
//!
//! This module still has no Cranelift-lowering consumer. It exists so later
//! U10 work items (guard/deopt lowering, the optimizing JIT backend) have a
//! shared IR shape to build on, and so the bytecode-to-SSA lift itself can be
//! verified independently, by comparing a trivial SSA interpreter (test-only,
//! below) against the real bytecode interpreter's output on the same
//! compiled `Proto`.
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

use sol_core::{StackMap, ValueCount, ValueTag};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GuardId(pub u32);

/// A fact a `Guard` checks at runtime before later code may rely on it. Only
/// one variant so far - work item 2 reserves the shape; nothing in this
/// module constructs a real `Guard` yet, since doing so for arithmetic would
/// need U8's `BoundedCache` profile data threaded into the lifter, which is
/// work item 3's job (`opt_lower.rs`), not this one's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GuardFact {
    TagIsInteger(ValueId),
}

/// What the interpreter needs to resume correctly if a `Guard` fails: the
/// bytecode `pc` to resume at, and which `frame.regs` slots must be valid
/// there. Under this milestone's no-register-unboxing scope decision (see
/// the module doc), the flat native register array stays fully synced to
/// `frame.regs` at all times, so `DeoptSnapshot::full` - naming every
/// register - is always a correct (if not precisely liveness-scoped)
/// snapshot; a true liveness-scoped subset is a documented future
/// refinement, not built now.
#[derive(Debug, Clone)]
pub struct DeoptSnapshot {
    pub resume_pc: usize,
    pub stack_map: StackMap,
}

impl DeoptSnapshot {
    pub fn full(resume_pc: usize, num_registers: usize) -> Self {
        DeoptSnapshot { resume_pc, stack_map: StackMap::new((0..num_registers).collect()) }
    }
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
    /// A runtime check guarding every later use of `fact`'s `ValueId` against
    /// a failed assumption; `snapshot` is where/how to resume the
    /// interpreter on failure. See `GuardFact`'s doc - not constructed by
    /// `lift_proto` in this work item, only by the hand-built graphs
    /// `hoist_loop_invariant_guards`/`fuse_redundant_guards` are tested
    /// against below.
    Guard { id: GuardId, fact: GuardFact, snapshot: DeoptSnapshot },
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
    /// The result `ValueId` and lifted `Inst` for every bytecode `pc` that
    /// produced a new SSA value (`LoadConst`/`LoadNil`/`LoadBool`, `Not`/
    /// `Neg`/`BitNot`, `Binary`/`IntegerBinary`) - `Move`/`NewLocal`/
    /// `DetachCell` alias an existing `ValueId` rather than defining a new
    /// one, so they have no entry here. Work item 3's `opt_lower.rs` is the
    /// only consumer: it needs each arithmetic instruction's *operand*
    /// `ValueId`s (already inside the recorded `Inst::Binary`/`Inst::Unary`)
    /// to look up `proofs` without re-deriving Phase B's per-pc register
    /// map from scratch.
    pub value_at_pc: HashMap<usize, (ValueId, Inst)>,
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
    let mut value_at_pc: HashMap<usize, (ValueId, Inst)> = HashMap::new();
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
                    let inst = Inst::Const(value);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
                    current.insert(*dst, id);
                }
                Instr::LoadNil(dst) => {
                    let id = alloc.next();
                    let inst = Inst::Const(Const::Nil);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
                    current.insert(*dst, id);
                }
                Instr::LoadBool(dst, value) => {
                    let id = alloc.next();
                    let inst = Inst::Const(Const::Bool(*value));
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
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
                    let inst = Inst::Unary(UnaryOp::Not, current[src]);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
                    current.insert(*dst, id);
                }
                Instr::Neg(dst, src) => {
                    let id = alloc.next();
                    let inst = Inst::Unary(UnaryOp::Neg, current[src]);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
                    current.insert(*dst, id);
                }
                Instr::BitNot(dst, src) => {
                    let id = alloc.next();
                    let inst = Inst::Unary(UnaryOp::BitNot, current[src]);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
                    current.insert(*dst, id);
                }
                Instr::Binary(op, dst, lhs, rhs) | Instr::IntegerBinary(op, dst, lhs, rhs) => {
                    let id = alloc.next();
                    let inst = Inst::Binary(*op, current[lhs], current[rhs]);
                    blocks[b].insts.push((id, inst.clone()));
                    value_at_pc.insert(pc, (id, inst));
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
        value_at_pc,
    }
}

fn tag_of(proof: Proof) -> Option<ValueTag> {
    match proof {
        Proof::TagProven(tag) => Some(tag),
        _ => None,
    }
}

fn const_proof(c: &Const) -> Proof {
    match c {
        Const::Nil => Proof::TagProven(ValueTag::Nil),
        Const::Bool(_) => Proof::TagProven(ValueTag::Boolean),
        Const::Integer(_) => Proof::TagProven(ValueTag::Integer),
        Const::Float(_) => Proof::TagProven(ValueTag::Float),
        // Strings are heap objects at the `ValueTag` granularity - there is
        // no separate string tag to prove.
        Const::Str(_) => Proof::TagProven(ValueTag::Object),
    }
}

fn unary_proof(op: UnaryOp, operand: Proof) -> Proof {
    match op {
        UnaryOp::Not => Proof::TagProven(ValueTag::Boolean),
        UnaryOp::BitNot => Proof::TagProven(ValueTag::Integer),
        UnaryOp::Neg => match tag_of(operand) {
            Some(ValueTag::Integer) => Proof::TagProven(ValueTag::Integer),
            Some(ValueTag::Float) => Proof::TagProven(ValueTag::Float),
            _ => Proof::None,
        },
    }
}

fn binary_proof(op: BinaryOp, lhs: Proof, rhs: Proof) -> Proof {
    match op {
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge => Proof::TagProven(ValueTag::Boolean),
        BinaryOp::BitAnd
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::Shl
        | BinaryOp::Shr => Proof::TagProven(ValueTag::Integer),
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Mod | BinaryOp::FloorDiv => {
            match (tag_of(lhs), tag_of(rhs)) {
                (Some(ValueTag::Integer), Some(ValueTag::Integer)) => {
                    Proof::TagProven(ValueTag::Integer)
                }
                (Some(ValueTag::Integer) | Some(ValueTag::Float), Some(ValueTag::Integer) | Some(ValueTag::Float)) => {
                    Proof::TagProven(ValueTag::Float)
                }
                _ => Proof::None,
            }
        }
        BinaryOp::Div | BinaryOp::Pow => Proof::TagProven(ValueTag::Float),
        BinaryOp::Concat => Proof::TagProven(ValueTag::Object),
        // The result keeps whichever operand's (unproven-here) type is
        // actually selected at runtime - not modeled.
        BinaryOp::And | BinaryOp::Or => Proof::None,
    }
}

/// Forward dataflow pass populating `Function::proofs` from `Inst::Const`'s
/// own tag, arithmetic/comparison combination rules, and - for `Inst::Phi` -
/// a trivial-phi shortcut: when every incoming shares one `ValueId` (a loop
/// header's phi for a register the loop body never reassigns is trivial by
/// construction, since Phase A preallocates one phi per register at every
/// phi block regardless of whether that register is ever rewritten), the
/// phi's proof is simply that shared value's own proof. A genuinely
/// loop-varying phi (incomings disagree) stays `Proof::None`: its back-edge
/// incoming is defined later in block order than the phi itself, so a
/// single forward pass cannot yet know its proof - deliberately
/// conservative, matching this milestone's documented single-pass scope
/// (see the module doc).
pub fn propagate_proofs(func: &mut Function) {
    let mut proofs: HashMap<ValueId, Proof> = HashMap::new();
    for block in &func.blocks {
        for (id, inst) in &block.insts {
            let proof = match inst {
                Inst::Const(c) => const_proof(c),
                Inst::Unary(op, a) => unary_proof(*op, proofs.get(a).copied().unwrap_or(Proof::None)),
                Inst::Binary(op, a, b) => binary_proof(
                    *op,
                    proofs.get(a).copied().unwrap_or(Proof::None),
                    proofs.get(b).copied().unwrap_or(Proof::None),
                ),
                Inst::Phi { incomings, .. } => {
                    // Ignore self-referencing incomings (a register the loop
                    // body never reassigns carries the phi's own id back
                    // around the back edge) - the standard trivial-phi
                    // definition looks only at the *other* operands.
                    let non_self: Vec<ValueId> =
                        incomings.iter().map(|(_, v)| *v).filter(|v| v != id).collect();
                    match non_self.split_first() {
                        Some((first, rest)) if rest.iter().all(|v| v == first) => {
                            proofs.get(first).copied().unwrap_or(Proof::None)
                        }
                        _ => Proof::None,
                    }
                }
                Inst::Guard { .. } | Inst::Unsupported(_) => Proof::None,
            };
            proofs.insert(*id, proof);
        }
    }
    func.proofs = proofs;
}

/// Follows a chain of trivial phis (every incoming shares one `ValueId`,
/// see `propagate_proofs`'s doc) to the value it actually carries, or
/// returns `id` unchanged if it is not a trivial phi (or not a phi at all).
fn trivial_phi_source(func: &Function, id: ValueId) -> ValueId {
    for block in &func.blocks {
        for (vid, inst) in &block.insts {
            if *vid != id {
                continue;
            }
            if let Inst::Phi { incomings, .. } = inst {
                let non_self: Vec<ValueId> =
                    incomings.iter().map(|(_, v)| *v).filter(|v| *v != id).collect();
                if let Some((first, rest)) = non_self.split_first() {
                    if rest.iter().all(|v| v == first) {
                        return trivial_phi_source(func, *first);
                    }
                }
            }
            return id;
        }
    }
    id
}

/// The block index that defines `id`, found by scanning every block's
/// instructions. This module has no optimizing consumer yet (see the module
/// doc), so an O(blocks) scan here is not on any hot path.
fn def_block_index(func: &Function, id: ValueId) -> Option<usize> {
    func.blocks.iter().position(|block| block.insts.iter().any(|(vid, _)| *vid == id))
}

fn guard_fact_value(fact: &GuardFact) -> ValueId {
    match fact {
        GuardFact::TagIsInteger(id) => *id,
    }
}

fn with_guard_fact_value(fact: &GuardFact, id: ValueId) -> GuardFact {
    match fact {
        GuardFact::TagIsInteger(_) => GuardFact::TagIsInteger(id),
    }
}

/// Removes a block's own repeated `Guard`s that check the identical
/// `GuardFact` (same fact variant, same checked `ValueId`), keeping the
/// first occurrence.
fn fuse_block(block: &mut Block) {
    let mut seen: Vec<GuardFact> = Vec::new();
    block.insts.retain(|(_, inst)| match inst {
        Inst::Guard { fact, .. } => {
            if seen.contains(fact) {
                false
            } else {
                seen.push(*fact);
                true
            }
        }
        _ => true,
    });
}

/// Applies `fuse_block` to every block in `func`.
pub fn fuse_redundant_guards(func: &mut Function) {
    for block in &mut func.blocks {
        fuse_block(block);
    }
}

/// A natural loop recognized from `Function`'s terminators alone: `latch` is
/// a block whose terminator has an edge back to `header` (`target <=
/// latch`), and `preheader` is `header`'s other predecessor - the forward
/// edge into the loop, found by index below `header`. Only single-preheader,
/// single-latch loops (the shape this crate's own structured `if`/`while`
/// compiler emits) are recognized; anything else is simply not hoisted.
struct Loop {
    header: usize,
    latch: usize,
    preheader: usize,
}

fn terminator_targets(terminator: &Terminator) -> Vec<usize> {
    match terminator {
        Terminator::Jump(t) => vec![t.0 as usize],
        Terminator::Branch { when_true, when_false, .. } => {
            vec![when_true.0 as usize, when_false.0 as usize]
        }
        Terminator::Return(_) | Terminator::ImplicitReturn | Terminator::Unsupported(_) => vec![],
    }
}

fn find_loops(func: &Function) -> Vec<Loop> {
    let mut loops = Vec::new();
    for (b, block) in func.blocks.iter().enumerate() {
        let Some(terminator) = block.terminator.as_ref() else { continue };
        for target in terminator_targets(terminator) {
            if target > b {
                continue;
            }
            let header = target;
            let preheader = func.blocks[..header].iter().position(|pb| {
                pb.terminator.as_ref().is_some_and(|t| terminator_targets(t).contains(&header))
            });
            if let Some(preheader) = preheader {
                loops.push(Loop { header, latch: b, preheader });
            }
        }
    }
    loops
}

/// Sinks a loop body `Guard` to its loop's preheader when the fact it checks
/// resolves (through `trivial_phi_source`) to a `ValueId` defined strictly
/// before the loop header - an SSA value defined there is, by definition,
/// the same value on every iteration, so checking it once before the loop
/// ever runs is equivalent to re-checking it every iteration. Guards already
/// in the preheader are fused with any newly hoisted duplicate via
/// `fuse_block`.
///
/// Known imprecision, documented rather than silently assumed correct: a
/// hoisted guard keeps its original mid-loop-body `snapshot.resume_pc`
/// unchanged, which is only a safe resume point for the interpreter on a
/// *first-iteration* failure path; this module has no optimizing consumer
/// yet (see the module doc) to make resume-pc rewriting meaningful, so that
/// is left to whichever later work item (3+) actually wires hoisting to a
/// real Cranelift side exit.
pub fn hoist_loop_invariant_guards(func: &mut Function) {
    for lp in find_loops(func) {
        let mut hoisted: Vec<(ValueId, Inst)> = Vec::new();
        for b in lp.header..=lp.latch {
            let mut i = 0;
            while i < func.blocks[b].insts.len() {
                let invariant_source = match &func.blocks[b].insts[i].1 {
                    Inst::Guard { fact, .. } => {
                        let source = trivial_phi_source(func, guard_fact_value(fact));
                        match def_block_index(func, source) {
                            Some(db) if db < lp.header => Some(source),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                match invariant_source {
                    Some(source) => {
                        let (id, inst) = func.blocks[b].insts.remove(i);
                        let Inst::Guard { fact, snapshot, id: guard_id } = inst else {
                            unreachable!("invariant_source only set for Inst::Guard")
                        };
                        hoisted.push((
                            id,
                            Inst::Guard { id: guard_id, fact: with_guard_fact_value(&fact, source), snapshot },
                        ));
                    }
                    None => i += 1,
                }
            }
        }
        func.blocks[lp.preheader].insts.extend(hoisted);
        fuse_block(&mut func.blocks[lp.preheader]);
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
                    Inst::Guard { .. } => {
                        panic!("sol_ir toy interpreter: lift_proto never constructs a Guard")
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
    fn value_at_pc_records_every_arithmetic_instructions_operand_ids() {
        let proto = compile(b"local a = 3 local b = 4 return a * b + 1");
        let func = lift_proto(&proto);
        assert!(func.fully_lifted);
        // `local a = 3` / `local b = 4` each lower to a `LoadConst`, and
        // `a * b` / `+ 1` each lower to a `Binary` - all four are
        // value-producing and so must have a `value_at_pc` entry recording
        // their own result id and lifted `Inst`.
        let binary_pcs: Vec<usize> = proto
            .instrs
            .iter()
            .enumerate()
            .filter(|(_, instr)| matches!(instr, Instr::Binary(..) | Instr::IntegerBinary(..)))
            .map(|(pc, _)| pc)
            .collect();
        assert_eq!(binary_pcs.len(), 2, "fixture should lower to exactly two binary ops");
        for pc in binary_pcs {
            let (id, inst) = func.value_at_pc.get(&pc).expect("every Binary pc must be recorded");
            assert!(matches!(inst, Inst::Binary(..)), "recorded Inst must match what was lowered");
            assert!(
                func.blocks.iter().any(|b| b.insts.iter().any(|(vid, _)| vid == id)),
                "the recorded ValueId must actually be defined somewhere in the function"
            );
        }
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

    /// The `ValueId` returned by `Terminator::Return(Some(_))`, wherever in
    /// the function that terminator actually lives (not necessarily the
    /// entry block - a `while`-loop fixture returns from the block after the
    /// loop, not the entry block before it).
    fn find_return_value(func: &Function) -> ValueId {
        func.blocks
            .iter()
            .find_map(|block| match block.terminator.as_ref() {
                Some(Terminator::Return(Some(id))) => Some(*id),
                _ => None,
            })
            .expect("fixture returns a value")
    }

    #[test]
    fn proof_propagation_proves_arithmetic_result_tags() {
        let proto = compile(b"local a = 3 local b = 4 return a * b + 1");
        let mut func = lift_proto(&proto);
        propagate_proofs(&mut func);
        let return_id = find_return_value(&func);
        assert_eq!(func.proofs[&return_id], Proof::TagProven(ValueTag::Integer));
    }

    #[test]
    fn proof_propagation_proves_a_loop_invariant_phi_via_the_trivial_phi_shortcut() {
        let proto = compile(
            b"local limit = 10 local i = 0
              while i < limit do
                i = i + 1
              end
              return limit",
        );
        let mut func = lift_proto(&proto);
        propagate_proofs(&mut func);
        let return_id = find_return_value(&func);
        // `limit` is read-only inside the loop, so even though Phase A
        // preallocates a phi for it at the loop header, that phi's proof
        // must still resolve to Integer via the trivial-phi shortcut.
        assert_eq!(func.proofs[&return_id], Proof::TagProven(ValueTag::Integer));
    }

    #[test]
    fn proof_propagation_leaves_a_genuinely_varying_phi_unproven() {
        // `s` is reassigned every iteration from a prior `s`, so its loop
        // header phi's incomings disagree - a single forward pass cannot
        // know the back edge's proof yet, and must stay conservative.
        let proto = compile(
            b"local s = 0 local i = 1
              while i <= 10 do
                s = s + i
                i = i + 1
              end
              return s",
        );
        let mut func = lift_proto(&proto);
        propagate_proofs(&mut func);
        let return_id = find_return_value(&func);
        assert_eq!(func.proofs[&return_id], Proof::None);
    }

    #[test]
    fn hoist_loop_invariant_guards_sinks_a_read_only_registers_guard_to_the_preheader() {
        let proto = compile(
            b"local limit = 10 local i = 0
              while i < limit do
                i = i + 1
              end
              return i",
        );
        let func = lift_proto(&proto);
        // `limit`'s SSA value: find the Phi in the loop header for the
        // register that is never reassigned, i.e. whose incomings agree.
        let header = (1..func.blocks.len())
            .find(|&b| {
                func.blocks[b]
                    .insts
                    .iter()
                    .any(|(_, inst)| matches!(inst, Inst::Phi { .. }))
            })
            .expect("fixture has a loop header with preallocated phis");
        let limit_phi_id = func.blocks[header]
            .insts
            .iter()
            .find_map(|(id, inst)| match inst {
                // Ignore the self-referencing (back-edge) incoming that
                // every read-only register's phi carries around the loop -
                // see `propagate_proofs`'s own trivial-phi handling.
                Inst::Phi { incomings, .. }
                    if incomings.iter().filter(|(_, v)| v != id).all(|(_, v)| {
                        *v == incomings.iter().find(|(_, v)| v != id).unwrap().1
                    }) =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .expect("the loop has exactly one read-only register (limit)");

        let mut func = func;
        let loop_body = header + 1;
        let snapshot = DeoptSnapshot::full(0, proto.metadata.registers as usize);
        func.blocks[loop_body].insts.insert(
            0,
            (
                ValueId(u32::MAX),
                Inst::Guard {
                    id: GuardId(0),
                    fact: GuardFact::TagIsInteger(limit_phi_id),
                    snapshot,
                },
            ),
        );

        hoist_loop_invariant_guards(&mut func);

        let body_still_has_guard = func.blocks[loop_body]
            .insts
            .iter()
            .any(|(_, inst)| matches!(inst, Inst::Guard { .. }));
        assert!(!body_still_has_guard, "the invariant guard should have moved out of the loop body");

        let preheader = header - 1;
        let hoisted = func.blocks[preheader]
            .insts
            .iter()
            .find_map(|(_, inst)| match inst {
                Inst::Guard { fact: GuardFact::TagIsInteger(id), .. } => Some(*id),
                _ => None,
            })
            .expect("the guard should have been hoisted into the preheader");
        // Hoisting resolves the trivial phi to its underlying invariant
        // source, not the phi id itself.
        assert_ne!(hoisted, limit_phi_id);
    }

    #[test]
    fn fuse_redundant_guards_collapses_duplicate_checks_in_one_block() {
        let snapshot_a = DeoptSnapshot::full(0, 4);
        let snapshot_b = DeoptSnapshot::full(0, 4);
        let mut block = Block::default();
        block.insts.push((
            ValueId(0),
            Inst::Guard { id: GuardId(0), fact: GuardFact::TagIsInteger(ValueId(1)), snapshot: snapshot_a },
        ));
        block.insts.push((ValueId(2), Inst::Const(Const::Integer(1))));
        block.insts.push((
            ValueId(3),
            Inst::Guard { id: GuardId(1), fact: GuardFact::TagIsInteger(ValueId(1)), snapshot: snapshot_b },
        ));
        let mut func = Function {
            blocks: vec![block],
            entry: BlockId(0),
            proofs: HashMap::new(),
            fully_lifted: true,
            value_at_pc: HashMap::new(),
        };

        fuse_redundant_guards(&mut func);

        let guard_count = func.blocks[0]
            .insts
            .iter()
            .filter(|(_, inst)| matches!(inst, Inst::Guard { .. }))
            .count();
        assert_eq!(guard_count, 1, "the duplicate guard on the same fact should have been fused away");
    }

    #[test]
    fn deopt_snapshot_full_roots_every_register() {
        let snapshot = DeoptSnapshot::full(42, 3);
        assert_eq!(snapshot.resume_pc, 42);
        let frame = vec![sol_core::Value::NIL, sol_core::Value::NIL, sol_core::Value::NIL];
        let roots = snapshot.stack_map.roots(&frame).expect("every slot is in range");
        assert_eq!(roots.len(), 3, "DeoptSnapshot::full must name every register, not a liveness subset");
    }
}
