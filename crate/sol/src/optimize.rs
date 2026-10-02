// AST-level constant folding, run after type-checking and before codegen -
// catches what Cranelift's IR-level mid-end never sees (sol's own AST).

use crate::ast::BinaryOp;
use crate::types::*;

pub fn optimize(program: &mut TProgram) {
    for f in &mut program.functions {
        f.body = fold_block(std::mem::take(&mut f.body));
    }
}

fn fold_block(stmts: TBlock) -> TBlock {
    stmts
        .into_iter()
        .map(|(line, s)| (line, fold_stmt(s)))
        .collect()
}

fn fold_stmt(stmt: TStmt) -> TStmt {
    match stmt {
        TStmt::Break => TStmt::Break,
        TStmt::Local { id, value } => TStmt::Local {
            id,
            value: fold_expr(value),
        },
        TStmt::Assign { id, value } => TStmt::Assign {
            id,
            value: fold_expr(value),
        },
        TStmt::AssignIndex {
            array,
            index,
            value,
        } => TStmt::AssignIndex {
            array: fold_expr(array),
            index: fold_expr(index),
            value: fold_expr(value),
        },
        TStmt::AssignField {
            base,
            field_index,
            value,
        } => TStmt::AssignField {
            base: fold_expr(base),
            field_index,
            value: fold_expr(value),
        },
        TStmt::If {
            cond,
            then_block,
            else_block,
        } => TStmt::If {
            cond: fold_expr(cond),
            then_block: fold_block(then_block),
            else_block: fold_block(else_block),
        },
        TStmt::While { cond, body } => TStmt::While {
            cond: fold_expr(cond),
            body: fold_block(body),
        },
        TStmt::NumericFor {
            id,
            stop_id,
            step_id,
            start,
            stop,
            step,
            body,
        } => TStmt::NumericFor {
            id,
            stop_id,
            step_id,
            start: fold_expr(start),
            stop: fold_expr(stop),
            step: fold_expr(step),
            body: fold_block(body),
        },
        TStmt::Return { value } => TStmt::Return {
            value: value.map(fold_expr),
        },
    }
}

fn fold_expr(expr: TExpr) -> TExpr {
    match expr.kind {
        TExprKind::Truth(e) => {
            let e = fold_expr(*e);
            let value = match &e.kind {
                TExprKind::NilLit => Some(false),
                TExprKind::BoolLit(b) => Some(*b),
                TExprKind::IntLit(_) | TExprKind::FloatLit(_) => Some(true),
                _ => None,
            };
            match value {
                Some(b) => TExpr {
                    kind: TExprKind::BoolLit(b),
                    ty: Type::Bool,
                },
                None => TExpr {
                    kind: TExprKind::Truth(Box::new(e)),
                    ty: Type::Bool,
                },
            }
        }
        TExprKind::Neg(e) => {
            let e = fold_expr(*e);
            match &e.kind {
                TExprKind::IntLit(n) => TExpr {
                    kind: TExprKind::IntLit(n.wrapping_neg()),
                    ty: e.ty,
                },
                TExprKind::FloatLit(n) => TExpr {
                    kind: TExprKind::FloatLit(-n),
                    ty: e.ty,
                },
                _ => TExpr {
                    ty: expr.ty,
                    kind: TExprKind::Neg(Box::new(e)),
                },
            }
        }
        TExprKind::Not(e) => {
            let e = fold_expr(*e);
            match &e.kind {
                TExprKind::BoolLit(b) => TExpr {
                    kind: TExprKind::BoolLit(!b),
                    ty: Type::Bool,
                },
                _ => TExpr {
                    ty: expr.ty,
                    kind: TExprKind::Not(Box::new(e)),
                },
            }
        }
        TExprKind::IntToFloat(e) => {
            let e = fold_expr(*e);
            match &e.kind {
                TExprKind::IntLit(n) => TExpr {
                    kind: TExprKind::FloatLit(*n as f64),
                    ty: Type::F64,
                },
                _ => TExpr {
                    ty: expr.ty,
                    kind: TExprKind::IntToFloat(Box::new(e)),
                },
            }
        }
        TExprKind::Arith(op, l, r) => {
            let l = fold_expr(*l);
            let r = fold_expr(*r);
            let folded = match (&l.kind, &r.kind) {
                (TExprKind::IntLit(a), TExprKind::IntLit(b)) => {
                    fold_int_arith(op, *a, *b).map(TExprKind::IntLit)
                }
                (TExprKind::FloatLit(a), TExprKind::FloatLit(b)) => {
                    Some(TExprKind::FloatLit(fold_float_arith(op, *a, *b)))
                }
                _ => None,
            };
            match folded {
                Some(kind) => TExpr { kind, ty: expr.ty },
                None => TExpr {
                    ty: expr.ty,
                    kind: TExprKind::Arith(op, Box::new(l), Box::new(r)),
                },
            }
        }
        TExprKind::Compare(op, l, r) => {
            let l = fold_expr(*l);
            let r = fold_expr(*r);
            let folded = match (&l.kind, &r.kind) {
                (TExprKind::IntLit(a), TExprKind::IntLit(b)) => Some(fold_int_compare(op, *a, *b)),
                (TExprKind::FloatLit(a), TExprKind::FloatLit(b)) => {
                    Some(fold_float_compare(op, *a, *b))
                }
                (TExprKind::BoolLit(a), TExprKind::BoolLit(b)) => Some(match op {
                    BinaryOp::Eq => a == b,
                    BinaryOp::NotEq => a != b,
                    _ => unreachable!("typeck only builds Eq/NotEq comparisons for bool operands"),
                }),
                _ => None,
            };
            match folded {
                Some(b) => TExpr {
                    kind: TExprKind::BoolLit(b),
                    ty: Type::Bool,
                },
                None => TExpr {
                    ty: Type::Bool,
                    kind: TExprKind::Compare(op, Box::new(l), Box::new(r)),
                },
            }
        }
        TExprKind::Logical(op, l, r) => {
            let l = fold_expr(*l);
            let r = fold_expr(*r);
            if let (TExprKind::BoolLit(a), TExprKind::BoolLit(b)) = (&l.kind, &r.kind) {
                let result = match op {
                    BinaryOp::And => *a && *b,
                    BinaryOp::Or => *a || *b,
                    _ => unreachable!("typeck only builds And/Or logical nodes"),
                };
                TExpr {
                    kind: TExprKind::BoolLit(result),
                    ty: Type::Bool,
                }
            } else {
                TExpr {
                    ty: expr.ty,
                    kind: TExprKind::Logical(op, Box::new(l), Box::new(r)),
                }
            }
        }
        TExprKind::Len(e) => TExpr {
            ty: expr.ty,
            kind: TExprKind::Len(Box::new(fold_expr(*e))),
        },
        TExprKind::Index(a, i) => TExpr {
            ty: expr.ty,
            kind: TExprKind::Index(Box::new(fold_expr(*a)), Box::new(fold_expr(*i))),
        },
        TExprKind::NewArray { elem, len } => TExpr {
            ty: expr.ty,
            kind: TExprKind::NewArray {
                elem,
                len: Box::new(fold_expr(*len)),
            },
        },
        TExprKind::ArrayLiteral { elem, values } => TExpr {
            ty: expr.ty,
            kind: TExprKind::ArrayLiteral {
                elem,
                values: values.into_iter().map(fold_expr).collect(),
            },
        },
        TExprKind::ArrayMap {
            array,
            callback,
            elem,
        } => TExpr {
            ty: expr.ty,
            kind: TExprKind::ArrayMap {
                array: Box::new(fold_expr(*array)),
                callback: Box::new(fold_expr(*callback)),
                elem,
            },
        },
        TExprKind::NewMap { key, value } => TExpr {
            ty: expr.ty,
            kind: TExprKind::NewMap { key, value },
        },
        TExprKind::MapLiteral {
            key,
            value,
            entries,
        } => TExpr {
            ty: expr.ty,
            kind: TExprKind::MapLiteral {
                key,
                value,
                entries: entries
                    .into_iter()
                    .map(|(key, value)| (fold_expr(key), fold_expr(value)))
                    .collect(),
            },
        },
        TExprKind::MapNext { map, cursor } => TExpr {
            ty: expr.ty,
            kind: TExprKind::MapNext {
                map: Box::new(fold_expr(*map)),
                cursor: Box::new(fold_expr(*cursor)),
            },
        },
        TExprKind::MapKey { map, cursor } => TExpr {
            ty: expr.ty,
            kind: TExprKind::MapKey {
                map: Box::new(fold_expr(*map)),
                cursor: Box::new(fold_expr(*cursor)),
            },
        },
        TExprKind::MapValue { map, cursor } => TExpr {
            ty: expr.ty,
            kind: TExprKind::MapValue {
                map: Box::new(fold_expr(*map)),
                cursor: Box::new(fold_expr(*cursor)),
            },
        },
        TExprKind::Call(name, args) => TExpr {
            ty: expr.ty,
            kind: TExprKind::Call(name, args.into_iter().map(fold_expr).collect()),
        },
        TExprKind::CallIndirect { callee, args } => TExpr {
            ty: expr.ty,
            kind: TExprKind::CallIndirect {
                callee: Box::new(fold_expr(*callee)),
                args: args.into_iter().map(fold_expr).collect(),
            },
        },
        TExprKind::StructLiteral { name, fields } => TExpr {
            ty: expr.ty,
            kind: TExprKind::StructLiteral {
                name,
                fields: fields.into_iter().map(fold_expr).collect(),
            },
        },
        TExprKind::Field { base, field_index } => TExpr {
            ty: expr.ty,
            kind: TExprKind::Field {
                base: Box::new(fold_expr(*base)),
                field_index,
            },
        },
        // No fold rule for box/unbox yet - just recurse.
        TExprKind::Box(e) => TExpr {
            ty: expr.ty,
            kind: TExprKind::Box(Box::new(fold_expr(*e))),
        },
        TExprKind::Unbox(e, target) => TExpr {
            ty: expr.ty,
            kind: TExprKind::Unbox(Box::new(fold_expr(*e)), target),
        },
        // Leaves - nothing to fold further.
        TExprKind::StringLit(_)
        | TExprKind::NilLit
        | TExprKind::IntLit(_)
        | TExprKind::FloatLit(_)
        | TExprKind::BoolLit(_)
        | TExprKind::Local(_)
        | TExprKind::FunctionRef(_) => expr,
    }
}

/// `None` for div/mod by a literal zero - left un-folded so the runtime trap stays the source of truth.
fn fold_int_arith(op: BinaryOp, a: i64, b: i64) -> Option<i64> {
    Some(match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        BinaryOp::FloorDiv if b != 0 => crate::numeric::floor_div(a, b),
        BinaryOp::Mod if b != 0 => crate::numeric::modulo(a, b),
        BinaryOp::BitAnd => a & b,
        BinaryOp::BitOr => a | b,
        BinaryOp::BitXor => a ^ b,
        BinaryOp::Shl => crate::numeric::shift(a, b, true),
        BinaryOp::Shr => crate::numeric::shift(a, b, false),
        _ => return None,
    })
}

fn fold_float_arith(op: BinaryOp, a: f64, b: f64) -> f64 {
    match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        BinaryOp::Mod => crate::numeric::modulo_float(a, b),
        BinaryOp::FloorDiv => (a / b).floor(),
        BinaryOp::Pow => a.powf(b),
        _ => unreachable!("not an arithmetic op"),
    }
}

fn fold_int_compare(op: BinaryOp, a: i64, b: i64) -> bool {
    match op {
        BinaryOp::Eq => a == b,
        BinaryOp::NotEq => a != b,
        BinaryOp::Lt => a < b,
        BinaryOp::Le => a <= b,
        BinaryOp::Gt => a > b,
        BinaryOp::Ge => a >= b,
        _ => unreachable!("not a comparison op"),
    }
}

fn fold_float_compare(op: BinaryOp, a: f64, b: f64) -> bool {
    match op {
        BinaryOp::Eq => a == b,
        BinaryOp::NotEq => a != b,
        BinaryOp::Lt => a < b,
        BinaryOp::Le => a <= b,
        BinaryOp::Gt => a > b,
        BinaryOp::Ge => a >= b,
        _ => unreachable!("not a comparison op"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser, typeck};

    fn optimized(src: &str) -> TProgram {
        let mut prog =
            typeck::check(&parser::parse(lexer::lex(src).unwrap()).unwrap(), false).unwrap();
        optimize(&mut prog);
        prog
    }

    #[test]
    fn constant_integer_arithmetic_folds_to_a_literal() {
        let prog = optimized("function main(): i64\n  return 2 + 3 * 4\nend\n");
        let TStmt::Return { value: Some(v) } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert!(matches!(v.kind, TExprKind::IntLit(14)), "{:?}", v.kind);
    }

    #[test]
    fn constant_comparison_folds_to_a_bool_literal() {
        let prog = optimized("function main(): bool\n  return 3 < 5\nend\n");
        let TStmt::Return { value: Some(v) } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert!(matches!(v.kind, TExprKind::BoolLit(true)), "{:?}", v.kind);
    }

    #[test]
    fn division_by_a_literal_zero_is_left_unfolded() {
        let prog = optimized("function main(): i64\n  return 1 // 0\nend\n");
        let TStmt::Return { value: Some(v) } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert!(
            matches!(v.kind, TExprKind::Arith(BinaryOp::FloorDiv, _, _)),
            "{:?}",
            v.kind
        );
    }

    #[test]
    fn folding_reaches_inside_nested_expressions_and_control_flow() {
        let prog = optimized("function main(): i64\n  if 1 + 1 == 2 then\n    return 10 * 10\n  end\n  return 0\nend\n");
        let TStmt::If {
            cond, then_block, ..
        } = &prog.functions[0].body[0].1
        else {
            panic!()
        };
        assert!(
            matches!(cond.kind, TExprKind::BoolLit(true)),
            "{:?}",
            cond.kind
        );
        let TStmt::Return { value: Some(v) } = &then_block[0].1 else {
            panic!()
        };
        assert!(matches!(v.kind, TExprKind::IntLit(100)), "{:?}", v.kind);
    }
}
