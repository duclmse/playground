// Escape analysis + scalar replacement (faster_lua.md §12): a struct local
// that never leaves its function is replaced by one scalar local per
// field - no allocation, no pointer, no load/store.
//
// Scope is deliberately narrow: eligible locals are `local p = Struct
// {...}` literals, never reassigned as a whole and never used as a whole
// value (only as a `.field` base) - see `expr_leaks_local`.

use std::collections::HashMap;

use crate::types::*;

pub fn scalar_replace(program: &mut TProgram) {
    let structs = program.structs.clone();
    for f in &mut program.functions {
        scalar_replace_function(f, &structs);
    }
}

fn scalar_replace_function(f: &mut TFunction, structs: &HashMap<String, StructLayout>) {
    let mut candidates = Vec::new();
    collect_struct_literal_locals(&f.body, &mut candidates);

    let eligible: HashMap<LocalId, String> = candidates
        .into_iter()
        .filter(|(id, _)| !is_reassigned_as_whole(&f.body, *id) && !block_leaks_local(&f.body, *id))
        .collect();
    if eligible.is_empty() {
        return;
    }

    let mut next_id = f.local_count;
    let field_ids: HashMap<LocalId, Vec<LocalId>> = eligible
        .iter()
        .map(|(struct_id, struct_name)| {
            let layout = &structs[struct_name];
            let ids: Vec<LocalId> = (0..layout.fields.len())
                .map(|_| {
                    let id = next_id;
                    next_id += 1;
                    id
                })
                .collect();
            (*struct_id, ids)
        })
        .collect();

    f.body = transform_block(std::mem::take(&mut f.body), &field_ids);
    f.local_count = next_id;
}

/// Every `local id = StructName { ... }` literal, paired with the struct name.
fn collect_struct_literal_locals(stmts: &[TStmt], out: &mut Vec<(LocalId, String)>) {
    for s in stmts {
        match s {
            TStmt::Break => {}
            TStmt::Local { id, value } => {
                if let TExprKind::StructLiteral { name, .. } = &value.kind {
                    out.push((*id, name.clone()));
                }
            }
            TStmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_struct_literal_locals(then_block, out);
                collect_struct_literal_locals(else_block, out);
            }
            TStmt::While { body, .. } | TStmt::NumericFor { body, .. } => {
                collect_struct_literal_locals(body, out)
            }
            TStmt::Assign { .. }
            | TStmt::AssignIndex { .. }
            | TStmt::AssignField { .. }
            | TStmt::Return { .. } => {}
        }
    }
}

/// True if `id` is ever the target of a whole-value `Assign` (`p = ...`);
/// a field-level write (`p.x = ...`) doesn't count.
fn is_reassigned_as_whole(stmts: &[TStmt], id: LocalId) -> bool {
    stmts.iter().any(|s| match s {
        TStmt::Assign { id: assigned, .. } => *assigned == id,
        TStmt::Break
        | TStmt::AssignIndex { .. }
        | TStmt::AssignField { .. }
        | TStmt::Local { .. }
        | TStmt::Return { .. } => false,
        TStmt::If {
            then_block,
            else_block,
            ..
        } => is_reassigned_as_whole(then_block, id) || is_reassigned_as_whole(else_block, id),
        TStmt::While { body, .. } | TStmt::NumericFor { body, .. } => {
            is_reassigned_as_whole(body, id)
        }
    })
}

fn block_leaks_local(stmts: &[TStmt], id: LocalId) -> bool {
    stmts.iter().any(|s| stmt_leaks_local(s, id))
}

fn stmt_leaks_local(s: &TStmt, id: LocalId) -> bool {
    match s {
        TStmt::Break => false,
        TStmt::Local { value, .. } | TStmt::Assign { value, .. } => expr_leaks_local(value, id),
        TStmt::AssignIndex {
            array,
            index,
            value,
        } => {
            expr_leaks_local(array, id)
                || expr_leaks_local(index, id)
                || expr_leaks_local(value, id)
        }
        TStmt::AssignField { base, value, .. } => {
            base_use_leaks(base, id) || expr_leaks_local(value, id)
        }
        TStmt::If {
            cond,
            then_block,
            else_block,
        } => {
            expr_leaks_local(cond, id)
                || block_leaks_local(then_block, id)
                || block_leaks_local(else_block, id)
        }
        TStmt::While { cond, body } => expr_leaks_local(cond, id) || block_leaks_local(body, id),
        TStmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            expr_leaks_local(start, id)
                || expr_leaks_local(stop, id)
                || expr_leaks_local(step, id)
                || block_leaks_local(body, id)
        }
        TStmt::Return { value } => value.as_ref().is_some_and(|v| expr_leaks_local(v, id)),
    }
}

/// `Local(id)` as a `.field` base doesn't leak; anything else recurses normally.
fn base_use_leaks(base: &TExpr, id: LocalId) -> bool {
    if matches!(base.kind, TExprKind::Local(bid) if bid == id) {
        false
    } else {
        expr_leaks_local(base, id)
    }
}

/// True if `e` uses local `id` as a whole value anywhere but a `.field` base.
fn expr_leaks_local(e: &TExpr, id: LocalId) -> bool {
    match &e.kind {
        TExprKind::Field { base, .. } => base_use_leaks(base, id),
        TExprKind::Local(other) => *other == id,

        TExprKind::StringLit(_)
        | TExprKind::NilLit
        | TExprKind::IntLit(_)
        | TExprKind::FloatLit(_)
        | TExprKind::BoolLit(_)
        | TExprKind::FunctionRef(_) => false,

        TExprKind::Truth(inner)
        | TExprKind::Neg(inner)
        | TExprKind::Not(inner)
        | TExprKind::IntToFloat(inner)
        | TExprKind::Len(inner) => expr_leaks_local(inner, id),

        TExprKind::Arith(_, l, r)
        | TExprKind::Compare(_, l, r)
        | TExprKind::Logical(_, l, r)
        | TExprKind::Index(l, r) => expr_leaks_local(l, id) || expr_leaks_local(r, id),

        TExprKind::Call(_, args) => args.iter().any(|a| expr_leaks_local(a, id)),
        TExprKind::CallIndirect { callee, args } => {
            expr_leaks_local(callee, id) || args.iter().any(|a| expr_leaks_local(a, id))
        }
        TExprKind::NewArray { len, .. } => expr_leaks_local(len, id),
        TExprKind::ArrayLiteral { values, .. } => {
            values.iter().any(|value| expr_leaks_local(value, id))
        }
        TExprKind::ArrayMap { array, callback } => {
            expr_leaks_local(array, id) || expr_leaks_local(callback, id)
        }
        TExprKind::NewMap { .. } => false,
        TExprKind::MapLiteral { entries, .. } => entries
            .iter()
            .any(|(key, value)| expr_leaks_local(key, id) || expr_leaks_local(value, id)),
        TExprKind::MapNext { map, cursor }
        | TExprKind::MapKey { map, cursor }
        | TExprKind::MapValue { map, cursor } => {
            expr_leaks_local(map, id) || expr_leaks_local(cursor, id)
        }
        TExprKind::StructLiteral { fields, .. } => fields.iter().any(|f| expr_leaks_local(f, id)),
        TExprKind::Box(inner) => expr_leaks_local(inner, id),
        TExprKind::Unbox(inner, _) => expr_leaks_local(inner, id),
    }
}

// -- the transform itself (only reached for functions with >=1 eligible local) --

fn transform_block(stmts: Vec<TStmt>, field_ids: &HashMap<LocalId, Vec<LocalId>>) -> Vec<TStmt> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            TStmt::Break => out.push(TStmt::Break),
            TStmt::Local { id, value } if field_ids.contains_key(&id) => {
                let TExprKind::StructLiteral { fields, .. } = value.kind else {
                    unreachable!("collect_struct_literal_locals only ever collects StructLiteral-initialized locals")
                };
                for (field_id, field_expr) in field_ids[&id].iter().zip(fields) {
                    out.push(TStmt::Local {
                        id: *field_id,
                        value: transform_expr(field_expr, field_ids),
                    });
                }
            }
            TStmt::AssignField {
                base,
                field_index,
                value,
            } => {
                if let TExprKind::Local(base_id) = &base.kind {
                    if let Some(ids) = field_ids.get(base_id) {
                        out.push(TStmt::Assign {
                            id: ids[field_index],
                            value: transform_expr(value, field_ids),
                        });
                        continue;
                    }
                }
                out.push(TStmt::AssignField {
                    base: transform_expr(base, field_ids),
                    field_index,
                    value: transform_expr(value, field_ids),
                });
            }
            TStmt::Local { id, value } => out.push(TStmt::Local {
                id,
                value: transform_expr(value, field_ids),
            }),
            TStmt::Assign { id, value } => out.push(TStmt::Assign {
                id,
                value: transform_expr(value, field_ids),
            }),
            TStmt::AssignIndex {
                array,
                index,
                value,
            } => out.push(TStmt::AssignIndex {
                array: transform_expr(array, field_ids),
                index: transform_expr(index, field_ids),
                value: transform_expr(value, field_ids),
            }),
            TStmt::If {
                cond,
                then_block,
                else_block,
            } => out.push(TStmt::If {
                cond: transform_expr(cond, field_ids),
                then_block: transform_block(then_block, field_ids),
                else_block: transform_block(else_block, field_ids),
            }),
            TStmt::While { cond, body } => out.push(TStmt::While {
                cond: transform_expr(cond, field_ids),
                body: transform_block(body, field_ids),
            }),
            TStmt::NumericFor {
                id,
                stop_id,
                step_id,
                start,
                stop,
                step,
                body,
            } => out.push(TStmt::NumericFor {
                id,
                stop_id,
                step_id,
                start: transform_expr(start, field_ids),
                stop: transform_expr(stop, field_ids),
                step: transform_expr(step, field_ids),
                body: transform_block(body, field_ids),
            }),
            TStmt::Return { value } => out.push(TStmt::Return {
                value: value.map(|v| transform_expr(v, field_ids)),
            }),
        }
    }
    out
}

/// Rewrites `Field { base: Local(replaced_id), .. }` to a plain field-local read.
fn transform_expr(e: TExpr, field_ids: &HashMap<LocalId, Vec<LocalId>>) -> TExpr {
    match e.kind {
        TExprKind::Field { base, field_index } => {
            if let TExprKind::Local(base_id) = &base.kind {
                if let Some(ids) = field_ids.get(base_id) {
                    return TExpr {
                        kind: TExprKind::Local(ids[field_index]),
                        ty: e.ty,
                    };
                }
            }
            TExpr {
                ty: e.ty,
                kind: TExprKind::Field {
                    base: Box::new(transform_expr(*base, field_ids)),
                    field_index,
                },
            }
        }
        TExprKind::Truth(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::Truth(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::Neg(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::Neg(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::Not(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::Not(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::IntToFloat(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::IntToFloat(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::Arith(op, l, r) => TExpr {
            ty: e.ty,
            kind: TExprKind::Arith(
                op,
                Box::new(transform_expr(*l, field_ids)),
                Box::new(transform_expr(*r, field_ids)),
            ),
        },
        TExprKind::Compare(op, l, r) => TExpr {
            ty: e.ty,
            kind: TExprKind::Compare(
                op,
                Box::new(transform_expr(*l, field_ids)),
                Box::new(transform_expr(*r, field_ids)),
            ),
        },
        TExprKind::Logical(op, l, r) => TExpr {
            ty: e.ty,
            kind: TExprKind::Logical(
                op,
                Box::new(transform_expr(*l, field_ids)),
                Box::new(transform_expr(*r, field_ids)),
            ),
        },
        TExprKind::Len(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::Len(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::Index(a, i) => TExpr {
            ty: e.ty,
            kind: TExprKind::Index(
                Box::new(transform_expr(*a, field_ids)),
                Box::new(transform_expr(*i, field_ids)),
            ),
        },
        TExprKind::NewArray { elem, len } => TExpr {
            ty: e.ty,
            kind: TExprKind::NewArray {
                elem,
                len: Box::new(transform_expr(*len, field_ids)),
            },
        },
        TExprKind::ArrayLiteral { elem, values } => TExpr {
            ty: e.ty,
            kind: TExprKind::ArrayLiteral {
                elem,
                values: values
                    .into_iter()
                    .map(|value| transform_expr(value, field_ids))
                    .collect(),
            },
        },
        TExprKind::ArrayMap { array, callback } => TExpr {
            ty: e.ty,
            kind: TExprKind::ArrayMap {
                array: Box::new(transform_expr(*array, field_ids)),
                callback: Box::new(transform_expr(*callback, field_ids)),
            },
        },
        TExprKind::NewMap { key, value } => TExpr {
            ty: e.ty,
            kind: TExprKind::NewMap { key, value },
        },
        TExprKind::MapLiteral {
            key,
            value,
            entries,
        } => TExpr {
            ty: e.ty,
            kind: TExprKind::MapLiteral {
                key,
                value,
                entries: entries
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            transform_expr(key, field_ids),
                            transform_expr(value, field_ids),
                        )
                    })
                    .collect(),
            },
        },
        TExprKind::MapNext { map, cursor } => TExpr {
            ty: e.ty,
            kind: TExprKind::MapNext {
                map: Box::new(transform_expr(*map, field_ids)),
                cursor: Box::new(transform_expr(*cursor, field_ids)),
            },
        },
        TExprKind::MapKey { map, cursor } => TExpr {
            ty: e.ty,
            kind: TExprKind::MapKey {
                map: Box::new(transform_expr(*map, field_ids)),
                cursor: Box::new(transform_expr(*cursor, field_ids)),
            },
        },
        TExprKind::MapValue { map, cursor } => TExpr {
            ty: e.ty,
            kind: TExprKind::MapValue {
                map: Box::new(transform_expr(*map, field_ids)),
                cursor: Box::new(transform_expr(*cursor, field_ids)),
            },
        },
        TExprKind::Call(name, args) => TExpr {
            ty: e.ty,
            kind: TExprKind::Call(
                name,
                args.into_iter()
                    .map(|a| transform_expr(a, field_ids))
                    .collect(),
            ),
        },
        TExprKind::CallIndirect { callee, args } => TExpr {
            ty: e.ty,
            kind: TExprKind::CallIndirect {
                callee: Box::new(transform_expr(*callee, field_ids)),
                args: args
                    .into_iter()
                    .map(|a| transform_expr(a, field_ids))
                    .collect(),
            },
        },
        TExprKind::StructLiteral { name, fields } => TExpr {
            ty: e.ty,
            kind: TExprKind::StructLiteral {
                name,
                fields: fields
                    .into_iter()
                    .map(|f| transform_expr(f, field_ids))
                    .collect(),
            },
        },
        TExprKind::Box(inner) => TExpr {
            ty: e.ty,
            kind: TExprKind::Box(Box::new(transform_expr(*inner, field_ids))),
        },
        TExprKind::Unbox(inner, target) => TExpr {
            ty: e.ty,
            kind: TExprKind::Unbox(Box::new(transform_expr(*inner, field_ids)), target),
        },
        TExprKind::StringLit(_)
        | TExprKind::NilLit
        | TExprKind::IntLit(_)
        | TExprKind::FloatLit(_)
        | TExprKind::BoolLit(_)
        | TExprKind::Local(_)
        | TExprKind::FunctionRef(_) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, optimize, parser, typeck};

    fn replaced(src: &str) -> TProgram {
        let mut prog =
            typeck::check(&parser::parse(lexer::lex(src).unwrap()).unwrap(), false).unwrap();
        optimize::optimize(&mut prog);
        scalar_replace(&mut prog);
        prog
    }

    #[test]
    fn a_non_escaping_struct_local_is_replaced_by_one_local_per_field() {
        let prog = replaced(
            "struct Point { x: i64, y: i64 }\nfunction main(): i64\n  local p = Point { x = 1, y = 2 }\n  return p.x + p.y\nend\n",
        );
        let body = &prog.functions[0].body;
        assert_eq!(body.len(), 3, "{body:?}"); // 2 field locals + return
        assert!(matches!(body[0], TStmt::Local { .. }));
        assert!(matches!(body[1], TStmt::Local { .. }));
        let TStmt::Local { value: v0, .. } = &body[0] else {
            panic!()
        };
        let TStmt::Local { value: v1, .. } = &body[1] else {
            panic!()
        };
        assert!(matches!(v0.kind, TExprKind::IntLit(1)), "{:?}", v0.kind);
        assert!(matches!(v1.kind, TExprKind::IntLit(2)), "{:?}", v1.kind);
        let TStmt::Return { value: Some(ret) } = &body[2] else {
            panic!()
        };
        fn contains_field_or_struct(e: &TExpr) -> bool {
            matches!(
                e.kind,
                TExprKind::Field { .. } | TExprKind::StructLiteral { .. }
            ) || match &e.kind {
                TExprKind::Arith(_, l, r) => {
                    contains_field_or_struct(l) || contains_field_or_struct(r)
                }
                _ => false,
            }
        }
        assert!(!contains_field_or_struct(ret), "{ret:?}");
    }

    #[test]
    fn a_struct_returned_by_value_is_not_scalar_replaced() {
        let prog = replaced("struct Point { x: i64, y: i64 }\nfunction make(): Point\n  local p = Point { x = 1, y = 2 }\n  return p\nend\nfunction main(): i64\n  local p = make()\n  return p.x\nend\n");
        let body = &prog.functions[0].body; // `make`
        assert!(matches!(body[0], TStmt::Local { .. }));
        let TStmt::Local { value, .. } = &body[0] else {
            panic!()
        };
        assert!(
            matches!(value.kind, TExprKind::StructLiteral { .. }),
            "expected the struct literal to survive unreplaced: {value:?}"
        );
    }

    #[test]
    fn a_struct_passed_to_a_function_call_is_not_scalar_replaced() {
        let prog = replaced(
            "struct Point { x: i64, y: i64 }\nfunction sum(p: Point): i64\n  return p.x + p.y\nend\nfunction main(): i64\n  local p = Point { x = 1, y = 2 }\n  return sum(p)\nend\n",
        );
        let body = &prog.functions[1].body; // `main`
        let TStmt::Local { value, .. } = &body[0] else {
            panic!()
        };
        assert!(
            matches!(value.kind, TExprKind::StructLiteral { .. }),
            "expected the struct literal to survive unreplaced: {value:?}"
        );
    }
}
