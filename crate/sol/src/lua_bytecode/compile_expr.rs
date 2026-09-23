//! Expression compilation: `Compiler::compile_expr`, a single large match
//! over `ExprKind` covering literals, names, closures, table constructors,
//! unary/binary operators, calls, indexing, and field access.

use crate::ast::{BinaryOp, Expr, ExprKind, TableField, UnaryOp};

use super::compile_calls::is_multi_expr;
use super::func_state::LocalOrGlobal;
use super::{Compiler, Const, Instr};

impl Compiler {
    pub(super) fn compile_expr(&mut self, expr: &Expr) -> Result<super::Reg, String> {
        let level = self.level();
        let line = expr.line;
        match &expr.kind {
            ExprKind::NilLit => {
                let r = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::LoadNil(r), line);
                Ok(r)
            }
            ExprKind::BoolLit(value) => {
                let r = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::LoadBool(r, *value), line);
                Ok(r)
            }
            ExprKind::IntLit(value) => {
                let r = self.stack[level].alloc_reg();
                let k = self.stack[level].push_const(Const::Integer(*value));
                self.stack[level].emit(Instr::LoadConst(r, k), line);
                Ok(r)
            }
            ExprKind::FloatLit(value) => {
                let r = self.stack[level].alloc_reg();
                let k = self.stack[level].push_const(Const::Float(*value));
                self.stack[level].emit(Instr::LoadConst(r, k), line);
                Ok(r)
            }
            ExprKind::StringLit(value) => {
                let r = self.stack[level].alloc_reg();
                let interned = self.intern_string_literal(value);
                let k = self.stack[level].push_const(Const::Str(interned));
                self.stack[level].emit(Instr::LoadConst(r, k), line);
                Ok(r)
            }
            ExprKind::Name(name) => {
                if let Some(LocalOrGlobal::Local(reg, _)) =
                    self.stack[level].find_local_or_global(name)
                {
                    return Ok(reg);
                }
                let r = self.stack[level].alloc_reg();
                self.compile_name_into(name, r, line)?;
                Ok(r)
            }
            ExprKind::Function(function) => {
                let proto = self.compile_function(function)?;
                let level = self.level();
                let idx = self.stack[level].nested.len() as u16;
                self.stack[level].nested.push(proto);
                let dst = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::NewClosure(dst, idx), line);
                Ok(dst)
            }
            ExprKind::Table(fields) => {
                let dst = self.stack[level].alloc_reg();
                self.stack[level].emit(Instr::NewTable(dst), line);
                let mut array_index: i64 = 1;
                for (index, field) in fields.iter().enumerate() {
                    match field {
                        TableField::Value(value) => {
                            if index + 1 == fields.len() && is_multi_expr(value) {
                                let base = self.compile_expr_multi_n(value, -1)?;
                                self.stack
                                    .last_mut()
                                    .unwrap()
                                    .emit(Instr::SetArrayMulti(dst, array_index, base), line);
                            } else {
                                let value_reg = self.compile_expr(value)?;
                                self.stack
                                    .last_mut()
                                    .unwrap()
                                    .emit(Instr::SetArrayItem(dst, array_index, value_reg), line);
                                array_index += 1;
                                // Each field's value lives only long enough to be
                                // stored into the table, so its register must be
                                // reclaimed here - otherwise a constructor with more
                                // fields than fit in a `Reg` (e.g. a table literal
                                // with tens of thousands of elements) would overflow
                                // `next_reg` instead of reusing a handful of temps.
                                self.stack.last_mut().unwrap().reset_to(dst + 1);
                            }
                        }
                        TableField::Named(name, value) => {
                            let value_reg = self.compile_expr(value)?;
                            let n = self
                                .stack
                                .last_mut()
                                .unwrap()
                                .push_name_const(name.as_str());
                            self.stack
                                .last_mut()
                                .unwrap()
                                .emit(Instr::SetField(dst, n, value_reg), line);
                            self.stack.last_mut().unwrap().reset_to(dst + 1);
                        }
                        TableField::Key(key, value) => {
                            let key_reg = self.compile_expr(key)?;
                            let value_reg = self.compile_expr(value)?;
                            self.stack
                                .last_mut()
                                .unwrap()
                                .emit(Instr::SetIndex(dst, key_reg, value_reg), line);
                            self.stack.last_mut().unwrap().reset_to(dst + 1);
                        }
                    }
                }
                Ok(dst)
            }
            ExprKind::Unary(op, value) => {
                let value_reg = self.compile_expr(value)?;
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                let instr = match op {
                    UnaryOp::Not => Instr::Not(dst, value_reg),
                    UnaryOp::Neg => Instr::Neg(dst, value_reg),
                    UnaryOp::BitNot => Instr::BitNot(dst, value_reg),
                };
                self.stack.last_mut().unwrap().emit(instr, line);
                Ok(dst)
            }
            ExprKind::Len(value) => {
                let value_reg = self.compile_expr(value)?;
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Len(dst, value_reg), line);
                Ok(dst)
            }
            ExprKind::Binary(op, left, right) => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    let left_reg = self.compile_expr(left)?;
                    let dst = self.stack.last_mut().unwrap().alloc_reg();
                    if dst != left_reg {
                        self.stack
                            .last_mut()
                            .unwrap()
                            .emit(Instr::Move(dst, left_reg), line);
                    }
                    let jump = match op {
                        BinaryOp::And => self
                            .stack
                            .last_mut()
                            .unwrap()
                            .emit(Instr::JumpIfFalse(dst, 0), line),
                        _ => self
                            .stack
                            .last_mut()
                            .unwrap()
                            .emit(Instr::JumpIfTrue(dst, 0), line),
                    };
                    self.compile_into(right, dst)?;
                    // The result of `and`/`or` comes from one of two
                    // control-flow paths, so it has no single Lua source
                    // name.  A self-move is a semantic no-op but serves as
                    // a compact bytecode debug-name barrier for
                    // `describe_register`; without it, an error raised by
                    // a later use of the joined value is incorrectly
                    // attributed to the right-hand branch's name.
                    self.stack
                        .last_mut()
                        .unwrap()
                        .emit(Instr::Move(dst, dst), line);
                    let end = self.stack.last_mut().unwrap().here();
                    self.stack.last_mut().unwrap().patch_jump(jump, end as i32);
                    return Ok(dst);
                }
                let left_reg = self.compile_expr(left)?;
                let right_reg = self.compile_expr(right)?;
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                let specialized = self.optimization_plan.as_ref().is_some_and(|plan| {
                    plan.proves_integer_binary(&self.stack[self.level()].name, line, *op)
                });
                self.stack.last_mut().unwrap().emit(
                    if specialized {
                        Instr::IntegerBinary(*op, dst, left_reg, right_reg)
                    } else {
                        Instr::Binary(*op, dst, left_reg, right_reg)
                    },
                    line,
                );
                Ok(dst)
            }
            ExprKind::Call(..)
            | ExprKind::CallExpr(..)
            | ExprKind::MethodCall(..)
            | ExprKind::Vararg => self.compile_expr_multi_n(expr, 1),
            ExprKind::Paren(inner) => self.compile_expr(inner),
            ExprKind::Index(base, index) => {
                let base_reg = self.compile_expr(base)?;
                let index_reg = self.compile_expr(index)?;
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::GetIndex(dst, base_reg, index_reg), line);
                Ok(dst)
            }
            ExprKind::Field(base, field) => {
                let base_reg = self.compile_expr(base)?;
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                let n = self
                    .stack
                    .last_mut()
                    .unwrap()
                    .push_name_const(field.as_str());
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::GetField(dst, base_reg, n), line);
                Ok(dst)
            }
            ExprKind::StructLiteral(..) | ExprKind::TypeTest(..) | ExprKind::Cast(..) => Err(
                format!("line {line}: typed expressions are unavailable in dynamic Lua"),
            ),
        }
    }
}
