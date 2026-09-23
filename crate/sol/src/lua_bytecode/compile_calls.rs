//! Expression-list adjustment, call/method-call/vararg compilation, and
//! name/`_ENV` resolution helpers shared by statement and expression
//! compilation.

use std::rc::Rc;

use sol_core::ValueCount;

use crate::ast::{Expr, ExprKind};

use super::{value_count, Compiler, Instr, Reg, Resolved};

impl Compiler {
    /// Compiles an expression list with Lua's adjustment rule (only the final
    /// expression expands) into exactly `count` registers, nil-padding or
    /// discarding as needed. The returned registers need not be contiguous.
    pub(super) fn compile_expr_list(
        &mut self,
        values: &[Expr],
        count: usize,
    ) -> Result<Vec<Reg>, String> {
        let mut regs = Vec::with_capacity(count);
        for (index, value) in values.iter().enumerate() {
            if index + 1 == values.len() && is_multi_expr(value) {
                let want = (count as i32 - regs.len() as i32).max(0);
                let base = self.compile_expr_multi_n(value, want)?;
                for i in 0..want {
                    regs.push(base + i as u16);
                }
            } else {
                regs.push(self.compile_expr(value)?);
            }
        }
        while regs.len() < count {
            let level = self.level();
            let r = self.stack[level].alloc_reg();
            self.stack[level].emit(Instr::LoadNil(r), 0);
            regs.push(r);
        }
        regs.truncate(count);
        Ok(regs)
    }

    /// Compiles an expression list keeping Lua's multi-result expansion of the
    /// final expression open (used for `return`/table-constructor trailing
    /// fields). Returns `(base_register, count)` where `count == -1` means
    /// "everything from `base` to the frame's current open-multi top", and
    /// otherwise the values occupy `base..base+count` contiguously.
    pub(super) fn compile_expr_list_multi(
        &mut self,
        values: &[Expr],
    ) -> Result<(Reg, i32), String> {
        if values.is_empty() {
            return Ok((0, 0));
        }
        let level = self.level();
        let base = self.stack[level].next_reg;
        let mut fixed: u16 = 0;
        for (index, value) in values.iter().enumerate() {
            let dst = base + fixed;
            if index + 1 == values.len() && is_multi_expr(value) {
                self.stack.last_mut().unwrap().reset_to(dst);
                let result_base = self.compile_expr_multi_n(value, -1)?;
                debug_assert_eq!(result_base, dst);
                return Ok((base, -1));
            }
            self.compile_into(value, dst)?;
            fixed += 1;
        }
        Ok((base, fixed as i32))
    }

    /// Compiles a call/method-call/vararg expression requesting exactly
    /// `want` results (`want == -1` means "keep open: however many the
    /// callee/vararg actually produces, extending the frame's top";
    /// `want >= 0` means the VM truncates/nil-pads to exactly that many).
    /// Any other expression only ever has one value; `want` must be `1` or
    /// `-1` in that case. Returns the base register the result(s) start at.
    pub(super) fn compile_expr_multi_n(&mut self, expr: &Expr, want: i32) -> Result<Reg, String> {
        match &expr.kind {
            ExprKind::Vararg => {
                let level = self.level();
                let base = self.stack[level].alloc_reg();
                for _ in 1..want.max(0) {
                    self.stack[level].alloc_reg();
                }
                self.stack[level].emit(Instr::Vararg(base, value_count(want)), expr.line);
                Ok(base)
            }
            ExprKind::Call(name, args) => {
                let base = self.compile_call_base(name, expr.line)?;
                let nargs = self.compile_call_args(args, base)?;
                let level = self.level();
                for _ in 1..want.max(0) {
                    self.stack[level].alloc_reg();
                }
                self.stack[level].emit(
                    Instr::Call(base, value_count(nargs), value_count(want)),
                    expr.line,
                );
                Ok(base)
            }
            ExprKind::CallExpr(callee, args) => {
                let level = self.level();
                let base = self.stack[level].alloc_reg();
                let callee_reg = self.compile_expr(callee)?;
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Move(base, callee_reg), expr.line);
                let nargs = self.compile_call_args(args, base)?;
                let level = self.level();
                for _ in 1..want.max(0) {
                    self.stack[level].alloc_reg();
                }
                self.stack[level].emit(
                    Instr::Call(base, value_count(nargs), value_count(want)),
                    expr.line,
                );
                Ok(base)
            }
            ExprKind::MethodCall(receiver, method, args) => {
                let base = self.compile_method_base(receiver, method, expr.line)?;
                // `self` occupies base+1 already, so explicit args must
                // start at base+2. `compile_call_args` places its first
                // argument right after whatever `base` it's given, so pass
                // `base + 1` to shift its whole contiguous range one
                // register over (this also keeps its internal `reset_to`
                // math correct for a trailing multi-value argument).
                let nargs = self.compile_call_args(args, base + 1)?;
                // `self` occupies base+1 already; explicit args follow it.
                let nargs = if nargs < 0 { nargs } else { nargs + 1 };
                let level = self.level();
                for _ in 1..want.max(0) {
                    self.stack[level].alloc_reg();
                }
                self.stack[level].emit(
                    Instr::Call(base, value_count(nargs), value_count(want)),
                    expr.line,
                );
                Ok(base)
            }
            _ => {
                debug_assert!(want == 1 || want < 0);
                self.compile_expr(expr)
            }
        }
    }

    pub(super) fn compile_tail_call(&mut self, expression: &Expr) -> Result<bool, String> {
        if !matches!(
            expression.kind,
            ExprKind::Call(..) | ExprKind::CallExpr(..) | ExprKind::MethodCall(..)
        ) {
            return Ok(false);
        }
        let base = self.compile_expr_multi_n(expression, -1)?;
        let instruction = self
            .stack
            .last_mut()
            .and_then(|state| state.instrs.last_mut())
            .expect("compiling a call emits its call instruction");
        let Instr::Call(call_base, arguments, ValueCount::Open) = instruction else {
            unreachable!("an open call expression ends in an open Call instruction")
        };
        debug_assert_eq!(*call_base, base);
        *instruction = Instr::TailCall(base, *arguments);
        Ok(true)
    }

    fn compile_call_base(&mut self, name: &str, line: u32) -> Result<Reg, String> {
        let level = self.level();
        let base = self.stack[level].alloc_reg();
        self.compile_name_into(name, base, line)?;
        Ok(base)
    }

    fn compile_method_base(
        &mut self,
        receiver: &Expr,
        method: &str,
        line: u32,
    ) -> Result<Reg, String> {
        // `base` must land exactly at whatever register the caller has
        // already reserved for this call (e.g. `compile_call_args`'s
        // trailing multi-value argument, which pre-reserves a contiguous
        // `dst` via `reset_to` before calling in here). So allocate `base`
        // and `self_reg` up front, before compiling the receiver - if the
        // receiver were compiled first, its own temp register would land
        // in the slot `base` needs, pushing `base` one register too high.
        let level = self.level();
        let base = self.stack[level].alloc_reg();
        let self_reg = self.stack[level].alloc_reg();
        debug_assert_eq!(self_reg, base + 1);
        let recv_reg = self.compile_expr(receiver)?;
        let name = self.stack[level].push_name_const(method);
        self.stack[level].emit(Instr::GetField(base, recv_reg, name), line);
        self.stack[level].emit(Instr::Move(self_reg, recv_reg), line);
        Ok(base)
    }

    /// Compiles call arguments into the contiguous register range starting
    /// right after `base`. Returns `nargs` with the usual `-1` = "open,
    /// consume to top" convention.
    fn compile_call_args(&mut self, args: &[Expr], base: Reg) -> Result<i32, String> {
        if args.is_empty() {
            return Ok(0);
        }
        let mut fixed: u16 = 0;
        for (index, arg) in args.iter().enumerate() {
            let dst = base + 1 + fixed;
            if index + 1 == args.len() && is_multi_expr(arg) {
                self.stack.last_mut().unwrap().reset_to(dst);
                let result_base = self.compile_expr_multi_n(arg, -1)?;
                debug_assert_eq!(result_base, dst);
                return Ok(-1);
            }
            self.compile_into(arg, dst)?;
            fixed += 1;
        }
        Ok(fixed as i32)
    }

    pub(super) fn compile_name_into(
        &mut self,
        name: &str,
        dst: Reg,
        line: u32,
    ) -> Result<(), String> {
        let level = self.level();
        if let Some((first, rest)) = name.split_once('.') {
            self.compile_name_into(first, dst, line)?;
            for field in rest.split('.') {
                let n = self.stack[level].push_name_const(field);
                self.stack[level].emit(Instr::GetField(dst, dst, n), line);
            }
            return Ok(());
        }
        match self.resolve(level, name) {
            Resolved::Local(reg, _) => {
                self.stack[level].emit(Instr::Move(dst, reg), line);
            }
            Resolved::Upval(idx, _) => {
                self.stack[level].emit(Instr::GetUpval(dst, idx), line);
            }
            Resolved::Global => {
                self.check_global_access(name, line)?;
                self.emit_environment_get(level, name, dst, line);
            }
        }
        Ok(())
    }

    pub(super) fn emit_environment_get(&mut self, level: usize, name: &str, dst: Reg, line: u32) {
        if name == "_ENV" {
            self.stack[level].emit(Instr::GetEnvironment(dst), line);
            return;
        }
        match self.resolve(level, "_ENV") {
            Resolved::Local(environment, _) => {
                let name = self.stack[level].push_name_const(name);
                self.stack[level].emit(Instr::GetField(dst, environment, name), line);
            }
            Resolved::Upval(environment, _) => {
                let environment_reg = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::GetUpval(environment_reg, environment), line);
                let name = self.stack[level].push_name_const(name);
                self.stack[level].emit(Instr::GetField(dst, environment_reg, name), line);
            }
            Resolved::Global => {
                self.stack[level].emit(Instr::GetGlobal(dst, Rc::from(name)), line);
            }
        }
    }

    /// Emits the runtime "already defined" guard real Lua 5.5 requires for a
    /// `global NAME = value` or `global function NAME` declaration: reads
    /// `name`'s current value and errors if it's non-nil. Callers must emit
    /// this *before* the corresponding `emit_environment_set`/store, since
    /// the check inspects the pre-declaration value.
    pub(super) fn emit_check_global_undefined(&mut self, level: usize, name: &str, line: u32) {
        let reg = self.stack[level].alloc_reg();
        self.emit_environment_get(level, name, reg, line);
        self.stack[level].emit(Instr::ErrorIfGlobalDefined(reg, Rc::from(name)), line);
    }

    pub(super) fn emit_environment_set(
        &mut self,
        level: usize,
        name: &str,
        value: Reg,
        constant: bool,
        declare: bool,
        line: u32,
    ) {
        // A bare (no `local`) assignment to `_ENV` itself must replace the
        // frame's effective environment for every later plain global
        // access in this same scope, not merely create/update an ordinary
        // global slot literally named `"_ENV"`: real Lua resolves a bare
        // `_ENV` reference through the chunk's implicit environment
        // upvalue, so reassigning it (without `local`) rebinds that same
        // upvalue rather than declaring a fresh one
        // (`lua-5.5.1-tests/attrib.lua`'s sub-package test loads a module
        // file whose first line is bare `_ENV = {}`, and every subsequent
        // "global" read/write in that file must go through the new table
        // while the caller's own globals stay untouched). This only
        // applies when `_ENV` has no local/upvalue binding in scope yet -
        // one that does is handled by the ordinary `Resolved::Local`/
        // `Upval` arms below, same as any other name - and never applies
        // to the unrelated declaring `global _ENV = ...` form (`declare`
        // true), which stays on the plain-global path and whatever error
        // `check_global_access`/`env_declared_as_global` already give it.
        if name == "_ENV" && !declare {
            if let Resolved::Global = self.resolve(level, "_ENV") {
                self.stack[level].emit(Instr::SetEnvironment(value), line);
                return;
            }
        }
        match self.resolve(level, "_ENV") {
            Resolved::Local(environment, _) => {
                let name = self.stack[level].push_name_const(name);
                self.stack[level].emit(Instr::SetField(environment, name, value), line);
            }
            Resolved::Upval(environment, _) => {
                let environment_reg = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::GetUpval(environment_reg, environment), line);
                let name = self.stack[level].push_name_const(name);
                self.stack[level].emit(Instr::SetField(environment_reg, name, value), line);
            }
            Resolved::Global => {
                self.stack[level].emit(
                    Instr::SetGlobal(Rc::from(name), value, constant, declare),
                    line,
                );
            }
        }
    }
}

pub(super) fn is_multi_expr(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Call(..) | ExprKind::CallExpr(..) | ExprKind::MethodCall(..) | ExprKind::Vararg
    )
}
