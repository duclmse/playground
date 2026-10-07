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
                if name.contains('.') {
                    // Only module lowering creates a dotted Name node;
                    // source-level field access is an ExprKind::Field.
                    self.emit_environment_get(level, name, r, line);
                } else {
                    self.compile_name_into(name, r, line)?;
                }
                Ok(r)
            }
            ExprKind::Function(function) => {
                let proto = self.compile_function(function)?;
                let level = self.level();
                let idx = self.stack[level].nested.len() as u16;
                self.stack[level].nested.push(proto);
                let dst = self.stack[level].alloc_reg();
                // Matches `Stmt::GlobalFunction`/`Stmt::LocalFunction`: real
                // Lua's `OP_CLOSURE` isn't emitted until the whole function
                // body has been parsed, tagged with that point's
                // `lastline` (`function.end_line`), not this expression's
                // own starting `line`.
                self.stack[level].emit(Instr::NewClosure(dst, idx), function.end_line);
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
                let entry = self.stack[level].next_reg;
                let value_reg = self.compile_expr(value)?;
                self.stack.last_mut().unwrap().free_reg(value_reg, entry);
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
                let entry = self.stack[level].next_reg;
                let value_reg = self.compile_expr(value)?;
                self.stack.last_mut().unwrap().free_reg(value_reg, entry);
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::Len(dst, value_reg), line);
                Ok(dst)
            }
            ExprKind::Binary(op, left, right) => {
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    let entry = self.stack[level].next_reg;
                    let left_reg = self.compile_expr(left)?;
                    self.stack.last_mut().unwrap().free_reg(left_reg, entry);
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
                let entry = self.stack[level].next_reg;
                // Lua parses and emits the left operand only after it has
                // scanned the binary operator.  Its line table therefore
                // records the operator's line for *all* code needed to
                // materialize that operand.  Our AST-first compiler already
                // has the left operand available before this point, so carry
                // the operator's line explicitly while compiling it.  This
                // matters for `debug.sethook("l")` when an operator is split
                // from its left operand by newlines (db.lua's line-info
                // stress test), not merely for diagnostic cosmetics.
                let left_at_operator = with_line(left, line);
                let left_reg = self.compile_expr(&left_at_operator)?;
                let right_reg = self.compile_expr(right)?;
                self.stack.last_mut().unwrap().free_reg(right_reg, entry);
                self.stack.last_mut().unwrap().free_reg(left_reg, entry);
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
                let entry = self.stack[level].next_reg;
                let base_reg = self.compile_expr(base)?;
                let index_reg = self.compile_expr(index)?;
                self.stack.last_mut().unwrap().free_reg(index_reg, entry);
                self.stack.last_mut().unwrap().free_reg(base_reg, entry);
                let dst = self.stack.last_mut().unwrap().alloc_reg();
                self.stack
                    .last_mut()
                    .unwrap()
                    .emit(Instr::GetIndex(dst, base_reg, index_reg), line);
                Ok(dst)
            }
            ExprKind::Field(base, field) => {
                let entry = self.stack[level].next_reg;
                let base_reg = self.compile_expr(base)?;
                self.stack.last_mut().unwrap().free_reg(base_reg, entry);
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

/// Returns an expression whose executable subexpressions carry `line` in
/// bytecode debug metadata.  Nested function bodies deliberately retain their
/// own source lines: only creating the closure belongs to the outer
/// expression, and `compile_expr` already attributes that to its closing
/// `end` like Lua does.
fn with_line(expr: &Expr, line: u32) -> Expr {
    let kind = match &expr.kind {
        ExprKind::Table(fields) => ExprKind::Table(
            fields
                .iter()
                .map(|field| match field {
                    TableField::Value(value) => TableField::Value(with_line(value, line)),
                    TableField::Named(name, value) => {
                        TableField::Named(name.clone(), with_line(value, line))
                    }
                    TableField::Key(key, value) => {
                        TableField::Key(with_line(key, line), with_line(value, line))
                    }
                })
                .collect(),
        ),
        ExprKind::Function(function) => ExprKind::Function(function.clone()),
        ExprKind::Unary(op, value) => ExprKind::Unary(*op, Box::new(with_line(value, line))),
        ExprKind::Binary(op, left, right) => ExprKind::Binary(
            *op,
            Box::new(with_line(left, line)),
            Box::new(with_line(right, line)),
        ),
        ExprKind::Call(name, args) => ExprKind::Call(
            name.clone(),
            args.iter().map(|arg| with_line(arg, line)).collect(),
        ),
        ExprKind::CallExpr(callee, args) => ExprKind::CallExpr(
            Box::new(with_line(callee, line)),
            args.iter().map(|arg| with_line(arg, line)).collect(),
        ),
        ExprKind::MethodCall(receiver, method, args) => ExprKind::MethodCall(
            Box::new(with_line(receiver, line)),
            method.clone(),
            args.iter().map(|arg| with_line(arg, line)).collect(),
        ),
        ExprKind::Index(base, index) => ExprKind::Index(
            Box::new(with_line(base, line)),
            Box::new(with_line(index, line)),
        ),
        ExprKind::Len(value) => ExprKind::Len(Box::new(with_line(value, line))),
        ExprKind::StructLiteral(name, fields) => ExprKind::StructLiteral(
            name.clone(),
            fields
                .iter()
                .map(|(name, value)| (name.clone(), with_line(value, line)))
                .collect(),
        ),
        ExprKind::Field(base, field) => {
            ExprKind::Field(Box::new(with_line(base, line)), field.clone())
        }
        ExprKind::TypeTest(value, ty) => {
            ExprKind::TypeTest(Box::new(with_line(value, line)), ty.clone())
        }
        ExprKind::Cast(value, ty) => ExprKind::Cast(Box::new(with_line(value, line)), ty.clone()),
        ExprKind::Paren(value) => ExprKind::Paren(Box::new(with_line(value, line))),
        ExprKind::StringLit(value) => ExprKind::StringLit(value.clone()),
        ExprKind::NilLit => ExprKind::NilLit,
        ExprKind::IntLit(value) => ExprKind::IntLit(*value),
        ExprKind::FloatLit(value) => ExprKind::FloatLit(*value),
        ExprKind::BoolLit(value) => ExprKind::BoolLit(*value),
        ExprKind::Name(name) => ExprKind::Name(name.clone()),
        ExprKind::Vararg => ExprKind::Vararg,
    };
    Expr { kind, line }
}

/// The latest source line represented by an expression.  Lua uses its lexer
/// cursor's `lastline` for assignment stores, which is normally the final
/// token of the right-hand expression rather than the line where the
/// assignment target started.  The AST does not retain every delimiter span,
/// but its rightmost executable node is the relevant line for bytecode line
/// hooks (and is exact for the indexed/binary shape exercised by db.lua).
pub(super) fn expression_last_line(expr: &Expr) -> u32 {
    let child_line = match &expr.kind {
        ExprKind::Table(fields) => fields
            .iter()
            .map(|field| match field {
                TableField::Value(value) | TableField::Named(_, value) => {
                    expression_last_line(value)
                }
                TableField::Key(_, value) => expression_last_line(value),
            })
            .max(),
        ExprKind::Function(function) => Some(function.end_line),
        ExprKind::Unary(_, value)
        | ExprKind::Len(value)
        | ExprKind::Paren(value)
        | ExprKind::TypeTest(value, _)
        | ExprKind::Cast(value, _) => Some(expression_last_line(value)),
        ExprKind::Binary(_, _, right) => Some(expression_last_line(right)),
        ExprKind::Call(_, args) => args.last().map(expression_last_line),
        ExprKind::CallExpr(_, args) | ExprKind::MethodCall(_, _, args) => {
            args.last().map(expression_last_line)
        }
        ExprKind::Index(_, index) => Some(expression_last_line(index)),
        ExprKind::StructLiteral(_, fields) => {
            fields.last().map(|(_, value)| expression_last_line(value))
        }
        ExprKind::Field(base, _) => Some(expression_last_line(base)),
        ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_)
        | ExprKind::Vararg => None,
    };
    child_line.unwrap_or(expr.line).max(expr.line)
}
