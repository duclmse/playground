//! Statement/control-flow compilation: blocks, `if`/`while`/`repeat`/
//! numeric- and generic-`for`, `local`/`global` declarations, assignment,
//! `return`, and goto/label statements.

use sol_core::ValueCount;

use crate::ast::{self, AssignTarget, Stmt};

use super::compile_calls::is_multi_expr;
use super::func_state::{GlobalScanState, LoopCtx};
use super::{value_count, Compiler, Const, Instr, Reg, Resolved};

/// A `Stmt::MultiAssign` target after `prepare_assign_target` has resolved
/// its addressing but before `commit_prepared_target` has stored anything.
/// `Index`/`Field` carry frozen registers (see `prepare_assign_target`)
/// rather than the original base/key expressions, so re-resolving them at
/// commit time can't observe an earlier target's store in the same
/// statement.
enum PreparedTarget {
    Local(Reg, bool, String),
    Upval(u16, bool, String),
    Global(String),
    Index(Reg, Reg),
    Field(Reg, u32),
}

impl Compiler {
    pub(super) fn compile_block(&mut self, block: &ast::Block) -> Result<(), String> {
        let level = self.level();
        self.stack[level].push_scope();
        let result = self.compile_statements(block, true);
        let level = self.level();
        let line = block.last().map(stmt_line).unwrap_or(0);
        self.stack[level].pop_scope(line)?;
        result
    }

    /// `allow_last_exception` gates the "label is the last statement of its
    /// immediate block" exception (see `BlockScope::nactive_at_start`) for
    /// this block. It's true for every ordinary block (terminated by `end`/
    /// `else`/`elseif`/end-of-chunk), matching real Lua's `block_follow`.
    /// It must be false for a `repeat`'s body: that body's own locals stay
    /// visible to the shared `until` condition (they're compiled in the
    /// same scope), so a label there can never take the exception even if
    /// it is textually last - real Lua excludes `until` from
    /// `block_follow` for exactly this reason (see the "cannot continue a
    /// repeat-until with variables" case in `goto.lua`).
    fn compile_statements(
        &mut self,
        block: &ast::Block,
        allow_last_exception: bool,
    ) -> Result<(), String> {
        let last_real = block.iter().rposition(|stmt| !is_trivial_tail_stmt(stmt));
        for (index, statement) in block.iter().enumerate() {
            let is_last = allow_last_exception
                && match last_real {
                    Some(last_real) => index > last_real,
                    None => true,
                };
            self.compile_stmt(statement, is_last)?;
            self.end_statement(stmt_line(statement));
        }
        Ok(())
    }

    /// Resets the temp-register cursor back down to "just past the
    /// currently declared locals", so later statements can reuse the
    /// register numbers this statement's temporaries used. Registers this
    /// statement retires that way (a just-closed loop's own body registers,
    /// e.g.) are never guaranteed to get overwritten by anything the *next*
    /// statement compiles - if it needs fewer registers than this one did,
    /// the gap is left holding whatever object reference was last stored
    /// there. Real Lua never has this problem: its GC only scans up to the
    /// dynamically tracked `L->top`, which drops right back down the moment
    /// a block/loop's locals go out of scope, so a retired register is
    /// simply invisible to the collector from then on, physical stale
    /// content notwithstanding. This VM instead always scans a frame's
    /// entire fixed `regs` array (see `push_lua_frame_roots`), so an
    /// equivalent register has to be made *actually* nil, not just
    /// bookkept as free - otherwise a table used only as a weak-table
    /// key/value inside a loop body (`gc.lua`'s `for i=1,lim do local
    /// t={}; a[t]=t end`) is kept spuriously reachable by this leftover
    /// register until something else happens to reuse that exact slot.
    ///
    /// These clears are compiler-synthesized with no source token of their
    /// own, so - like the `else`-skipping `Instr::Jump` `last_line` already
    /// documents - they're tagged with the line of the last real instruction
    /// emitted so far rather than `line` (the statement that just *finished*
    /// compiling), so `debug.sethook`'s `"line"` hook never sees a spurious
    /// extra line transition for them; `line` is only a fallback for the
    /// (unreachable in practice) case of clearing before anything at all has
    /// been emitted yet.
    fn end_statement(&mut self, line: u32) {
        let level = self.level();
        let state = &mut self.stack[level];
        let floor = state
            .scopes
            .last()
            .map(|scope| {
                scope
                    .locals
                    .last()
                    .map(|(_, reg, _)| reg + 1)
                    .unwrap_or(scope.saved_next_reg)
            })
            .unwrap_or(0);
        let new_next_reg = floor.max(state.retired_floor).max(state.loop_reg_floor());
        let old_next_reg = state.next_reg;
        if old_next_reg > new_next_reg {
            let clear_line = state.last_line().unwrap_or(line);
            state.clear_retired_registers(new_next_reg, old_next_reg, clear_line);
        }
        state.next_reg = new_next_reg;
    }

    fn compile_stmt(&mut self, statement: &Stmt, is_last: bool) -> Result<(), String> {
        let level = self.level();
        match statement {
            Stmt::Global { .. }
            | Stmt::GlobalFunction(_)
            | Stmt::LocalFunction(_)
            | Stmt::Label { .. }
            | Stmt::Goto { .. }
            | Stmt::MultiReturn { .. } => self.compile_stmt_ext(statement, is_last),
            Stmt::Block(body) => self.compile_block(body),
            Stmt::Break { line } => {
                let scope_depth = match self.stack[level].loops.last() {
                    Some(ctx) => ctx.scope_depth,
                    None => return Err(format!("line {line}: break outside a loop")),
                };
                let to_close: u16 = self.stack[level].scopes[scope_depth..]
                    .iter()
                    .map(|scope| scope.close_count)
                    .sum();
                if to_close > 0 {
                    self.stack[level].emit(Instr::CloseSlots(to_close), *line);
                }
                let target = self.stack[level].emit(Instr::Jump(0), *line);
                self.stack[level]
                    .loops
                    .last_mut()
                    .unwrap()
                    .break_patches
                    .push(target);
                Ok(())
            }
            Stmt::Expr(expr) => {
                self.compile_expr(expr)?;
                Ok(())
            }
            Stmt::Local {
                name,
                constant,
                close,
                value,
                line,
                ..
            } => {
                // Compile the initializer directly into what will become the
                // local's own register (mirroring `Stmt::MultiLocal` below,
                // which fixed the same issue: a separate `value_reg` +
                // freshly-`alloc_reg`'d `dst` used to leave `value_reg`
                // permanently allocated and holding a stale, uncleared
                // duplicate of the initializer's value for the rest of the
                // enclosing scope - harmless for a still-live local, but a
                // real leak/GC-rooting bug for one later reassigned, since
                // nothing ever clears that orphaned register again).
                // `NewLocal(dst, dst, name)` then "freshens" the slot's cell
                // identity in place - safe because `reg_set_fresh` reads a
                // register's current value before replacing its cell, so a
                // self-referential `dst == src` is an ordinary in-place
                // update.
                let dst = self.stack[level].next_reg;
                // A prior loop iteration (or an unrelated sibling scope) may
                // have left this exact register number captured by an
                // earlier closure - detach it *before* compiling the
                // initializer directly into `dst`, so that write can't
                // silently mutate the old cell out from under whatever
                // already captured it (see `Instr::DetachCell`'s doc).
                self.stack[level].emit(Instr::DetachCell(dst), *line);
                self.compile_into(value, dst)?;
                let name_const = self.stack[level].push_name_const(name);
                self.stack[level].emit(Instr::NewLocal(dst, dst, name_const), *line);
                self.stack
                    .last_mut()
                    .unwrap()
                    .scopes
                    .last_mut()
                    .unwrap()
                    .locals
                    // Lua's to-be-closed locals are immutable after their
                    // initializer, even without an explicit `<const>`.
                    .push((name.clone(), dst, *constant || *close));
                self.stack[level].check_local_variable_limit(*line)?;
                if *close {
                    let name_const = self.stack[level].push_name_const(name);
                    self.stack[level].emit(Instr::MarkClose(dst, name_const), *line);
                    self.stack[level].scopes.last_mut().unwrap().close_count += 1;
                }
                Ok(())
            }
            Stmt::MultiLocal {
                names,
                values,
                line,
            } => {
                // Real Lua's `localstat` (`lparser.c`) compiles each
                // initializer directly into the new local's own eventual
                // register via `luaK_exp2nextreg`, one register per declared
                // name. This used to instead compute every initializer (or
                // nil pad) into its own fresh register via
                // `compile_expr_list`, then allocate a *second*, separate
                // register per name for `Instr::NewLocal`'s destination -
                // permanently doubling a `local a, b, ...` statement's
                // register cost (confirmed to trip `MAX_FSTACK` on a plain
                // ~127-local declaration with no closures at all; see
                // `docs/features/unified-sol-runtime-plan.md`'s U6 section
                // and `tests/lua55/manifest.toml`'s `errors.lua` entry).
                //
                // `base..base + names.len()` are reserved up front as the
                // locals' own registers. Each initializer compiles directly
                // into its slot via `compile_into`, whose own
                // `reserve_through` keeps its internal temporaries above
                // every slot - so an initializer that aliases an outer
                // variable's own register (e.g. `local a, b = outerA,
                // outerA + 1`) still only ever *copies* that value in via a
                // `Move`; the new locals aren't pushed into `scopes.locals`
                // until every initializer above has already compiled, so
                // `resolve` can't see them early either way.
                // `NewLocal(dst, dst, name)` then "freshens" each slot's
                // cell identity in place - safe because `reg_set_fresh`
                // reads a register's current value before replacing its
                // cell, so a self-referential `dst == src` is an ordinary
                // in-place update.
                let base = self.stack[level].next_reg;
                let count = names.len();
                // See `Stmt::Local`'s matching comment: detach every
                // destination register up front, before any initializer
                // (including the nil-padding below) writes into it.
                for offset in 0..count as u16 {
                    self.stack[level].emit(Instr::DetachCell(base + offset), *line);
                }
                let mut filled: usize = 0;
                for (index, value) in values.iter().enumerate() {
                    let is_last = index + 1 == values.len();
                    if is_last && is_multi_expr(value) {
                        let want = (count as i32 - filled as i32).max(0);
                        let dst = base + filled as u16;
                        self.stack[level].reset_to(dst);
                        let result_base = self.compile_expr_multi_n(value, want)?;
                        debug_assert_eq!(result_base, dst);
                        filled = count;
                        break;
                    }
                    if filled < count {
                        let dst = base + filled as u16;
                        self.compile_into(value, dst)?;
                        filled += 1;
                    } else {
                        // More initializers than names: still evaluate for
                        // side effects (`local a = f(), g()`), but the
                        // result doesn't need a home.
                        self.compile_expr(value)?;
                    }
                }
                while filled < count {
                    let dst = base + filled as u16;
                    self.stack[level].reserve_through(dst);
                    self.stack[level].emit(Instr::LoadNil(dst), *line);
                    filled += 1;
                }
                for (index, (name, _, constant, close)) in names.iter().enumerate() {
                    let dst = base + index as u16;
                    let name_const = self.stack[level].push_name_const(name);
                    self.stack[level].emit(Instr::NewLocal(dst, dst, name_const), *line);
                    self.stack
                        .last_mut()
                        .unwrap()
                        .scopes
                        .last_mut()
                        .unwrap()
                        .locals
                        // Lua's to-be-closed locals are immutable after their
                        // initializer, even without an explicit `<const>`.
                        .push((name.clone(), dst, *constant || *close));
                    self.stack[level].check_local_variable_limit(*line)?;
                    if *close {
                        let name_const = self.stack[level].push_name_const(name);
                        self.stack[level].emit(Instr::MarkClose(dst, name_const), *line);
                        self.stack[level].scopes.last_mut().unwrap().close_count += 1;
                    }
                }
                Ok(())
            }
            Stmt::Assign {
                target,
                value,
                line,
            } => {
                // Capturing a name on the left side of a simple assignment
                // establishes its Lua upvalue slot before captures in the
                // right-side expression. This matters after string.dump:
                // `a = b + 1` must retain `a` as upvalue #1 and `b` as #2,
                // even though the RHS executes first at runtime.
                if matches!(target, AssignTarget::Name(_)) {
                    let prepared = self.prepare_assign_target(target, *line)?;
                    let value_reg = self.compile_expr(value)?;
                    return self.commit_prepared_target(&prepared, value_reg, *line);
                }
                let value_reg = self.compile_expr(value)?;
                self.compile_assign(target, value_reg, *line)
            }
            Stmt::MultiAssign {
                targets,
                values,
                line,
            } => {
                // Lua evaluates all RHS values before performing any
                // assignment (`a, b = b, a` swaps). `compile_expr_list`
                // may return a simple name's own register unchanged, which
                // can alias a later assignment target, so snapshot every
                // value into a fresh register before assigning any of them.
                let raw = self.compile_expr_list(values, targets.len())?;
                let regs: Vec<Reg> = raw
                    .iter()
                    .map(|&r| {
                        let dst = self.stack[level].alloc_reg();
                        self.stack[level].emit(Instr::Move(dst, r), *line);
                        dst
                    })
                    .collect();
                // Lua also resolves every target's own addressing (which
                // table/upvalue, which key) before any assignment in this
                // statement takes effect - an earlier target's store must
                // never change what a later target's table/key expression
                // reads. `lua-5.5.1-tests/attrib.lua`'s "test conflicts in
                // multiple assignment" exercises exactly this:
                // `i, a[i], a, j, a[j], a[i+j] = j, i, i, b, j, i` reassigns
                // `a` itself as the third target, but the fifth and sixth
                // targets (`a[j]`, `a[i+j]`) must still index the ORIGINAL
                // table `a` referred to when the statement began, not
                // whatever `a` becomes after the third target's store runs.
                // `prepare_assign_target` resolves and freezes each
                // `Index`/`Field` target's base/key into fresh registers
                // up front, before `commit_prepared_target` performs any
                // of the actual stores.
                let prepared: Vec<PreparedTarget> = targets
                    .iter()
                    .map(|target| self.prepare_assign_target(target, *line))
                    .collect::<Result<_, _>>()?;
                for (index, target) in prepared.iter().enumerate() {
                    self.commit_prepared_target(target, regs[index], *line)?;
                }
                Ok(())
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
            } => {
                let cond_before = self.stack[level].next_reg;
                let cond_reg = self.compile_expr(cond)?;
                let test_line = self.stack.last().unwrap().last_line().unwrap_or(*line);
                let jump_to_else = self
                    .stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::JumpIfFalse(cond_reg, 0), test_line);
                // Whatever the condition left allocated above `cond_before`
                // (`cond_reg` itself, plus e.g. a call's argument-marshaling
                // scratch that a `Call` never reclaims down to just its own
                // result register) must be dropped right here, on *both*
                // the branch this `JumpIfFalse` falls through to and the one
                // it jumps to - not left for the shared, once-only
                // `end_statement` call after the whole `if`/`else`
                // compiles. That call fires at the control-flow *join
                // point* both branches share, where the only available
                // compile-time line (`last_line()`) always reflects
                // whichever branch was textually compiled *last* (the
                // `else` block) - producing a spurious `debug.sethook`
                // `"line"` event on any run that actually took the *other*
                // branch. Tagging both copies with `test_line` (this very
                // `JumpIfFalse`'s own line) is safe on both paths: it's
                // already the current hook line the instant either branch
                // is entered, so neither copy causes a line transition.
                // Dropping `next_reg` back to `cond_before` immediately
                // (instead of `free_reg`, which only reclaims a single,
                // strictly-topmost register) means the outer
                // `end_statement` sees no leftover delta to (mis-)clear on
                // its own once this statement finishes.
                let leaked_top = self.stack[level].next_reg;
                let needs_clear = leaked_top > cond_before;
                if needs_clear {
                    self.stack
                        .last_mut()
                        .unwrap()
                        .retire_registers_to(cond_before, test_line);
                }
                self.compile_block(then_block)?;
                if else_block.is_some() || needs_clear {
                    let skip_line = self.stack.last().unwrap().last_line().unwrap_or(*line);
                    let jump_to_end = self
                        .stack
                        .last_mut()
                        .unwrap()
                        .emit(Instr::Jump(0), skip_line);
                    let else_start = self.stack.last_mut().unwrap().here();
                    self.stack
                        .last_mut()
                        .unwrap()
                        .patch_jump(jump_to_else, else_start as i32);
                    if needs_clear {
                        self.stack
                            .last_mut()
                            .unwrap()
                            .clear_retired_registers(cond_before, leaked_top, test_line);
                    }
                    if let Some(else_block) = else_block {
                        self.compile_block(else_block)?;
                    }
                    let end = self.stack.last_mut().unwrap().here();
                    self.stack
                        .last_mut()
                        .unwrap()
                        .patch_jump(jump_to_end, end as i32);
                } else {
                    let end = self.stack.last_mut().unwrap().here();
                    self.stack
                        .last_mut()
                        .unwrap()
                        .patch_jump(jump_to_else, end as i32);
                }
                Ok(())
            }
            Stmt::While { cond, body, line } => {
                let test_start = self.stack[level].here();
                let cond_before = self.stack[level].next_reg;
                let cond_reg = self.compile_expr(cond)?;
                let cond_line = self.stack.last().unwrap().last_line().unwrap_or(*line);
                let exit_jump = self
                    .stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::JumpIfFalse(cond_reg, 0), cond_line);
                // Whatever the condition left allocated above `cond_before`
                // (register allocation is stack-disciplined, so a bare
                // local/upvalue-as-local condition like `while flag do`
                // returns `flag`'s own register, always below this
                // watermark, and is correctly left untouched) must be
                // dropped immediately, on *both* paths this `JumpIfFalse`
                // can take - not left for the shared, once-only
                // `end_statement` call after the whole `while` statement
                // compiles. Real Lua's tracing collector never even sees
                // this stale reference: a temporary test value like `x[1]`
                // in `while x[1] do ... end` lives only above the stack's
                // `top`, which recedes right after the test. Without this,
                // this runtime's Rc-refcounting model would keep leaving the
                // last iteration's test value sitting in this register
                // uncleared for the rest of the loop body's execution,
                // holding a phantom strong reference that defeats
                // weak-table pruning/cycle collection for a reference-typed
                // condition - exactly the idiom
                // `lua-5.5.1-tests/closure.lua` uses (`while x[1] do ... end`
                // waiting for automatic GC to clear a weak-valued entry)
                // to force a collection.
                //
                // Both copies are tagged `cond_line` (this very
                // `JumpIfFalse`'s own line): on the "enter/re-enter the
                // body" path it's already current (the test just ran); on
                // the "loop exits" path it's *still* current even after
                // many iterations, because every back-edge re-executes the
                // test and re-fires the hook for `cond_line` right before
                // falling out - unlike `last_line()` (what the outer,
                // shared `end_statement` would otherwise use), which is the
                // *body*'s last line, a line that's no longer "current" by
                // the time the loop actually exits. Reducing `next_reg`
                // back to `cond_before` immediately means that outer call
                // sees no leftover delta to (mis-)clear on its own either
                // way.
                let leaked_top = self.stack[level].next_reg;
                let needs_clear = leaked_top > cond_before;
                if needs_clear {
                    // Clears the condition's own leaked temporaries' *value*
                    // every iteration (the GC-hygiene concern this comment
                    // block explains above) without giving the register
                    // *number* back to the pool: the `LoopCtx` pushed right
                    // below seeds `reg_floor` at `leaked_top`, so nothing the
                    // loop body declares can land on top of a slot this same
                    // re-executing condition test still writes to on every
                    // later iteration - the same class of stale-cell-aliasing
                    // hazard `Instr::DetachCell`'s doc describes, just for a
                    // register this condition test itself (rather than a
                    // sibling scope) keeps reusing.
                    self.stack
                        .last_mut()
                        .unwrap()
                        .clear_retired_registers(cond_before, leaked_top, cond_line);
                }
                let scope_depth = self.stack[level].scopes.len();
                self.stack.last_mut().unwrap().loops.push(LoopCtx {
                    break_patches: Vec::new(),
                    scope_depth,
                    reg_floor: leaked_top,
                });
                self.compile_block(body)?;
                let back_line = self.stack.last().unwrap().last_line().unwrap_or(*line);
                let back = self
                    .stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Jump(0), back_line);
                self.stack
                    .last_mut()
                    .unwrap()
                    .patch_jump(back, test_start as i32);
                let end = self.stack.last_mut().unwrap().here();
                self.stack
                    .last_mut()
                    .unwrap()
                    .patch_jump(exit_jump, end as i32);
                if needs_clear {
                    self.stack
                        .last_mut()
                        .unwrap()
                        .clear_retired_registers(cond_before, leaked_top, cond_line);
                }
                let ctx = self.stack.last_mut().unwrap().loops.pop().unwrap();
                if let Some(parent) = self.stack.last_mut().unwrap().loops.last_mut() {
                    parent.reg_floor = parent.reg_floor.max(ctx.reg_floor);
                }
                for patch in ctx.break_patches {
                    self.stack.last_mut().unwrap().patch_jump(patch, end as i32);
                }
                Ok(())
            }
            Stmt::Repeat { body, cond, line } => {
                let start = self.stack[level].here();
                let scope_depth = self.stack[level].scopes.len();
                self.stack[level].loops.push(LoopCtx {
                    break_patches: Vec::new(),
                    scope_depth,
                    reg_floor: 0,
                });
                // `until` can see locals declared in the body, so both
                // share one scope (matching the tree-walker's
                // `exec_statements` call over body+cond together).
                self.stack[level].push_scope();
                let result = self.compile_statements(body, false).and_then(|_| {
                    let cond_reg = self.compile_expr(cond)?;
                    let cl = self.level();
                    let cond_line = self.stack[cl].last_line().unwrap_or(*line);
                    let here = self.stack[cl].here();
                    self.stack[cl].emit(
                        Instr::JumpIfFalse(cond_reg, start as i32 - here as i32),
                        cond_line,
                    );
                    Ok(())
                });
                let cl = self.level();
                self.stack[cl].pop_scope(*line)?;
                result?;
                let end = self.stack[cl].here();
                let ctx = self.stack[cl].loops.pop().unwrap();
                if let Some(parent) = self.stack[cl].loops.last_mut() {
                    parent.reg_floor = parent.reg_floor.max(ctx.reg_floor);
                }
                for patch in ctx.break_patches {
                    self.stack[cl].patch_jump(patch, end as i32);
                }
                Ok(())
            }
            Stmt::NumericFor {
                var,
                start,
                stop,
                step,
                body,
                line,
            } => {
                // Compile all three control expressions into independent
                // temporaries first (each may use its own internal temps),
                // then allocate the contiguous `[start, stop, step, var]`
                // block back-to-back with only `Move`s in between — this is
                // the only way to guarantee the four registers land next to
                // each other regardless of how many temps each expression
                // needed.
                let start_reg = self.compile_expr(start)?;
                let stop_reg = self.compile_expr(stop)?;
                let step_reg = match step {
                    Some(step) => self.compile_expr(step)?,
                    None => {
                        let level = self.level();
                        let r = self.stack[level].alloc_reg();
                        let k = self.stack[level].push_const(Const::Integer(1));
                        self.stack[level].emit(Instr::LoadConst(r, k), *line);
                        r
                    }
                };
                let level = self.level();
                let ctrl_base = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(ctrl_base, start_reg), *line);
                let ctrl_stop = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(ctrl_stop, stop_reg), *line);
                let ctrl_step = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(ctrl_step, step_reg), *line);
                debug_assert_eq!(ctrl_stop, ctrl_base + 1);
                debug_assert_eq!(ctrl_step, ctrl_base + 2);
                let var_reg = self.stack[level].alloc_reg();
                debug_assert_eq!(var_reg, ctrl_base + 3);
                let prep = self.stack[level].emit(Instr::ForPrep(ctrl_base, 0), *line);
                let scope_depth = self.stack[level].scopes.len();
                self.stack[level].push_scope();
                self.stack[level].scopes.last_mut().unwrap().locals.push((
                    var.clone(),
                    var_reg,
                    true,
                ));
                self.stack[level].check_local_variable_limit(*line)?;
                self.stack[level].loops.push(LoopCtx {
                    break_patches: Vec::new(),
                    scope_depth,
                    reg_floor: 0,
                });
                let body_start = self.stack[level].here();
                let result = self.compile_statements(body, true);
                let cl = self.level();
                self.stack[cl].pop_scope(*line)?;
                result?;
                let here = self.stack[cl].here();
                self.stack[cl].emit(
                    Instr::ForLoop(ctrl_base, body_start as i32 - here as i32),
                    *line,
                );
                let end = self.stack[cl].here();
                self.stack[cl].patch_jump(prep, end as i32);
                let ctx = self.stack[cl].loops.pop().unwrap();
                if let Some(parent) = self.stack[cl].loops.last_mut() {
                    parent.reg_floor = parent.reg_floor.max(ctx.reg_floor);
                }
                for patch in ctx.break_patches {
                    self.stack[cl].patch_jump(patch, end as i32);
                }
                Ok(())
            }
            Stmt::GenericFor {
                vars,
                iterators,
                body,
                line,
            } => {
                // A 4th iterator-expression value is Lua 5.4+'s implicit
                // to-be-closed value for the whole loop (closed once when
                // the loop ends by any means, not per iteration) - fetched
                // into its own register first (contiguity only matters for
                // the `[f, s, ctrl]` block the VM's `TForCall`/`TForLoop`
                // hardcode at `base..base+2`, with results at `base+3`).
                let regs = self.compile_expr_list(iterators, 4)?;
                let closing_reg = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(closing_reg, regs[3]), *line);
                let base = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(base, regs[0]), *line);
                let s = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(s, regs[1]), *line);
                let ctrl = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(ctrl, regs[2]), *line);
                debug_assert_eq!(s, base + 1);
                debug_assert_eq!(ctrl, base + 2);
                // Outer scope, wrapping the whole loop: tracks only the
                // implicit 4th closing value, unconditionally (a `nil` in
                // that slot - the common case, when the iterator expression
                // didn't produce a 4th value - is trivially closeable and a
                // no-op to close, matching real Lua's always-track-it
                // behavior).
                let scope_depth = self.stack[level].scopes.len();
                self.stack[level].push_scope();
                let close_name = self.stack[level].push_name_const("(for closing value)");
                self.stack[level].emit(Instr::MarkClose(closing_reg, close_name), *line);
                self.stack[level].scopes.last_mut().unwrap().close_count += 1;
                self.stack[level].loops.push(LoopCtx {
                    break_patches: Vec::new(),
                    scope_depth,
                    reg_floor: 0,
                });
                let init_jump = self.stack[level].emit(Instr::Jump(0), *line);
                let body_start = self.stack[level].here();
                // Inner scope, per iteration: the loop variables and any
                // `<close>` locals the body itself declares.
                self.stack[level].push_scope();
                let mut var_regs = Vec::new();
                for _ in vars {
                    var_regs.push(self.stack[level].alloc_reg());
                }
                for (index, (name, reg)) in vars.iter().zip(var_regs.iter()).enumerate() {
                    // Only the first generic `for` variable (the one fed back
                    // into `TForCall` as the iterator's control value) is
                    // implicitly const in real Lua - the rest are ordinary
                    // reassignable locals (see `nextvar.lua`'s `load
                    // "for v, k in pairs{} do v = 10 end"` check, which only
                    // rejects reassigning the first variable).
                    self.stack[level].scopes.last_mut().unwrap().locals.push((
                        name.clone(),
                        *reg,
                        index == 0,
                    ));
                    self.stack[level].check_local_variable_limit(*line)?;
                }
                debug_assert_eq!(var_regs.first().copied(), Some(base + 3));
                let result = self.compile_statements(body, true);
                let cl = self.level();
                self.stack[cl].pop_scope(*line)?;
                result?;
                let test = self.stack[cl].here();
                self.stack[cl].patch_jump(init_jump, test as i32);
                self.stack[cl].emit(Instr::TForCall(base, vars.len() as u16), *line);
                let here = self.stack[cl].here();
                self.stack[cl].emit(
                    Instr::TForLoop(base, body_start as i32 - here as i32),
                    *line,
                );
                // `TForLoop` falls through exactly here when the iterator
                // signals completion (its first result was nil) - this is
                // the loop's one normal-completion point, so popping the
                // outer scope here closes the 4th value exactly once.
                self.stack[cl].pop_scope(*line)?;
                let end = self.stack[cl].here();
                let ctx = self.stack[cl].loops.pop().unwrap();
                if let Some(parent) = self.stack[cl].loops.last_mut() {
                    parent.reg_floor = parent.reg_floor.max(ctx.reg_floor);
                }
                for patch in ctx.break_patches {
                    self.stack[cl].patch_jump(patch, end as i32);
                }
                Ok(())
            }
            Stmt::Return { value, line } => {
                // A pending `<close>` variable suppresses the tail-call
                // optimization (real Lua's own documented behavior) since
                // a tail call replaces this frame before its scopes' normal
                // `CloseSlots` would ever run.
                let total_close = self.total_close_count(level);
                match value {
                    Some(value) => {
                        if total_close == 0 && self.compile_tail_call(value)? {
                            return Ok(());
                        }
                        let (base, count) =
                            self.compile_expr_list_multi(std::slice::from_ref(value))?;
                        if total_close > 0 {
                            self.stack[level].emit(Instr::CloseSlots(total_close), *line);
                        }
                        self.stack
                            .last_mut()
                            .unwrap()
                            .emit(Instr::Return(base, value_count(count)), *line);
                    }
                    None => {
                        if total_close > 0 {
                            self.stack[level].emit(Instr::CloseSlots(total_close), *line);
                        }
                        self.stack[level].emit(Instr::Return(0, ValueCount::ZERO), *line);
                    }
                }
                Ok(())
            }
        }
    }

    /// Sums `close_count` over every currently-open scope in the function
    /// at `level` (`return` exits all of them at once, unlike `break`,
    /// which only exits up to its own loop's `scope_depth`).
    fn total_close_count(&self, level: usize) -> u16 {
        self.stack[level]
            .scopes
            .iter()
            .map(|scope| scope.close_count)
            .sum()
    }

    /// Statements that only exist in dynamic Lua source and need no extra
    /// register bookkeeping beyond globals/closures/labels, split out to
    /// keep `compile_stmt`'s match arm list manageable.
    fn compile_stmt_ext(&mut self, statement: &Stmt, is_last: bool) -> Result<(), String> {
        let level = self.level();
        match statement {
            Stmt::Global {
                names,
                values,
                line,
            } if values.is_empty() => {
                // Bare `global name[, name...]` (no `= ...`) only declares
                // that these names are legitimately used as globals -
                // real Lua 5.5 does not touch their current value. Reread
                // each name's existing value (built-ins keep their real
                // binding; a name that isn't bound anywhere resolves to
                // nil, same as an ordinary undeclared global) before
                // re-declaring it, so `<const>` still takes effect without
                // clobbering anything already there.
                for binding in names {
                    match &binding.name {
                        ast::GlobalName::Name(name) => {
                            let reg = self.stack[level].alloc_reg();
                            self.emit_environment_get(level, name, reg, *line);
                            self.emit_environment_set(
                                level,
                                name,
                                reg,
                                binding.constant,
                                true,
                                *line,
                            );
                            if binding.constant {
                                self.stack[level].environment_constants.insert(name.clone());
                            } else {
                                self.stack[level].environment_constants.remove(name);
                            }
                            self.stack[level].declare_global_named(name);
                        }
                        ast::GlobalName::All => {
                            self.stack[level].declare_global_collective(binding.constant);
                        }
                        ast::GlobalName::None => {
                            self.stack[level].declare_global_none();
                        }
                    }
                }
                Ok(())
            }
            Stmt::Global {
                names,
                values,
                line,
            } => {
                let regs = self.compile_expr_list(values, names.len())?;
                for (index, binding) in names.iter().enumerate() {
                    if let ast::GlobalName::Name(name) = &binding.name {
                        self.emit_check_global_undefined(level, name, *line);
                        self.emit_environment_set(
                            level,
                            name,
                            regs[index],
                            binding.constant,
                            true,
                            *line,
                        );
                        if binding.constant {
                            self.stack[level].environment_constants.insert(name.clone());
                        } else {
                            self.stack[level].environment_constants.remove(name);
                        }
                        self.stack[level].declare_global_named(name);
                    }
                }
                Ok(())
            }
            Stmt::GlobalFunction(function) => {
                // Real Lua's `globalfunc` declares (and activates) the
                // function's own name as a global *before* parsing its
                // body, so the body's own recursive self-reference (and
                // any use of the name after the closure is built) resolve
                // as the declared global rather than an outer local of the
                // same name - only for the genuine `global function NAME`
                // form, never for a plain, non-`global` top-level
                // `function NAME` the parser couldn't hoist (that's
                // ordinary assignment sugar, not a declaration).
                if function.is_global_decl && !function.name.contains(['.', ':']) {
                    let level = self.level();
                    self.stack[level].declare_global_named(&function.name);
                }
                let proto = self.compile_function(function)?;
                let level = self.level();
                let idx = self.stack[level].nested.len() as u16;
                self.stack[level].nested.push(proto);
                let dst = self.stack[level].alloc_reg();
                // Real Lua's `codeclosure` emits `OP_CLOSURE` only after the
                // whole function body has been parsed, tagged with
                // `ls->lastline` at that point (the closing `end`'s line,
                // i.e. `function.end_line`) - not the `function` keyword's
                // own opening line. A line-event hook installed before this
                // statement runs must not observe anything until the
                // closure is actually created.
                self.stack[level].emit(Instr::NewClosure(dst, idx), function.end_line);
                if function.is_global_decl && !function.name.contains(['.', ':']) {
                    // `globalfunc` always supplies its closure as the
                    // declaration's initializer, so (unlike a bare `global
                    // NAME`) this form always runs the "already defined"
                    // guard, right before the closure is actually stored.
                    // This guard is a Sol-only extension with no real-Lua
                    // equivalent to match, but it conceptually belongs to
                    // declaring the name (like the store below), not to
                    // creating the closure value - so it shares the same
                    // `function.line` tag, not `end_line`, for the same
                    // reason (and so a non-table environment surfaces on the
                    // declaration's own line, like the store would).
                    self.emit_check_global_undefined(level, &function.name, function.line);
                }
                // Real Lua's `funcstat` resolves the assignment target
                // (`funcname` - a plain name, or a `.`/`:`-chained prefix for
                // `function a.b.c()`/`function a:m()`) *before* parsing the
                // closure body, then explicitly re-tags the final store
                // instruction back to that start line via
                // `luaK_fixline(ls->fs, line)` ("definition happens in the
                // first line") - so both the dotted-name prefix lookups and
                // the store into the target land on `function.line`, the
                // `function`/`global` keyword's own line, not `end_line`
                // (unlike the closure creation above, which real Lua's
                // `codeclosure` does tag with the closing `end`'s line).
                self.compile_function_name_assign(&function.name, dst, function.line)?;
                Ok(())
            }
            Stmt::LocalFunction(function) => {
                let level = self.level();
                let nil = self.stack[level].alloc_reg();
                // No real-Lua equivalent emits any code for pre-declaring
                // the recursive-reference local itself (`new_localvar`
                // alone doesn't touch `lastline`), so these two setup
                // instructions must not be independently line-observable
                // either - tag them `end_line` too, matching the closure
                // instructions below.
                self.stack[level].emit(Instr::LoadNil(nil), function.end_line);
                let slot = self.stack[level].alloc_reg();
                let name_const = self.stack[level].push_name_const(&function.name);
                self.stack[level].emit(Instr::NewLocal(slot, nil, name_const), function.end_line);
                self.stack[level].scopes.last_mut().unwrap().locals.push((
                    function.name.clone(),
                    slot,
                    false,
                ));
                self.stack[level].check_local_variable_limit(function.end_line)?;
                let proto = self.compile_function(function)?;
                let level = self.level();
                let idx = self.stack[level].nested.len() as u16;
                self.stack[level].nested.push(proto);
                let dst = self.stack[level].alloc_reg();
                // See the matching comment in `Stmt::GlobalFunction`: the
                // closure isn't observably created until its body is fully
                // parsed, so this (and the `Move` storing it into `slot`)
                // must carry `end_line`, not the declaration's own line.
                self.stack[level].emit(Instr::NewClosure(dst, idx), function.end_line);
                self.stack[level].emit(Instr::Move(slot, dst), function.end_line);
                Ok(())
            }
            Stmt::Label { name, line } => {
                let offset = self.stack[level].here();
                self.stack[level].record_label(name, offset, is_last, *line)
            }
            Stmt::Goto { name, line } => {
                // Known limitation: a `goto` that jumps out of a scope
                // holding pending `<close>` locals does not close them (real
                // Lua's compiler tracks goto-scope-depth specifically to get
                // this right; replicating that on top of this compiler's
                // bubble-up-through-`pop_scope` goto resolution is a
                // separate, more invasive effort than this feature's actual
                // corpus-driven scope - see `nextvar.lua`, which never
                // combines `goto` with `<close>`).
                let patch_site = self.stack[level].emit(Instr::Jump(0), *line);
                self.stack[level].goto_stmt(name, patch_site, *line)
            }
            Stmt::MultiReturn { values, line } => {
                // Lua's bytecode encodes a fixed return count in an 8-bit
                // field where zero means the open/multi-result form.  Its
                // largest representable explicit list is therefore 254
                // values; reject a longer source list at compile time rather
                // than silently accepting bytecode native Lua cannot load.
                if values.len() > 254 {
                    return Err(format!("line {line}: too many returns"));
                }
                let total_close = self.total_close_count(level);
                if total_close == 0 {
                    if let [value] = values.as_slice() {
                        if self.compile_tail_call(value)? {
                            return Ok(());
                        }
                    }
                }
                let (base, count) = self.compile_expr_list_multi(values)?;
                if total_close > 0 {
                    self.stack[level].emit(Instr::CloseSlots(total_close), *line);
                }
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Return(base, value_count(count)), *line);
                Ok(())
            }
            _ => unreachable!("compile_stmt_ext called with a non-extended statement"),
        }
    }

    fn compile_assign(
        &mut self,
        target: &AssignTarget,
        value_reg: Reg,
        line: u32,
    ) -> Result<(), String> {
        let level = self.level();
        match target {
            AssignTarget::Name(name) => match self.resolve(level, name) {
                Resolved::Local(reg, constant) => {
                    if constant {
                        return Err(format!(
                            "line {line}: attempt to assign to const variable '{name}'"
                        ));
                    }
                    self.stack
                        .last_mut()
                        .unwrap()
                        .emit(Instr::Move(reg, value_reg), line);
                    Ok(())
                }
                Resolved::Upval(idx, constant) => {
                    if constant {
                        return Err(format!(
                            "line {line}: attempt to assign to const variable '{name}'"
                        ));
                    }
                    self.stack
                        .last_mut()
                        .unwrap()
                        .emit(Instr::SetUpval(idx, value_reg), line);
                    Ok(())
                }
                Resolved::Global => {
                    self.check_global_access(name, line)?;
                    let collective_const =
                        matches!(self.resolve_global_decl(name), GlobalScanState::Found(true));
                    if self.stack[level].environment_constants.contains(name) || collective_const {
                        return Err(format!(
                            "line {line}: attempt to assign to const variable '{name}'"
                        ));
                    }
                    self.emit_environment_set(level, name, value_reg, false, false, line);
                    Ok(())
                }
            },
            AssignTarget::Index(base, index) => {
                let base_reg = self.compile_expr(base)?;
                let index_reg = self.compile_expr(index)?;
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::SetIndex(base_reg, index_reg, value_reg), line);
                Ok(())
            }
            AssignTarget::Field(base, field) => {
                let base_reg = self.compile_expr(base)?;
                let name = self
                    .stack
                    .last_mut()
                    .unwrap()
                    .push_name_const(field.as_str());
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::SetField(base_reg, name, value_reg), line);
                Ok(())
            }
        }
    }

    /// Resolves a `Stmt::MultiAssign` target's addressing without storing
    /// anything yet, so a whole target list can be resolved before any of
    /// their stores run (see the `Stmt::MultiAssign` arm above for why that
    /// ordering matters). For `Index`/`Field` targets this evaluates the
    /// base (and, for `Index`, the key) expression and immediately freezes
    /// each into a fresh register via `Move` - the same technique already
    /// used to snapshot RHS values - since a plain `compile_expr` result
    /// for a local variable is that variable's own register, whose content
    /// can change if an earlier target in the same statement reassigns it.
    fn prepare_assign_target(
        &mut self,
        target: &AssignTarget,
        line: u32,
    ) -> Result<PreparedTarget, String> {
        let level = self.level();
        match target {
            AssignTarget::Name(name) => match self.resolve(level, name) {
                Resolved::Local(reg, constant) => {
                    Ok(PreparedTarget::Local(reg, constant, name.clone()))
                }
                Resolved::Upval(idx, constant) => {
                    Ok(PreparedTarget::Upval(idx, constant, name.clone()))
                }
                Resolved::Global => Ok(PreparedTarget::Global(name.clone())),
            },
            AssignTarget::Index(base, index) => {
                let base_reg = self.compile_expr(base)?;
                let frozen_base = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(frozen_base, base_reg), line);
                let index_reg = self.compile_expr(index)?;
                let frozen_index = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(frozen_index, index_reg), line);
                Ok(PreparedTarget::Index(frozen_base, frozen_index))
            }
            AssignTarget::Field(base, field) => {
                let base_reg = self.compile_expr(base)?;
                let frozen_base = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::Move(frozen_base, base_reg), line);
                let name = self.stack[level].push_name_const(field.as_str());
                Ok(PreparedTarget::Field(frozen_base, name))
            }
        }
    }

    /// Performs the actual store for a target already resolved by
    /// `prepare_assign_target`.
    fn commit_prepared_target(
        &mut self,
        target: &PreparedTarget,
        value_reg: Reg,
        line: u32,
    ) -> Result<(), String> {
        let level = self.level();
        match target {
            PreparedTarget::Local(reg, constant, name) => {
                if *constant {
                    return Err(format!(
                        "line {line}: attempt to assign to const variable '{name}'"
                    ));
                }
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Move(*reg, value_reg), line);
                Ok(())
            }
            PreparedTarget::Upval(idx, constant, name) => {
                if *constant {
                    return Err(format!(
                        "line {line}: attempt to assign to const variable '{name}'"
                    ));
                }
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::SetUpval(*idx, value_reg), line);
                Ok(())
            }
            PreparedTarget::Global(name) => {
                self.check_global_access(name, line)?;
                let collective_const =
                    matches!(self.resolve_global_decl(name), GlobalScanState::Found(true));
                if self.stack[level].environment_constants.contains(name) || collective_const {
                    return Err(format!(
                        "line {line}: attempt to assign to const variable '{name}'"
                    ));
                }
                self.emit_environment_set(level, name, value_reg, false, false, line);
                Ok(())
            }
            PreparedTarget::Index(base_reg, index_reg) => {
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::SetIndex(*base_reg, *index_reg, value_reg), line);
                Ok(())
            }
            PreparedTarget::Field(base_reg, name_const) => {
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::SetField(*base_reg, *name_const, value_reg), line);
                Ok(())
            }
        }
    }

    /// Assigns `value_reg` to the target named by a `function` statement's
    /// declaration name, which the parser leaves as a single string with
    /// embedded `.`/`:` separators for dotted (`function t.f()`) and method
    /// (`function t:f()`) declarations (`:` only ever appears as the final
    /// separator; both are plain field assignments by the time codegen sees
    /// them - the parser already inserted the implicit `self` parameter for
    /// `:`). A bare name (no separators) uses ordinary assignment resolution,
    /// including an existing local or upvalue with the same name.
    fn compile_function_name_assign(
        &mut self,
        name: &str,
        value_reg: Reg,
        line: u32,
    ) -> Result<(), String> {
        let level = self.level();
        let mut segments = Vec::new();
        let mut rest = name;
        while let Some(idx) = rest.find(['.', ':']) {
            segments.push(&rest[..idx]);
            rest = &rest[idx + 1..];
        }
        segments.push(rest);
        if segments.len() == 1 {
            return self.compile_assign(&AssignTarget::Name(name.to_string()), value_reg, line);
        }
        let base = self.stack[level].alloc_reg();
        self.compile_name_into(segments[0], base, line)?;
        for field in &segments[1..segments.len() - 1] {
            let n = self.stack[level].push_name_const(field);
            self.stack[level].emit(Instr::GetField(base, base, n), line);
        }
        let last = self.stack[level].push_name_const(segments[segments.len() - 1]);
        self.stack[level].emit(Instr::SetField(base, last, value_reg), line);
        Ok(())
    }
}

/// True for a statement that doesn't count when deciding whether a label is
/// the last statement of its block (see `compile_statements`): a bare `;`
/// parses as an empty `Stmt::Block(vec![])`, and a run of labels at a
/// block's end (`::l1:: ::l2:: end`) should let *each* of them take the
/// "last statement" exception, not just the final one.
fn is_trivial_tail_stmt(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Label { .. }) || matches!(stmt, Stmt::Block(body) if body.is_empty())
}

/// A representative source line for `stmt`, used only to attribute a
/// scope's implicit fallthrough `Instr::CloseSlots` (emitted by
/// `FuncState::pop_scope`) to some line when the scope has no line of its
/// own (a bare `Stmt::Block`).
fn stmt_line(stmt: &Stmt) -> u32 {
    match stmt {
        Stmt::Global { line, .. }
        | Stmt::Label { line, .. }
        | Stmt::Goto { line, .. }
        | Stmt::MultiLocal { line, .. }
        | Stmt::MultiAssign { line, .. }
        | Stmt::Repeat { line, .. }
        | Stmt::Break { line }
        | Stmt::Local { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::If { line, .. }
        | Stmt::While { line, .. }
        | Stmt::NumericFor { line, .. }
        | Stmt::GenericFor { line, .. }
        | Stmt::Return { line, .. }
        | Stmt::MultiReturn { line, .. } => *line,
        Stmt::GlobalFunction(f) | Stmt::LocalFunction(f) => f.line,
        Stmt::Expr(e) => e.line,
        Stmt::Block(block) => block.last().map(stmt_line).unwrap_or(0),
    }
}
