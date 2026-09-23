//! Compile-time expansion of Sol type aliases (M12).

use std::collections::{HashMap, HashSet};

use crate::ast::{self, Expr, ExprKind, Stmt, TypeName};

pub fn expand(program: &mut ast::Program) -> Result<(), String> {
    let struct_names: HashSet<&str> = program
        .structs
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    let mut definitions = HashMap::new();
    for alias in &program.aliases {
        if struct_names.contains(alias.name.as_str()) || definitions.contains_key(&alias.name) {
            return Err(format!(
                "line {}: duplicate type declaration '{}'",
                alias.line, alias.name
            ));
        }
        definitions.insert(alias.name.clone(), (alias.target.clone(), alias.line));
    }

    // Resolve every definition even when it is otherwise unused so cycles and
    // malformed exported interfaces fail deterministically.
    for alias in &program.aliases {
        resolve_type(
            &TypeName::Struct(alias.name.clone()),
            &definitions,
            &mut Vec::new(),
            alias.line,
        )?;
    }
    for structure in &mut program.structs {
        for (_, ty) in &mut structure.fields {
            *ty = resolve_type(ty, &definitions, &mut Vec::new(), structure.line)?;
        }
    }
    for external in &mut program.externs {
        for (_, ty) in &mut external.params {
            *ty = resolve_type(ty, &definitions, &mut Vec::new(), external.line)?;
        }
        external.return_type = resolve_type(
            &external.return_type,
            &definitions,
            &mut Vec::new(),
            external.line,
        )?;
    }
    for function in &mut program.functions {
        expand_function(function, &definitions)?;
    }
    program.aliases.clear();
    Ok(())
}

fn resolve_type(
    ty: &TypeName,
    definitions: &HashMap<String, (TypeName, u32)>,
    stack: &mut Vec<String>,
    line: u32,
) -> Result<TypeName, String> {
    Ok(match ty {
        TypeName::Array(inner) => {
            TypeName::Array(Box::new(resolve_type(inner, definitions, stack, line)?))
        }
        TypeName::Map(key, value) => TypeName::Map(
            Box::new(resolve_type(key, definitions, stack, line)?),
            Box::new(resolve_type(value, definitions, stack, line)?),
        ),
        TypeName::Function {
            params,
            return_type,
        } => TypeName::Function {
            params: params
                .iter()
                .map(|param| resolve_type(param, definitions, stack, line))
                .collect::<Result<Vec<_>, _>>()?,
            return_type: Box::new(resolve_type(return_type, definitions, stack, line)?),
        },
        TypeName::Struct(name) if definitions.contains_key(name) => {
            if let Some(start) = stack.iter().position(|entry| entry == name) {
                let mut cycle = stack[start..].to_vec();
                cycle.push(name.clone());
                return Err(format!(
                    "line {line}: type alias cycle: {}",
                    cycle.join(" -> ")
                ));
            }
            stack.push(name.clone());
            let (target, definition_line) = &definitions[name];
            let resolved = resolve_type(target, definitions, stack, *definition_line)?;
            stack.pop();
            resolved
        }
        other => other.clone(),
    })
}

fn expand_function(
    function: &mut ast::Function,
    definitions: &HashMap<String, (TypeName, u32)>,
) -> Result<(), String> {
    for (_, ty) in &mut function.params {
        *ty = resolve_type(ty, definitions, &mut Vec::new(), function.line)?;
    }
    if let Some(ty) = &mut function.return_type {
        *ty = resolve_type(ty, definitions, &mut Vec::new(), function.line)?;
    }
    expand_block(&mut function.body, definitions)
}

fn expand_block(
    block: &mut [Stmt],
    definitions: &HashMap<String, (TypeName, u32)>,
) -> Result<(), String> {
    for statement in block {
        match statement {
            Stmt::Global { values, .. } => {
                for value in values {
                    expand_expr(value, definitions)?;
                }
            }
            Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
                expand_function(function, definitions)?;
            }
            Stmt::MultiLocal {
                names,
                values,
                line,
            } => {
                for (_, ty, _, _) in names {
                    if let Some(ty) = ty {
                        *ty = resolve_type(ty, definitions, &mut Vec::new(), *line)?;
                    }
                }
                for value in values {
                    expand_expr(value, definitions)?;
                }
            }
            Stmt::MultiAssign {
                targets, values, ..
            } => {
                for target in targets {
                    expand_target(target, definitions)?;
                }
                for value in values {
                    expand_expr(value, definitions)?;
                }
            }
            Stmt::Block(body) => expand_block(body, definitions)?,
            Stmt::Repeat { body, cond, .. } | Stmt::While { body, cond, .. } => {
                expand_block(body, definitions)?;
                expand_expr(cond, definitions)?;
            }
            Stmt::Expr(expr) => expand_expr(expr, definitions)?,
            Stmt::Local {
                ty, value, line, ..
            } => {
                if let Some(ty) = ty {
                    *ty = resolve_type(ty, definitions, &mut Vec::new(), *line)?;
                }
                expand_expr(value, definitions)?;
            }
            Stmt::Assign { target, value, .. } => {
                expand_target(target, definitions)?;
                expand_expr(value, definitions)?;
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                expand_expr(cond, definitions)?;
                expand_block(then_block, definitions)?;
                if let Some(block) = else_block {
                    expand_block(block, definitions)?;
                }
            }
            Stmt::NumericFor {
                start,
                stop,
                step,
                body,
                ..
            } => {
                expand_expr(start, definitions)?;
                expand_expr(stop, definitions)?;
                if let Some(step) = step {
                    expand_expr(step, definitions)?;
                }
                expand_block(body, definitions)?;
            }
            Stmt::GenericFor {
                iterators, body, ..
            } => {
                for iterator in iterators {
                    expand_expr(iterator, definitions)?;
                }
                expand_block(body, definitions)?;
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    expand_expr(value, definitions)?;
                }
            }
            Stmt::MultiReturn { values, .. } => {
                for value in values {
                    expand_expr(value, definitions)?;
                }
            }
            Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
        }
    }
    Ok(())
}

fn expand_target(
    target: &mut ast::AssignTarget,
    definitions: &HashMap<String, (TypeName, u32)>,
) -> Result<(), String> {
    match target {
        ast::AssignTarget::Name(_) => Ok(()),
        ast::AssignTarget::Index(base, index) => {
            expand_expr(base, definitions)?;
            expand_expr(index, definitions)
        }
        ast::AssignTarget::Field(base, _) => expand_expr(base, definitions),
    }
}

fn expand_expr(
    expr: &mut Expr,
    definitions: &HashMap<String, (TypeName, u32)>,
) -> Result<(), String> {
    match &mut expr.kind {
        ExprKind::Function(function) => expand_function(function, definitions),
        ExprKind::Unary(_, inner)
        | ExprKind::Len(inner)
        | ExprKind::Field(inner, _)
        | ExprKind::Paren(inner) => expand_expr(inner, definitions),
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            expand_expr(left, definitions)?;
            expand_expr(right, definitions)
        }
        ExprKind::Call(_, args) => {
            for arg in args {
                expand_expr(arg, definitions)?;
            }
            Ok(())
        }
        ExprKind::CallExpr(callee, args) | ExprKind::MethodCall(callee, _, args) => {
            expand_expr(callee, definitions)?;
            for arg in args {
                expand_expr(arg, definitions)?;
            }
            Ok(())
        }
        ExprKind::Table(fields) => {
            for field in fields {
                match field {
                    ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                        expand_expr(value, definitions)?;
                    }
                    ast::TableField::Key(key, value) => {
                        expand_expr(key, definitions)?;
                        expand_expr(value, definitions)?;
                    }
                }
            }
            Ok(())
        }
        ExprKind::StructLiteral(name, fields) => {
            if definitions.contains_key(name) {
                let resolved = resolve_type(
                    &TypeName::Struct(name.clone()),
                    definitions,
                    &mut Vec::new(),
                    expr.line,
                )?;
                let TypeName::Struct(resolved_name) = resolved else {
                    return Err(format!(
                        "line {}: type alias '{}' does not name a struct constructor",
                        expr.line, name
                    ));
                };
                *name = resolved_name;
            }
            for (_, value) in fields {
                expand_expr(value, definitions)?;
            }
            Ok(())
        }
        ExprKind::TypeTest(inner, ty) | ExprKind::Cast(inner, ty) => {
            expand_expr(inner, definitions)?;
            *ty = resolve_type(ty, definitions, &mut Vec::new(), expr.line)?;
            Ok(())
        }
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => Ok(()),
    }
}
