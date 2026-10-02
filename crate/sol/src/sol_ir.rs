//! Shared SSA IR scaffolding (U10 work items 1-2: see
//! `docs/features/milestones/u10-optimizing-jit-osr.md`).
//!
//! Lifts a bounded subset of `lua_bytecode::Proto` bytecode - constants,
//! unary/binary arithmetic and comparisons, `if`/`while`-shaped control flow
//! built from `Jump`/`JumpIfFalse`/`JumpIfTrue`, and (work item 7) a narrow
//! shape of monomorphic call site - into a block-structured SSA form.
//! Anything outside that subset (tables, globals, upvalues, numeric/generic
//! `for`, captured registers, polymorphic/megamorphic calls, ...) lowers to
//! an `Inst::Unsupported`/`Terminator::Unsupported` sentinel and marks the
//! lifted `Function` as not fully lifted, rather than guessing at semantics
//! it does not yet model.
//!
//! `propagate_proofs` populates `Function::proofs` from constant tags and
//! arithmetic/comparison combination rules (work item 2). `Inst::Guard`,
//! `GuardFact`, and `DeoptSnapshot` are also work item 2's; work item 7 is
//! the first thing that constructs a real `Inst::Guard` from actual
//! bytecode, pairing one with every `Inst::Call` it lifts (see that
//! variant's doc and `callee_is_inlinable`). `hoist_loop_invariant_guards`
//! and `fuse_redundant_guards` are real, tested IR transformations that
//! apply to those guards with no new logic of their own, exactly as item 6
//! anticipated.
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

use sol_core::{ObjectId, StackMap, ValueCount, ValueTag};

use crate::ast::{BinaryOp, UnaryOp};
use crate::lua_bytecode::{CallCacheEntry, Const, Instr, Proto, Reg};

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

/// A fact a `Guard` checks at runtime before later code may rely on it.
/// `TagIsInteger` is work item 2's original reservation; nothing in this
/// module constructs a real `Guard` from either variant yet - `lift_proto`
/// never emits `Inst::Guard` at all (see `Function::fully_lifted`'s doc), so
/// both are only exercised by the hand-built graphs this module's own tests
/// construct below.
///
/// `ClosureIdentity(id, guard)` is work item 6's reservation of the
/// call-site shape: "the closure value at `id` is the same object as
/// `guard`". `guard` is a call site's own `CallCacheEntry::guard`
/// (`lua_bytecode::instr`) - U8's existing generation-checked closure
/// identity, already the signal `closure_parts_cached`
/// (`lua_runtime/table.rs`) re-verifies on every cache hit. Work item 7
/// (real call inlining) is the first thing that constructs this variant from
/// real bytecode - see `Inst::Call`'s own doc and
/// `docs/features/milestones/u10-optimizing-jit-osr.md`'s item-6/7 notes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GuardFact {
    TagIsInteger(ValueId),
    ClosureIdentity(ValueId, ObjectId),
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
    /// Work item 7: a monomorphic `Instr::Call` site `lift_proto` has decided
    /// is safe to inline - `target` is that call site's own
    /// `BoundedCache::monomorphic_call_target()` resolution, re-derived (not
    /// cached across calls to `lift_proto`) every time this `Proto` is
    /// lifted, same "lift again, trust nothing stale" posture `opt_lower.rs`
    /// already uses for the whole `Proto`. Always immediately preceded in its
    /// block by an `Inst::Guard` on `GuardFact::ClosureIdentity` checking the
    /// closure actually invoked at this call site against `target.guard` -
    /// `lift_proto` never emits one without the other. See
    /// `callee_is_inlinable` for exactly which `target` shapes qualify (the
    /// callee itself must be straight-line/no-further-calls/no-upvalues,
    /// fixed arity matching this call's own argument count, and - the
    /// narrowest restriction, see that function's own doc - every instruction
    /// in its single block is a `Const` or a proof-provably integer
    /// `Add`/`Sub`/`Mul` `Binary`, nothing else); anything else stays an
    /// ordinary un-inlined call (`Inst::Unsupported`).
    ///
    /// `param_value_ids`, `body`, and `result` are `callee_is_inlinable`'s own
    /// resolved `InlinePlan`, carried here rather than re-derived by
    /// `opt_lower.rs`: `body` is the callee's already-lifted, already-proof-
    /// checked `blocks[0].insts` (every parameter's nil-seed `ValueId` still
    /// present positionally, per-element order preserved so each operand is
    /// already defined by the time a later entry references it), `result` is
    /// the `ValueId` `blocks[0]`'s own `Return` yields (`None` for a
    /// zero-result callee), and `param_value_ids[i]` is the `ValueId` the
    /// caller's `i`th argument register must be substituted for when
    /// splicing. `opt_lower.rs` is the only consumer that actually splices
    /// `body` into Cranelift IR, mechanically translating each `Const`/
    /// `Binary` entry in turn - this variant only records *that* inlining is
    /// sound and *what* to splice, not the Cranelift IR itself.
    Call {
        target: CallCacheEntry,
        param_value_ids: Vec<ValueId>,
        body: Vec<(ValueId, Inst)>,
        /// Work item 7b (inline-site bookkeeping): `body`'s own
        /// `ValueId -> callee bytecode pc` chain, for every `body` entry that
        /// actually originated from a real callee instruction (everything
        /// except the callee's own parameter nil-seeds - see
        /// `callee_is_inlinable`'s doc on how those are found positionally;
        /// they have no entry here because they are not themselves a callee
        /// bytecode position, only a substitution point for the caller's own
        /// argument values). Built once, by inverting the callee's own
        /// `Function::value_at_pc` during `callee_is_inlinable`, and carried
        /// here unchanged - `opt_lower.rs`'s `lower_call` copies it verbatim
        /// into `Proto::inline_map` (`lua_bytecode::instr::InlineMapEntry`)
        /// rather than re-deriving it.
        body_pcs: HashMap<ValueId, usize>,
        result: Option<ValueId>,
    },
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
///
/// Thin wrapper over `lift_proto_impl(proto, true)` - callers get to attempt
/// work item 7's call inlining. The only other caller of `lift_proto_impl`
/// is `callee_is_inlinable` itself, which always passes `false`: a callee
/// being considered for inlining is never itself allowed to inline a further
/// call, which is what keeps that mutual check from recursing more than one
/// level deep (see that function's own doc).
pub fn lift_proto(proto: &Proto) -> Function {
    lift_proto_impl(proto, true)
}

fn lift_proto_impl(proto: &Proto, inline_calls: bool) -> Function {
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
    let mut next_guard_id: u32 = 0;
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
                Instr::Call(base, nargs_vc, nresults_vc) => {
                    let resolved = inline_calls
                        .then(|| match (nargs_vc, nresults_vc) {
                            (ValueCount::Fixed(nargs), ValueCount::Fixed(nresults)) if *nresults <= 1 => {
                                let arg_proofs: Vec<Proof> = (0..*nargs)
                                    .map(|i| {
                                        let reg = *base + 1 + i as Reg;
                                        infer_known_proof(&blocks, current[&reg])
                                    })
                                    .collect();
                                proto
                                    .call_cache
                                    .get(pc)
                                    .and_then(|cache| cache.monomorphic_call_target())
                                    .and_then(|entry| {
                                        callee_is_inlinable(&entry, *nargs, *nresults, &arg_proofs)
                                            .map(|plan| (entry, plan, *nresults))
                                    })
                            }
                            _ => None,
                        })
                        .flatten();
                    match resolved {
                        Some((entry, plan, nresults)) => {
                            let guard_id = GuardId(next_guard_id);
                            next_guard_id += 1;
                            blocks[b].insts.push((
                                alloc.next(),
                                Inst::Guard {
                                    id: guard_id,
                                    fact: GuardFact::ClosureIdentity(current[base], entry.guard),
                                    snapshot: DeoptSnapshot::full(pc, num_registers),
                                },
                            ));
                            let call_id = alloc.next();
                            let call_inst = Inst::Call {
                                target: entry,
                                param_value_ids: plan.param_value_ids,
                                body: plan.body,
                                body_pcs: plan.body_pcs,
                                result: plan.result,
                            };
                            blocks[b].insts.push((call_id, call_inst.clone()));
                            value_at_pc.insert(pc, (call_id, call_inst));
                            if nresults == 1 {
                                current.insert(*base, call_id);
                            }
                        }
                        None => {
                            blocks[b].insts.push((alloc.next(), Inst::Unsupported(pc)));
                            fully_lifted = false;
                        }
                    }
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

/// Whether `entry`'s resolved callee can be spliced directly into a caller's
/// compiled body in place of a real call (work item 7's narrow first slice
/// of real inlining - see `Inst::Call`'s own doc).
///
/// Four structural requirements, all independent of any later Cranelift
/// codegen concern: `entry.prototype` takes exactly `nargs` fixed
/// (non-variadic) parameters and this call site passes exactly `nresults`
/// (0 or 1) results - no nil-padding/truncation logic is built, so arity
/// must match exactly; it captures no upvalues of its own (inlining does not
/// thread a closure's captured cells through yet); and its own body must be
/// `lift_proto_impl(_, false)`-fully-lifted *and* have its entry block
/// (`blocks[0]`) terminate directly in a `Return`/`ImplicitReturn` of the
/// right arity, never a `Jump`/`Branch` - stricter than `fully_lifted` alone
/// allows (which also permits an `if`/`while` shape spread across several
/// blocks), because splicing a callee's own internal control flow into the
/// caller's compiled body is not attempted by this item: a callee whose
/// entry block branches or loops is simply not inlined, falling back to the
/// ordinary call path, same as any other out-of-scope shape. This is
/// deliberately a check on `blocks[0]`'s own terminator, not on
/// `blocks.len()`: the compiler always emits per-scope-exit cleanup
/// (`DetachCell`/`LoadNil` for every local, plus a trailing defensive
/// `Return`) *after* a function's own explicit `return`, which
/// `compute_block_starts` always splits into further block(s) - unreachable
/// from `blocks[0]` (nothing jumps to them), fully within the bounded lift
/// subset, and irrelevant to splicing (only `blocks[0]` is ever spliced), but
/// present in nearly every real compiled function regardless of whether its
/// own logic branches at all. `lift_proto_impl(_, false)` - never
/// `lift_proto`/`lift_proto_impl(_, true)` - is what keeps a
/// mutually-recursive pair of monomorphic call sites (A inlines B, B inlines
/// A) from recursing without bound: a callee under consideration here is
/// never itself allowed to model a further call, regardless of its own
/// call-site cache state.
///
/// The narrowest restriction is the last one: `blocks[0]` may only contain
/// `Const` (never a string constant - `opt_lower.rs`'s whole native tier
/// excludes those, same as its own `is_eligible`) and proof-provably-
/// `Integer` `Add`/`Sub`/`Mul` `Binary` - nothing else, not even `Not`/`Neg`/
/// `BitNot` (which `lift_proto_impl` does lower in general, but which this
/// first inlining increment deliberately does not attempt to splice - see
/// `Inst::Call`'s own doc). This is deliberately narrower than "whatever
/// `opt_lower::is_eligible` already lowers": `opt_lower.rs` splices this
/// callee's body as bare Cranelift SSA values threaded between caller
/// registers, never through a flat register window of its own, so only
/// instructions with a value-in/value-out shape (not `opt_lower`'s existing
/// register-indexed stubs, e.g. `dynjit_binary`'s unproven fallback) can be
/// spliced at all; widening this (unary ops, a value-shaped binary stub) is
/// future work, not attempted here.
///
/// `arg_proofs` (one entry per fixed argument, caller order) is what keeps
/// this check from being vacuous: `lift_proto_impl` always seeds a fresh
/// `Proto`'s block-0 registers - including a callee's own parameter
/// registers - with an unconditional `Const::Nil` (see that seeding's own
/// doc), since nothing before this work item ever needed a *callee's own*
/// freshly lifted parameters to carry a meaningful proof. Left uncorrected,
/// every parameter would look proof-`Nil`, so `return a + b` (the common
/// case) could never pass the check above - only constant-only arithmetic
/// could. `propagate_proofs_with_param_seed` corrects exactly that: it
/// reuses `propagate_proofs`'s own per-`Inst` rules unchanged, except that
/// each parameter register's nil-seed `ValueId` takes its proof from
/// `arg_proofs` instead of `const_proof(&Const::Nil)`. Those nil-seed ids are
/// found positionally - `blocks[0].insts[0..nargs]` - rather than by
/// assuming any particular numeric `ValueId` layout: Phase A can allocate
/// phi ids for *other* (always-unreachable-from-`blocks[0]`, see this
/// function's own doc) blocks before block 0's seeding loop runs, so
/// `ValueId(r) == r` does not hold in general, but block 0 itself is never a
/// phi block and nothing touches `blocks[0].insts` before that seeding loop
/// finishes, so the first `nargs` entries there are always exactly the
/// parameter registers' nil-seeds, in register order. `arg_proofs` itself is
/// computed by the call site (`infer_known_proof`, over the *caller's* own
/// partially-built instruction list) from the actual argument expressions
/// this call passes - a constant argument, or one built from earlier
/// proven-integer arithmetic, carries a real proof here; anything the caller
/// can't yet determine (another call's result, a table/global load, a
/// loop-carried phi, ...) conservatively stays `Proof::None`, which can only
/// make this check *reject* more often, never accept unsoundly.
/// `callee_is_inlinable`'s resolved splice plan, carried into `Inst::Call`
/// unchanged - see that variant's own doc.
struct InlinePlan {
    param_value_ids: Vec<ValueId>,
    body: Vec<(ValueId, Inst)>,
    body_pcs: HashMap<ValueId, usize>,
    result: Option<ValueId>,
}

fn callee_is_inlinable(entry: &CallCacheEntry, nargs: u32, nresults: u32, arg_proofs: &[Proof]) -> Option<InlinePlan> {
    let callee_proto = &entry.prototype;
    if callee_proto.metadata.arity.variadic || callee_proto.metadata.arity.parameters != nargs {
        return None;
    }
    if nresults > 1 || !callee_proto.upvals.is_empty() {
        return None;
    }
    let mut callee_func = lift_proto_impl(callee_proto, false);
    if !callee_func.fully_lifted || callee_func.blocks.is_empty() {
        return None;
    }
    let return_shape_ok = matches!(
        (&callee_func.blocks[0].terminator, nresults),
        (Some(Terminator::Return(Some(_))), 1)
            | (Some(Terminator::Return(None)), 0)
            | (Some(Terminator::ImplicitReturn), 0)
    );
    if !return_shape_ok {
        return None;
    }
    let param_value_ids: Vec<ValueId> =
        callee_func.blocks[0].insts[..nargs as usize].iter().map(|(id, _)| *id).collect();
    propagate_proofs_with_param_seed(&mut callee_func, &param_value_ids, arg_proofs);
    let body_ok = callee_func.blocks[0].insts.iter().all(|(_, inst)| match inst {
        Inst::Const(c) => !matches!(c, Const::Str(_)),
        Inst::Binary(op, a, b) => {
            matches!(op, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)
                && tag_of(callee_func.proofs.get(a).copied().unwrap_or(Proof::None)) == Some(ValueTag::Integer)
                && tag_of(callee_func.proofs.get(b).copied().unwrap_or(Proof::None)) == Some(ValueTag::Integer)
        }
        _ => false,
    });
    if !body_ok {
        return None;
    }
    let result = match (&callee_func.blocks[0].terminator, nresults) {
        (Some(Terminator::Return(Some(id))), 1) => Some(*id),
        _ => None,
    };
    // Work item 7b: invert the callee's own `value_at_pc` (pc -> (id, inst))
    // once, then keep only the entries `blocks[0]`'s own instruction list
    // actually contains - this naturally excludes the `nargs` parameter
    // nil-seeds at the front of `blocks[0].insts` (seeded directly by the
    // register-0 seeding loop in `lift_proto_impl`, never recorded in
    // `value_at_pc` - see that loop's own comment), leaving exactly the
    // callee bytecode positions `body_ok`'s check above already confirmed are
    // real `Const`/`Binary` instructions.
    let pc_of_value: HashMap<ValueId, usize> =
        callee_func.value_at_pc.iter().map(|(pc, (id, _))| (*id, *pc)).collect();
    let body_pcs: HashMap<ValueId, usize> = callee_func.blocks[0]
        .insts
        .iter()
        .filter_map(|(id, _)| pc_of_value.get(id).map(|pc| (*id, *pc)))
        .collect();
    Some(InlinePlan {
        param_value_ids,
        body: callee_func.blocks[0].insts.clone(),
        body_pcs,
        result,
    })
}

/// A conservative, recursive proof lookup over a (possibly still
/// mid-construction) instruction list, for exactly one purpose: letting a
/// call site's own already-lifted argument expressions seed
/// `callee_is_inlinable`'s check of the callee's parameters (see that
/// function's doc). Unlike `propagate_proofs` (a whole-`Function`, run-once
/// forward pass caching every value's proof in one map), this walks straight
/// to `id`'s own producing `Inst` and recurses into its operands on demand -
/// safe to call mid-`lift_proto_impl` (before that block's own producer list
/// is even finished) because it only ever looks at instructions already
/// pushed into `blocks`, which by Phase B's forward-scan invariant is
/// guaranteed to already include `id`'s producer (a register's current
/// `ValueId` is never bound to something not yet lifted). Anything this
/// cannot yet resolve - most importantly `Inst::Phi` (a loop-carried or
/// join-point register; its `incomings` are not filled in until the final
/// pass at the very end of `lift_proto_impl`) - stays conservatively
/// `Proof::None`, same as `propagate_proofs` does for every genuinely
/// loop-varying phi.
fn infer_known_proof(blocks: &[Block], id: ValueId) -> Proof {
    for block in blocks {
        for (vid, inst) in &block.insts {
            if *vid != id {
                continue;
            }
            return match inst {
                Inst::Const(c) => const_proof(c),
                Inst::Unary(op, a) => unary_proof(*op, infer_known_proof(blocks, *a)),
                Inst::Binary(op, a, b) => {
                    binary_proof(*op, infer_known_proof(blocks, *a), infer_known_proof(blocks, *b))
                }
                Inst::Phi { .. } | Inst::Guard { .. } | Inst::Call { .. } | Inst::Unsupported(_) => Proof::None,
            };
        }
    }
    Proof::None
}

/// `propagate_proofs`'s own per-`Inst` rules, except each `ValueId` in
/// `param_value_ids` (`param_value_ids[i]`'s proof comes from
/// `arg_proofs[i]`) takes its seeded proof instead of whatever its actual
/// producing `Inst` would otherwise compute (always `Inst::Const(Const::Nil)`
/// in practice - see `callee_is_inlinable`'s doc for why this seed exists and
/// how it locates those ids).
fn propagate_proofs_with_param_seed(func: &mut Function, param_value_ids: &[ValueId], arg_proofs: &[Proof]) {
    let mut proofs: HashMap<ValueId, Proof> = HashMap::new();
    for block in &func.blocks {
        for (id, inst) in &block.insts {
            let seeded = param_value_ids.iter().position(|p| p == id).map(|i| arg_proofs[i]);
            let proof = if let Some(seeded) = seeded {
                seeded
            } else {
                match inst {
                    Inst::Const(c) => const_proof(c),
                    Inst::Unary(op, a) => unary_proof(*op, proofs.get(a).copied().unwrap_or(Proof::None)),
                    Inst::Binary(op, a, b) => binary_proof(
                        *op,
                        proofs.get(a).copied().unwrap_or(Proof::None),
                        proofs.get(b).copied().unwrap_or(Proof::None),
                    ),
                    Inst::Phi { incomings, .. } => {
                        let non_self: Vec<ValueId> =
                            incomings.iter().map(|(_, v)| *v).filter(|v| v != id).collect();
                        match non_self.split_first() {
                            Some((first, rest)) if rest.iter().all(|v| v == first) => {
                                proofs.get(first).copied().unwrap_or(Proof::None)
                            }
                            _ => Proof::None,
                        }
                    }
                    Inst::Guard { .. } | Inst::Call { .. } | Inst::Unsupported(_) => Proof::None,
                }
            };
            proofs.insert(*id, proof);
        }
    }
    func.proofs = proofs;
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
                Inst::Guard { .. } | Inst::Call { .. } | Inst::Unsupported(_) => Proof::None,
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
        GuardFact::ClosureIdentity(id, _) => *id,
    }
}

fn with_guard_fact_value(fact: &GuardFact, id: ValueId) -> GuardFact {
    match fact {
        GuardFact::TagIsInteger(_) => GuardFact::TagIsInteger(id),
        GuardFact::ClosureIdentity(_, guard) => GuardFact::ClosureIdentity(id, *guard),
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
                    Inst::Call { .. } => {
                        panic!(
                            "sol_ir toy interpreter: inlined calls are exercised through \
                             opt_lower/dispatch differential tests, not this toy interpreter"
                        )
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

    /// Finds the fixture's single `Instr::Call` and warms its `call_cache`
    /// directly (rather than by actually running the program, which would
    /// warm a *different* `Rc<Proto>` than the one this test holds - see
    /// `lua_bytecode::BoundedCache::insert`'s own public API, which exists
    /// for exactly this kind of targeted test setup already, e.g.
    /// `instr::monomorphic_call_target_tests`).
    fn warm_call_cache(proto: &Proto, guard_raw: u64) -> usize {
        let call_pc = proto
            .instrs
            .iter()
            .position(|i| matches!(i, Instr::Call(..)))
            .expect("fixture calls exactly one function");
        let callee = proto.nested[0].clone();
        proto.call_cache[call_pc].insert(CallCacheEntry {
            guard: sol_core::ObjectId::from_raw(guard_raw).expect("nonzero raw id"),
            prototype: callee,
            upvalues: Rc::from(Vec::new()),
        });
        call_pc
    }

    /// Whether `pc` lifted to a real inlined call (`Inst::Call`, always
    /// paired with a preceding `Inst::Guard` on `GuardFact::ClosureIdentity`
    /// - see `Inst::Call`'s own doc) rather than falling back to
    /// `Inst::Unsupported(pc)`. Checked directly against this one call site,
    /// never against `Function::fully_lifted` for the whole caller: every
    /// fixture below defines its callee as a `local function` in the same
    /// chunk, whose `NewClosure` is itself outside the bounded subset this
    /// module models (sol_ir has no closure-creation support at all, a
    /// limitation this work item does not touch) - so `fully_lifted` is
    /// always already `false` for these fixtures regardless of whether the
    /// call site itself inlined, and asserting on it would vacuously pass.
    fn call_site_inlined(func: &Function, call_pc: usize) -> bool {
        match func.value_at_pc.get(&call_pc) {
            Some((_, Inst::Call { .. })) => true,
            Some((_, other)) => panic!("unexpected lifted Inst at the call site's own pc: {other:?}"),
            None => {
                let unsupported = func
                    .blocks
                    .iter()
                    .any(|b| b.insts.iter().any(|(_, inst)| matches!(inst, Inst::Unsupported(pc) if *pc == call_pc)));
                assert!(unsupported, "call site pc must lift to either Inst::Call or Inst::Unsupported");
                false
            }
        }
    }

    #[test]
    fn monomorphic_straight_line_integer_callee_lifts_to_guarded_inline_call() {
        let proto = compile(b"local function add(a, b) return a + b end local r = add(1, 2) return r");
        let call_pc = warm_call_cache(&proto, 1);

        let func = lift_proto(&proto);
        assert!(call_site_inlined(&func, call_pc), "a monomorphic straight-line integer callee should inline");

        let (call_id, _) = func.value_at_pc.get(&call_pc).unwrap();
        let block = func
            .blocks
            .iter()
            .find(|b| b.insts.iter().any(|(id, _)| id == call_id))
            .expect("the recorded call value must be defined in some block");
        let call_pos = block.insts.iter().position(|(id, _)| id == call_id).unwrap();
        assert!(
            call_pos > 0
                && matches!(
                    block.insts[call_pos - 1].1,
                    Inst::Guard { fact: GuardFact::ClosureIdentity(..), .. }
                ),
            "every inlined Inst::Call must be immediately preceded by its own ClosureIdentity guard"
        );
    }

    /// Work item 7b (inline-site bookkeeping): `Inst::Call::body_pcs` must
    /// attribute the one spliced arithmetic instruction to the callee's own
    /// real bytecode `pc` - not the caller's call-site `pc`, and not absent -
    /// while the callee's two parameter nil-seeds (never real callee
    /// instructions, see `body_pcs`'s own doc) must have no entry at all.
    #[test]
    fn body_pcs_attributes_the_spliced_binary_to_the_callees_own_bytecode_pc() {
        let proto = compile(b"local function add(a, b) return a + b end local r = add(1, 2) return r");
        let call_pc = warm_call_cache(&proto, 1);
        let callee = proto.nested[0].clone();

        let func = lift_proto(&proto);
        let (_, call_inst) = func.value_at_pc.get(&call_pc).expect("call site must be inlined");
        let Inst::Call { param_value_ids, body, body_pcs, .. } = call_inst else {
            panic!("expected Inst::Call, got {call_inst:?}");
        };

        // Independently re-derive the callee's own expected pc by lifting it
        // directly (never trusting the already-built map under test).
        let callee_func = lift_proto_impl(&callee, false);
        let expected_binary_pc = callee
            .instrs
            .iter()
            .position(|i| matches!(i, Instr::Binary(..) | Instr::IntegerBinary(..)))
            .expect("callee's body is `a + b`, a single Binary instruction");
        let (expected_binary_id, _) = callee_func
            .value_at_pc
            .get(&expected_binary_pc)
            .expect("the callee's own lift must record that pc");

        assert_eq!(body_pcs.len(), 1, "only the one real Binary instruction should be attributed, not the param seeds");
        assert_eq!(
            body_pcs.get(expected_binary_id),
            Some(&expected_binary_pc),
            "the spliced Binary's ValueId must map back to the callee's own bytecode pc"
        );
        for param_id in param_value_ids {
            assert!(
                !body_pcs.contains_key(param_id),
                "a parameter nil-seed is not a real callee instruction and must have no body_pcs entry"
            );
        }
        // `body_pcs` must only ever reference `ValueId`s `body` itself
        // contains - no stray/unused entries.
        for id in body_pcs.keys() {
            assert!(body.iter().any(|(bid, _)| bid == id), "every body_pcs key must also appear in body");
        }
    }

    #[test]
    fn polymorphic_call_site_is_not_inlined() {
        let proto = compile(b"local function add(a, b) return a + b end local r = add(1, 2) return r");
        let call_pc = warm_call_cache(&proto, 1);
        // A second, distinct identity at the same call site makes it
        // polymorphic - `monomorphic_call_target` must now refuse to resolve
        // it, regardless of whether both identities happen to share a
        // prototype.
        let callee = proto.nested[0].clone();
        proto.call_cache[call_pc].insert(CallCacheEntry {
            guard: sol_core::ObjectId::from_raw(2).unwrap(),
            prototype: callee,
            upvalues: Rc::from(Vec::new()),
        });

        let func = lift_proto(&proto);
        assert!(!call_site_inlined(&func, call_pc), "a polymorphic call site must not be inlined");
    }

    #[test]
    fn callee_needing_unproven_arithmetic_is_not_inlined() {
        let proto = compile(br#"local function add(a, b) return a + b end local r = add("x", "y") return r"#);
        let call_pc = warm_call_cache(&proto, 1);

        let func = lift_proto(&proto);
        assert!(
            !call_site_inlined(&func, call_pc),
            "add(\"x\", \"y\") has no provably-integer operands, so it must not be inlined"
        );
    }

    #[test]
    fn callee_with_internal_branch_is_not_inlined() {
        let proto = compile(
            b"local function abs(a)
                if a < 0 then return -a end
                return a
              end
              local r = abs(1)
              return r",
        );
        let call_pc = warm_call_cache(&proto, 1);

        let func = lift_proto(&proto);
        assert!(
            !call_site_inlined(&func, call_pc),
            "a callee with internal control flow is outside this item's narrow single-block shape"
        );
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
    fn fuse_redundant_guards_collapses_duplicate_closure_identity_checks_but_not_distinct_identities() {
        let snapshot_a = DeoptSnapshot::full(0, 4);
        let snapshot_b = DeoptSnapshot::full(0, 4);
        let snapshot_c = DeoptSnapshot::full(0, 4);
        let guard_x = ObjectId::from_raw(1).unwrap();
        let guard_y = ObjectId::from_raw(2).unwrap();
        let mut block = Block::default();
        block.insts.push((
            ValueId(0),
            Inst::Guard {
                id: GuardId(0),
                fact: GuardFact::ClosureIdentity(ValueId(1), guard_x),
                snapshot: snapshot_a,
            },
        ));
        block.insts.push((
            ValueId(2),
            Inst::Guard {
                id: GuardId(1),
                fact: GuardFact::ClosureIdentity(ValueId(1), guard_x),
                snapshot: snapshot_b,
            },
        ));
        // A guard against the same register but a *different* identity is a
        // genuinely different fact (e.g. a polymorphic call site re-checked
        // after a deopt) and must survive fusion.
        block.insts.push((
            ValueId(3),
            Inst::Guard {
                id: GuardId(2),
                fact: GuardFact::ClosureIdentity(ValueId(1), guard_y),
                snapshot: snapshot_c,
            },
        ));
        let mut func = Function {
            blocks: vec![block],
            entry: BlockId(0),
            proofs: HashMap::new(),
            fully_lifted: true,
            value_at_pc: HashMap::new(),
        };

        fuse_redundant_guards(&mut func);

        let facts: Vec<GuardFact> = func.blocks[0]
            .insts
            .iter()
            .filter_map(|(_, inst)| match inst {
                Inst::Guard { fact, .. } => Some(*fact),
                _ => None,
            })
            .collect();
        assert_eq!(
            facts,
            vec![
                GuardFact::ClosureIdentity(ValueId(1), guard_x),
                GuardFact::ClosureIdentity(ValueId(1), guard_y),
            ],
            "same-identity duplicate must fuse away, but the distinct identity must remain"
        );
    }

    #[test]
    fn hoist_loop_invariant_guards_sinks_a_closure_identity_guard_to_the_preheader() {
        // Same fixture shape as the `TagIsInteger` hoist test above -
        // `hoist_loop_invariant_guards` is generic over `GuardFact` via
        // `guard_fact_value`/`with_guard_fact_value`, so this only needs to
        // show the new variant takes the identical path.
        let proto = compile(
            b"local limit = 10 local i = 0
              while i < limit do
                i = i + 1
              end
              return i",
        );
        let func = lift_proto(&proto);
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
        let guard = ObjectId::from_raw(7).unwrap();
        func.blocks[loop_body].insts.insert(
            0,
            (
                ValueId(u32::MAX),
                Inst::Guard {
                    id: GuardId(0),
                    fact: GuardFact::ClosureIdentity(limit_phi_id, guard),
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
                Inst::Guard { fact: GuardFact::ClosureIdentity(id, g), .. } if *g == guard => Some(*id),
                _ => None,
            })
            .expect("the guard should have been hoisted into the preheader");
        assert_ne!(hoisted, limit_phi_id);
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
