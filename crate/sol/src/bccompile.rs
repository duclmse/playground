// Typed AST -> bytecode.rs for tier 0's interpreter.
//
// Register allocation is deliberately naive: each named local keeps its
// `LocalId` as its register; each temporary gets a fresh register that resets
// to `local_count` after each statement. Trivially correct, not space-optimal
// - fine since hot functions promote to Cranelift anyway.
//
// A function needing >255 registers is never bytecode-compiled - `tier.rs`
// sends it straight to native instead.

use std::collections::HashMap;

use crate::ast::BinaryOp;
use crate::bytecode::{BcFunction, Instr, LocalDebug, Op};
use crate::types::*;
use crate::value;

/// Maps each function name to an id (`Op::Call`'s `B` operand, capped at 255).
pub fn function_index(program: &TProgram) -> Result<HashMap<String, u8>, String> {
    let total = program.functions.len() + program.externs.len();
    if total > 256 {
        return Err("bytecode interpreter supports at most 256 functions".to_string());
    }
    let mut ids: HashMap<String, u8> = program
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), i as u8))
        .collect();
    let base = program.functions.len();
    ids.extend(
        program
            .externs
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), (base + i) as u8)),
    );
    Ok(ids)
}

/// `None` if `f` exceeds the interpreter's register-field limits.
pub fn compile_function(f: &TFunction, func_ids: &HashMap<String, u8>) -> Option<BcFunction> {
    if f.local_count > 256 || f.params.len() > 255 {
        return None;
    }
    let mut b = Builder {
        code: Vec::new(),
        lines: Vec::new(),
        current_line: f.source_line,
        consts: Vec::new(),
        next_reg: f.local_count,
        max_reg: f.local_count,
        func_ids,
        breaks: vec![],
        literals: vec![],
        local_names: &f.local_names,
        debug_locals: Vec::new(),
    };
    let mut top_level_loops = Vec::new();
    if !b.compile_block(&f.body, f.local_count, Some(&mut top_level_loops)) {
        // Defensive fallback (typeck guarantees every path returns) so a
        // violated invariant can't run off the end of `code`.
        b.emit(Instr::iabc(Op::Return, 0, 0, 0));
    }
    if b.max_reg > 256 {
        return None;
    }
    // U12 item 3: a real per-instruction SourceMap, built from the line each
    // instruction's originating `TStmt` carried through `TBlock` (see
    // `types.rs`'s `TBlock` doc comment). Before this, every typed `.sol`
    // function got `SourceMap::single_line(..., f.source_line)` - every pc
    // mapped to the function's *definition* line, so a debugger could never
    // distinguish one statement from another inside the same function (see
    // `docs/features/milestones/u12-wasm-playground.md`'s Work item 2
    // section). `b.lines` is built 1:1 with `b.code` by `emit()`, so this
    // mapping is exact at every instruction boundary the interpreter can
    // stop at - including the inline operand words some instructions emit
    // (e.g. `Box`'s tag/pointer-mask words), which just inherit the
    // enclosing instruction's line and are never themselves a valid `pc` for
    // the interpreter to stop at.
    debug_assert_eq!(b.lines.len(), b.code.len());
    for (id, _) in &f.params {
        if let Some(index) = b.record_local(*id, 0) {
            b.debug_locals[index].end_pc = b.code.len() as u32;
        }
    }
    let source_map = sol_core::SourceMap::new(
        b.lines
            .iter()
            .map(|&line| sol_core::SourceLocation::new(line, 0))
            .collect(),
    );
    Some(BcFunction {
        metadata: sol_core::PrototypeMetadata::new(&f.name, f.params.len(), false, b.max_reg)
            .expect("typed bytecode metadata fits the compiler's narrower register limits"),
        source_map,
        source_file: f.source_file.clone(),
        source_line: f.source_line,
        source_span: f.source_span,
        code: b.code,
        consts: b.consts,
        local_count: f.local_count,
        debug_locals: b.debug_locals,
        top_level_loops,
        literals: b.literals,
    })
}

struct Builder<'a> {
    code: Vec<Instr>,
    /// Parallel to `code`: `lines[pc]` is the source line that produced
    /// `code[pc]`, maintained 1:1 by `emit()`. Fed into `sol_core::SourceMap`
    /// at the end of `compile_function` (U12 item 3).
    lines: Vec<u32>,
    /// The line of the `TStmt` currently being compiled; every `emit()`
    /// records this. Set by `compile_block` before each statement.
    current_line: u32,
    consts: Vec<u64>,
    next_reg: usize,
    max_reg: usize,
    func_ids: &'a HashMap<String, u8>,
    breaks: Vec<Vec<usize>>,
    literals: Vec<Box<[u64]>>,
    local_names: &'a [String],
    debug_locals: Vec<LocalDebug>,
}

impl<'a> Builder<'a> {
    fn record_local(&mut self, id: usize, start_pc: usize) -> Option<usize> {
        let name = self.local_names.get(id)?;
        let index = self.debug_locals.len();
        self.debug_locals.push(LocalDebug {
            local_id: id,
            name: name.clone(),
            start_pc: start_pc as u32,
            end_pc: u32::MAX,
        });
        Some(index)
    }

    fn close_local_scope(&mut self, start: usize) {
        for local in &mut self.debug_locals[start..] {
            if local.end_pc == u32::MAX {
                local.end_pc = self.code.len() as u32;
            }
        }
    }
    fn emit(&mut self, i: Instr) -> usize {
        self.code.push(i);
        self.lines.push(self.current_line);
        self.code.len() - 1
    }

    fn alloc(&mut self) -> u8 {
        let r = self.next_reg;
        self.next_reg += 1;
        self.max_reg = self.max_reg.max(self.next_reg);
        r as u8
    }

    fn const_slot(&mut self, bits: u64) -> u16 {
        // Linear scan is fine: small pools, runs once at compile time.
        if let Some(i) = self.consts.iter().position(|&c| c == bits) {
            return i as u16;
        }
        self.consts.push(bits);
        (self.consts.len() - 1) as u16
    }

    /// Compiles a block; `next_reg` resets to `floor` after each statement.
    /// `top_level_loops` is `Some` only at the function's outermost call, so
    /// nested loops are never recorded (OSR only handles top-level loops).
    fn compile_block(
        &mut self,
        stmts: &TBlock,
        floor: usize,
        mut top_level_loops: Option<&mut Vec<(usize, usize)>>,
    ) -> bool {
        let scope_start = self.debug_locals.len();
        for (i, (line, s)) in stmts.iter().enumerate() {
            self.current_line = *line;
            let (terminated, loop_header) = self.compile_stmt(s, floor);
            if let (Some(loops), Some(header)) = (top_level_loops.as_deref_mut(), loop_header) {
                loops.push((header, i));
            }
            if terminated {
                self.close_local_scope(scope_start);
                return true; // a Return - nothing valid follows it
            }
        }
        self.close_local_scope(scope_start);
        false
    }

    /// `(terminated, loop_header_pc)`: terminated iff a `Return`;
    /// loop_header_pc is `Some` iff a `While`/`NumericFor`.
    fn compile_stmt(&mut self, stmt: &TStmt, floor: usize) -> (bool, Option<usize>) {
        let mut loop_header = None;
        match stmt {
            TStmt::Break => {
                let jump = self.emit(Instr::iasbx(Op::Jump, 0, 0));
                self.breaks
                    .last_mut()
                    .expect("typechecked break")
                    .push(jump);
                self.next_reg = floor;
                return (true, None);
            }
            TStmt::Local { id, value } => {
                self.compile_expr_into(value, *id as u8);
                self.record_local(*id, self.code.len());
            }
            TStmt::Assign { id, value } => {
                self.compile_expr_into(value, *id as u8);
            }
            TStmt::AssignIndex {
                array,
                index,
                value,
            } => {
                let a = self.compile_expr(array);
                let i = self.compile_expr(index);
                let v = self.compile_expr(value);
                let op = if matches!(array.ty, Type::Map(_, _)) {
                    Op::MapSetI64
                } else {
                    Op::SetIndex
                };
                self.emit(Instr::iabc(op, a, i, v));
            }
            TStmt::AssignField {
                base,
                field_index,
                value,
            } => {
                let b = self.compile_expr(base);
                let v = self.compile_expr(value);
                self.emit(Instr::iabc(Op::SetField, b, *field_index as u8, v));
            }
            TStmt::If {
                cond,
                then_block,
                else_block,
            } => {
                let c = self.compile_expr(cond);
                let jf = self.emit(Instr::iasbx(Op::JumpIfFalse, c, 0));
                self.next_reg = floor; // `c` already consumed
                let then_terminated = self.compile_block(then_block, floor, None);
                if else_block.is_empty() {
                    self.patch_jump(jf, self.code.len());
                } else {
                    let jend = if then_terminated {
                        None
                    } else {
                        Some(self.emit(Instr::iasbx(Op::Jump, 0, 0)))
                    };
                    self.patch_jump(jf, self.code.len());
                    self.next_reg = floor;
                    self.compile_block(else_block, floor, None);
                    if let Some(jend) = jend {
                        self.patch_jump(jend, self.code.len());
                    }
                }
            }
            TStmt::While { cond, body } => {
                let header = self.code.len();
                loop_header = Some(header);
                self.next_reg = floor;
                let c = self.compile_expr(cond);
                let jexit = self.emit(Instr::iasbx(Op::JumpIfFalse, c, 0));
                self.next_reg = floor;
                self.breaks.push(vec![]);
                self.compile_block(body, floor, None);
                self.emit_jump_to(header);
                self.patch_jump(jexit, self.code.len());
                for jump in self.breaks.pop().unwrap() {
                    self.patch_jump(jump, self.code.len());
                }
            }
            TStmt::NumericFor {
                id,
                stop_id,
                step_id,
                start,
                stop,
                step,
                body,
            } => {
                self.next_reg = floor;
                let start_r = self.compile_expr(start);
                self.emit(Instr::iabc(Op::Move, *id as u8, start_r, 0));
                self.next_reg = floor;
                let stop_r = *stop_id as u8;
                let step_r = *step_id as u8;
                self.compile_expr_into(stop, stop_r);
                self.compile_expr_into(step, step_r);
                self.emit(Instr::iabc(Op::TrapIfZero, step_r, 0, 0));
                let zero_r = self.alloc();
                self.emit_load_const_i64(zero_r, 0);
                let step_nonneg = self.alloc();
                self.emit(Instr::iabc(Op::GeI, step_nonneg, step_r, zero_r));
                // Raise the floor so the body's per-statement resets can't
                // clobber stop_r/step_r/step_nonneg, which must survive every iteration.
                let loop_floor = self.next_reg;

                let header = self.code.len();
                loop_header = Some(header);
                let loop_local = self.record_local(*id, header);
                self.next_reg = loop_floor;
                let cond_pos = self.alloc();
                self.emit(Instr::iabc(Op::LeI, cond_pos, *id as u8, stop_r));
                let cond_neg = self.alloc();
                self.emit(Instr::iabc(Op::GeI, cond_neg, *id as u8, stop_r));
                let t1 = self.alloc();
                self.emit(Instr::iabc(Op::And, t1, step_nonneg, cond_pos));
                let not_nonneg = self.alloc();
                self.emit(Instr::iabc(Op::Not, not_nonneg, step_nonneg, 0));
                let t2 = self.alloc();
                self.emit(Instr::iabc(Op::And, t2, not_nonneg, cond_neg));
                let cond = self.alloc();
                self.emit(Instr::iabc(Op::Or, cond, t1, t2));
                let jexit = self.emit(Instr::iasbx(Op::JumpIfFalse, cond, 0));

                self.next_reg = loop_floor;
                self.breaks.push(vec![]);
                self.compile_block(body, loop_floor, None);
                self.next_reg = loop_floor;
                let next = self.alloc();
                let safe = self.alloc();
                self.emit(Instr::iabc(Op::AddNoOverflow, next, *id as u8, step_r));
                // AddNoOverflow places the overflow flag in the next register.
                debug_assert_eq!(safe, next + 1);
                let overflow_exit = self.emit(Instr::iasbx(Op::JumpIfFalse, safe, 0));
                self.emit(Instr::iabc(Op::Move, *id as u8, next, 0));
                self.emit_jump_to(header);
                self.patch_jump(overflow_exit, self.code.len());
                self.patch_jump(jexit, self.code.len());
                for jump in self.breaks.pop().unwrap() {
                    self.patch_jump(jump, self.code.len());
                }
                if let Some(index) = loop_local {
                    self.debug_locals[index].end_pc = self.code.len() as u32;
                }
            }
            TStmt::Return { value } => {
                self.next_reg = floor;
                if let Some(value) = value {
                    if self.compile_tail_call(value) {
                        self.next_reg = floor;
                        return (true, None);
                    }
                }
                let r = value.as_ref().map(|e| self.compile_expr(e)).unwrap_or(0);
                self.emit(Instr::iabc(Op::Return, r, 0, 0));
                self.next_reg = floor;
                return (true, None);
            }
        }
        self.next_reg = floor;
        (false, loop_header)
    }

    fn compile_tail_call(&mut self, expression: &TExpr) -> bool {
        if !matches!(
            expression.kind,
            TExprKind::Call(..) | TExprKind::CallIndirect { .. }
        ) {
            return false;
        }
        let result = self.compile_expr(expression);
        let instruction = self
            .code
            .last_mut()
            .expect("compiling a call emits its call instruction");
        let replacement = match instruction.op() {
            Op::Call => Instr::iabc(
                Op::TailCall,
                instruction.a(),
                instruction.b(),
                instruction.c(),
            ),
            Op::CallIndirect => Instr::iabc(
                Op::TailCallIndirect,
                instruction.a(),
                instruction.b(),
                instruction.c(),
            ),
            _ => unreachable!("a typed call expression ends in a call instruction"),
        };
        debug_assert_eq!(replacement.a(), result);
        *instruction = replacement;
        true
    }

    fn emit_jump_to(&mut self, target: usize) {
        let at = self.emit(Instr::iasbx(Op::Jump, 0, 0));
        self.patch_jump(at, target);
    }

    fn patch_jump(&mut self, at: usize, target: usize) {
        let offset = target as i64 - (at as i64 + 1);
        let i = self.code[at];
        self.code[at] = Instr::iasbx(i.op(), i.a(), offset as i16);
    }

    fn emit_load_const_i64(&mut self, dst: u8, n: i64) {
        let slot = self.const_slot(n as u64);
        self.emit(Instr::iabx(Op::LoadK, dst, slot));
    }

    /// Compiles `e` into exactly `target` (emits a `Move` if needed).
    fn compile_expr_into(&mut self, e: &TExpr, target: u8) {
        let r = self.compile_expr(e);
        if r != target {
            self.emit(Instr::iabc(Op::Move, target, r, 0));
        }
    }

    fn truth_reg(&mut self, v: u8, ty: &Type) -> u8 {
        if *ty == Type::Bool {
            return v;
        }
        let r = self.alloc();
        if *ty == Type::Any {
            self.emit(Instr::iabc(Op::DynamicTruth, r, v, 0));
        } else {
            self.emit(Instr::iabc(Op::LoadBool, r, (*ty != Type::Nil) as u8, 0));
        }
        r
    }

    fn compile_expr(&mut self, e: &TExpr) -> u8 {
        match &e.kind {
            TExprKind::StringLit(bytes) => {
                let literal = crate::strings::literal(bytes);
                let ptr = literal.as_ptr() as u64;
                self.literals.push(literal);
                let slot = self.const_slot(ptr);
                let r = self.alloc();
                self.emit(Instr::iabx(Op::LoadK, r, slot));
                r
            }
            TExprKind::NilLit => {
                let r = self.alloc();
                self.emit_load_const_i64(r, 0);
                r
            }
            TExprKind::Truth(e) => {
                let v = self.compile_expr(e);
                self.truth_reg(v, &e.ty)
            }
            TExprKind::Local(id) => *id as u8,
            TExprKind::FunctionRef(name) => {
                let function = *self
                    .func_ids
                    .get(name)
                    .expect("typeck already verified this function exists");
                let r = self.alloc();
                self.emit(Instr::iabc(Op::LoadFunc, r, function, 0));
                r
            }
            TExprKind::IntLit(n) => {
                let r = self.alloc();
                self.emit_load_const_i64(r, *n);
                r
            }
            TExprKind::FloatLit(n) => {
                let r = self.alloc();
                let slot = self.const_slot(n.to_bits());
                self.emit(Instr::iabx(Op::LoadK, r, slot));
                r
            }
            TExprKind::BoolLit(b) => {
                let r = self.alloc();
                self.emit(Instr::iabc(Op::LoadBool, r, if *b { 1 } else { 0 }, 0));
                r
            }
            TExprKind::Neg(inner) => {
                let v = self.compile_expr(inner);
                let r = self.alloc();
                let op = match inner.ty {
                    Type::F64 => Op::NegF,
                    Type::Any => Op::DynamicNeg,
                    _ => Op::NegI,
                };
                self.emit(Instr::iabc(op, r, v, 0));
                r
            }
            TExprKind::Not(inner) => {
                let v = self.compile_expr(inner);
                let r = self.alloc();
                self.emit(Instr::iabc(Op::Not, r, v, 0));
                r
            }
            TExprKind::IntToFloat(inner) => {
                let v = self.compile_expr(inner);
                let r = self.alloc();
                self.emit(Instr::iabc(Op::IntToFloat, r, v, 0));
                r
            }
            TExprKind::Arith(op, l, rhs) => {
                let lv = self.compile_expr(l);
                let rv = self.compile_expr(rhs);
                let r = self.alloc();
                if l.ty == Type::Any {
                    self.emit(Instr::iabc(Op::DynamicBinary, r, lv, rv));
                    self.emit(Instr(*op as u32));
                    return r;
                }
                let is_float = l.ty == Type::F64;
                let opcode = match (op, is_float) {
                    (BinaryOp::Add, false) => Op::AddI,
                    (BinaryOp::Add, true) => Op::AddF,
                    (BinaryOp::Sub, false) => Op::SubI,
                    (BinaryOp::Sub, true) => Op::SubF,
                    (BinaryOp::Mul, false) => Op::MulI,
                    (BinaryOp::Mul, true) => Op::MulF,
                    (BinaryOp::FloorDiv, false) => Op::DivI,
                    (BinaryOp::FloorDiv, true) => Op::FloorDivF,
                    (BinaryOp::Mod, true) => Op::ModF,
                    (BinaryOp::Pow, true) => Op::PowF,
                    (BinaryOp::BitAnd, false) => Op::BandI,
                    (BinaryOp::BitOr, false) => Op::BorI,
                    (BinaryOp::BitXor, false) => Op::BxorI,
                    (BinaryOp::Shl, false) => Op::ShlI,
                    (BinaryOp::Shr, false) => Op::ShrI,
                    (BinaryOp::Div, true) => Op::DivF,
                    (BinaryOp::Mod, false) => Op::ModI,
                    _ => unreachable!("typeck only allows Mod for i64"),
                };
                self.emit(Instr::iabc(opcode, r, lv, rv));
                r
            }
            TExprKind::Compare(op, l, rhs) => {
                let mut lv = self.compile_expr(l);
                let mut rv = self.compile_expr(rhs);
                if l.ty == Type::String {
                    let order = self.alloc();
                    self.emit(Instr::iabc(Op::StringOrder, order, lv, rv));
                    lv = order;
                    rv = self.alloc();
                    self.emit_load_const_i64(rv, 0);
                }
                let r = self.alloc();
                if l.ty == Type::Any {
                    self.emit(Instr::iabc(Op::DynamicCompare, r, lv, rv));
                    self.emit(Instr(*op as u32));
                    return r;
                }
                let opcode = compare_op(op, &l.ty);
                self.emit(Instr::iabc(opcode, r, lv, rv));
                r
            }
            TExprKind::Logical(op, l, rhs) => {
                let r = self.alloc();
                self.compile_expr_into(l, r);
                let truth = self.truth_reg(r, &l.ty);
                let test = if *op == BinaryOp::Or {
                    let inverse = self.alloc();
                    self.emit(Instr::iabc(Op::Not, inverse, truth, 0));
                    inverse
                } else {
                    truth
                };
                let skip = self.emit(Instr::iasbx(Op::JumpIfFalse, test, 0));
                self.compile_expr_into(rhs, r);
                self.patch_jump(skip, self.code.len());
                r
            }
            TExprKind::Len(inner) => {
                let v = self.compile_expr(inner);
                let r = self.alloc();
                self.emit(Instr::iabc(Op::Len, r, v, 0));
                r
            }
            TExprKind::Index(array, index) => {
                let a = self.compile_expr(array);
                let i = self.compile_expr(index);
                let r = self.alloc();
                let op = if matches!(array.ty, Type::Map(_, _)) {
                    Op::MapGetI64
                } else {
                    Op::Index
                };
                self.emit(Instr::iabc(op, r, a, i));
                r
            }
            TExprKind::NewArray { elem, len } => {
                let l = self.compile_expr(len);
                let r = self.alloc();
                let op = if *elem == Type::I64 {
                    Op::NewArrayI64
                } else {
                    Op::NewArrayF64
                };
                self.emit(Instr::iabc(op, r, l, 0));
                r
            }
            TExprKind::ArrayLiteral { elem, values } => {
                let values: Vec<u8> = values
                    .iter()
                    .map(|value| self.compile_expr(value))
                    .collect();
                let len = self.alloc();
                self.emit_load_const_i64(len, values.len() as i64);
                let array = self.alloc();
                let op = if elem.is_gc_pointer() {
                    Op::NewArrayPtr
                } else if *elem == Type::I64 {
                    Op::NewArrayI64
                } else {
                    Op::NewArrayF64
                };
                self.emit(Instr::iabc(op, array, len, 0));
                for (index, value) in values.into_iter().enumerate() {
                    let index_reg = self.alloc();
                    self.emit_load_const_i64(index_reg, index as i64);
                    self.emit(Instr::iabc(Op::SetIndex, array, index_reg, value));
                }
                array
            }
            TExprKind::ArrayMap {
                array, callback, ..
            } => {
                // One opcode covers both I64 and F64 element types: tier-0's
                // register file is untagged u64 bit patterns, and the
                // callback call goes through the uniform-ABI native wrapper
                // (see interp.rs's `call_native`), so no element-type
                // dispatch is needed at this tier - only the native-codegen
                // tier (codegen.rs) calls a typed C-ABI function pointer
                // directly and needs a per-type runtime symbol.
                let array = self.compile_expr(array);
                let callback = self.compile_expr(callback);
                let result = self.alloc();
                self.emit(Instr::iabc(Op::ArrayMapI64, result, array, callback));
                result
            }
            TExprKind::NewMap { .. } => {
                let r = self.alloc();
                self.emit(Instr::iabc(Op::NewMapI64, r, 0, 0));
                r
            }
            TExprKind::MapLiteral { entries, .. } => {
                let entries: Vec<(u8, u8)> = entries
                    .iter()
                    .map(|(key, value)| (self.compile_expr(key), self.compile_expr(value)))
                    .collect();
                let map = self.alloc();
                self.emit(Instr::iabc(Op::NewMapI64, map, 0, 0));
                for (key, value) in entries {
                    self.emit(Instr::iabc(Op::MapSetI64, map, key, value));
                }
                map
            }
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                let map_reg = self.compile_expr(map);
                let cursor_reg = self.compile_expr(cursor);
                let result = self.alloc();
                let op = match &e.kind {
                    TExprKind::MapNext { .. } => Op::MapNextI64,
                    TExprKind::MapKey { .. } => Op::MapKeyI64,
                    TExprKind::MapValue { .. } => Op::MapValueI64,
                    _ => unreachable!(),
                };
                self.emit(Instr::iabc(op, result, map_reg, cursor_reg));
                result
            }
            TExprKind::StructLiteral { fields, .. } => {
                let field_regs: Vec<u8> = fields.iter().map(|f| self.compile_expr(f)).collect();
                let r = self.alloc();
                self.emit(Instr::iabx(Op::StructAlloc, r, fields.len() as u16 * 8));
                let pointer_mask = pointer_layout_mask(fields.iter().map(|field| &field.ty));
                self.emit(Instr(pointer_mask as u32));
                self.emit(Instr((pointer_mask >> 32) as u32));
                for (i, fr) in field_regs.into_iter().enumerate() {
                    self.emit(Instr::iabc(Op::SetField, r, i as u8, fr));
                }
                r
            }
            TExprKind::Field { base, field_index } => {
                let b = self.compile_expr(base);
                let r = self.alloc();
                self.emit(Instr::iabc(Op::GetField, r, b, *field_index as u8));
                r
            }
            TExprKind::Box(inner) => {
                let v = self.compile_expr(inner);
                let tag = value::tag_for(&inner.ty).expect("typeck never boxes an any value");
                let r = self.alloc();
                self.emit(Instr::iabc(Op::Box, r, v, 0));
                self.emit(Instr(tag as u32));
                self.emit(Instr(if inner.ty.is_gc_pointer() { 0b10 } else { 0 }));
                r
            }
            TExprKind::Unbox(inner, target) => {
                let v = self.compile_expr(inner);
                let tag = value::tag_for(target).expect("typeck never unboxes to any");
                let r = self.alloc();
                self.emit(Instr::iabc(Op::Unbox, r, v, 0));
                self.emit(Instr(tag as u32));
                r
            }
            TExprKind::Call(name, args) => {
                let base = self.alloc();
                let arg_regs: Vec<u8> = (0..args.len()).map(|_| self.alloc()).collect();
                for (r, a) in arg_regs.iter().zip(args) {
                    self.compile_expr_into(a, *r);
                }
                let func_idx = *self
                    .func_ids
                    .get(name)
                    .expect("typeck already verified this function exists");
                self.emit(Instr::iabc(Op::Call, base, func_idx, args.len() as u8));
                base
            }
            TExprKind::CallIndirect { callee, args } => {
                let callee_reg = self.compile_expr(callee);
                let base = self.alloc();
                let arg_regs: Vec<u8> = (0..args.len()).map(|_| self.alloc()).collect();
                for (r, arg) in arg_regs.iter().zip(args) {
                    self.compile_expr_into(arg, *r);
                }
                self.emit(Instr::iabc(
                    Op::CallIndirect,
                    base,
                    callee_reg,
                    args.len() as u8,
                ));
                base
            }
        }
    }
}

fn compare_op(op: &BinaryOp, operand_ty: &Type) -> Op {
    use BinaryOp::*;
    match operand_ty {
        Type::F64 => match op {
            Eq => Op::EqF,
            NotEq => Op::NeF,
            Lt => Op::LtF,
            Le => Op::LeF,
            Gt => Op::GtF,
            Ge => Op::GeF,
            _ => unreachable!("not a comparison op"),
        },
        Type::Bool => match op {
            Eq => Op::EqB,
            NotEq => Op::NeB,
            _ => unreachable!("typeck only allows Eq/NotEq on bool"),
        },
        _ => match op {
            Eq => Op::EqI,
            NotEq => Op::NeI,
            Lt => Op::LtI,
            Le => Op::LeI,
            Gt => Op::GtI,
            Ge => Op::GeI,
            _ => unreachable!("not a comparison op"),
        },
    }
}
