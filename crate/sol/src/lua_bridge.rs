// Support for `main.rs`'s per-function typed/dynamic split for `.lua` files
// (see `typeck::check_partitioned`): once functions are partitioned into a
// native set and a dynamic set, the dynamic set's *raw* AST (not the typed
// one - it never went through `typeck`) has to be scanned for calls that
// name a native function, so `main.rs` knows which native functions need a
// `LuaValue::Native` bridge binding at all. Native functions with no such
// call site are simply never bound into the interpreter's globals.

use std::collections::HashSet;

use crate::ast::{self, AssignTarget, Block, Expr, ExprKind, Stmt};

/// Every name that appears as a static call target (`ExprKind::Call`)
/// anywhere in `program.functions`' bodies whose name is in `dynamic`,
/// including inside nested function literals (`ExprKind::Function`) - those
/// still execute in the dynamic world and resolve calls the same way.
pub fn called_from_dynamic(program: &ast::Program, dynamic: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    for f in &program.functions {
        if dynamic.contains(&f.name) {
            walk_block(&f.body, &mut out);
        }
    }
    out
}

fn walk_block(block: &Block, out: &mut HashSet<String>) {
    for stmt in block {
        walk_stmt(stmt, out);
    }
}

fn walk_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Global { values, .. } => values.iter().for_each(|e| walk_expr(e, out)),
        Stmt::GlobalFunction(f) | Stmt::LocalFunction(f) => walk_block(&f.body, out),
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
        Stmt::MultiLocal { values, .. } => values.iter().for_each(|e| walk_expr(e, out)),
        Stmt::MultiAssign {
            targets, values, ..
        } => {
            for target in targets {
                walk_assign_target(target, out);
            }
            values.iter().for_each(|e| walk_expr(e, out));
        }
        Stmt::Block(block) => walk_block(block, out),
        Stmt::Repeat { body, cond, .. } => {
            walk_block(body, out);
            walk_expr(cond, out);
        }
        Stmt::Expr(e) => walk_expr(e, out),
        Stmt::Local { value, .. } => walk_expr(value, out),
        Stmt::Assign { target, value, .. } => {
            walk_assign_target(target, out);
            walk_expr(value, out);
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            walk_expr(cond, out);
            walk_block(then_block, out);
            if let Some(else_block) = else_block {
                walk_block(else_block, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, out);
            walk_block(body, out);
        }
        Stmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            walk_expr(start, out);
            walk_expr(stop, out);
            if let Some(step) = step {
                walk_expr(step, out);
            }
            walk_block(body, out);
        }
        Stmt::GenericFor {
            iterators, body, ..
        } => {
            iterators.iter().for_each(|e| walk_expr(e, out));
            walk_block(body, out);
        }
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                walk_expr(value, out);
            }
        }
        Stmt::MultiReturn { values, .. } => values.iter().for_each(|e| walk_expr(e, out)),
    }
}

fn walk_assign_target(target: &AssignTarget, out: &mut HashSet<String>) {
    match target {
        AssignTarget::Name(_) => {}
        AssignTarget::Index(base, index) => {
            walk_expr(base, out);
            walk_expr(index, out);
        }
        AssignTarget::Field(base, _) => walk_expr(base, out),
    }
}

fn walk_expr(expr: &Expr, out: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Table(fields) => {
            for field in fields {
                match field {
                    ast::TableField::Value(e) => walk_expr(e, out),
                    ast::TableField::Named(_, e) => walk_expr(e, out),
                    ast::TableField::Key(k, v) => {
                        walk_expr(k, out);
                        walk_expr(v, out);
                    }
                }
            }
        }
        ExprKind::Function(f) => walk_block(&f.body, out),
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => {}
        ExprKind::Unary(_, e) | ExprKind::Len(e) | ExprKind::Paren(e) => walk_expr(e, out),
        ExprKind::Binary(_, l, r) => {
            walk_expr(l, out);
            walk_expr(r, out);
        }
        ExprKind::Call(name, args) => {
            out.insert(name.clone());
            args.iter().for_each(|a| walk_expr(a, out));
        }
        ExprKind::CallExpr(callee, args) => {
            walk_expr(callee, out);
            args.iter().for_each(|a| walk_expr(a, out));
        }
        ExprKind::MethodCall(receiver, _, args) => {
            walk_expr(receiver, out);
            args.iter().for_each(|a| walk_expr(a, out));
        }
        ExprKind::Index(base, index) => {
            walk_expr(base, out);
            walk_expr(index, out);
        }
        ExprKind::StructLiteral(_, fields) => fields.iter().for_each(|(_, e)| walk_expr(e, out)),
        ExprKind::Field(base, _) => walk_expr(base, out),
        ExprKind::TypeTest(e, _) | ExprKind::Cast(e, _) => walk_expr(e, out),
    }
}
