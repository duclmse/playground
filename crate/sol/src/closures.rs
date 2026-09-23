//! Zero-allocation lowering for nonescaping typed local functions (M11).
//!
//! Captures become leading parameters on a lambda-lifted top-level function.
//! The current value is supplied at each direct call, so mutations in the
//! enclosing scope are visible without an environment allocation. Mutations
//! from inside the closure, escaping function values, and shared mutable cells
//! are rejected until their GC environment representation is available.

use std::collections::{HashMap, HashSet};

use crate::ast::{self, AssignTarget, Expr, ExprKind, Function, Stmt, TypeName};

#[derive(Clone)]
struct Binding {
    lifted_name: String,
    captures: Vec<String>,
}

pub fn lower(program: &mut ast::Program) -> Result<(), String> {
    let mut lifted = Vec::new();
    for function in &mut program.functions {
        let mut environment: HashMap<String, TypeName> = function.params.iter().cloned().collect();
        let mutations = assigned_names(&function.body);
        let parent = function.name.clone();
        function.body = lower_block(
            std::mem::take(&mut function.body),
            &mut environment,
            &HashMap::new(),
            &mut lifted,
            &parent,
            &mutations,
            function.source_file.clone(),
        )?;
    }
    program.functions.extend(lifted);
    Ok(())
}

fn lower_block(
    block: Vec<Stmt>,
    environment: &mut HashMap<String, TypeName>,
    inherited_bindings: &HashMap<String, Binding>,
    lifted: &mut Vec<Function>,
    parent: &str,
    _mutations: &HashSet<String>,
    source_file: Option<String>,
) -> Result<Vec<Stmt>, String> {
    let mut output = Vec::new();
    let mut bindings = inherited_bindings.clone();
    for mut statement in block {
        match statement {
            Stmt::LocalFunction(mut function) => {
                if bindings.contains_key(&function.name) {
                    return Err(format!(
                        "line {}: duplicate local function '{}'",
                        function.line, function.name
                    ));
                }
                let free = free_names(&function);
                let mut captures = free
                    .iter()
                    .filter(|name| environment.contains_key(*name))
                    .cloned()
                    .collect::<Vec<_>>();
                captures.sort();
                let inner_mutations = assigned_names(&function.body);
                for capture in &captures {
                    let ty = environment
                        .get(capture)
                        .expect("capture came from environment");
                    if inner_mutations.contains(capture) {
                        return Err(format!(
                            "line {}: captured variable '{capture}' is assigned inside the closure; shared capture cells are not implemented yet",
                            function.line
                        ));
                    }
                    if !matches!(
                        ty,
                        TypeName::I64 | TypeName::F64 | TypeName::Bool | TypeName::String
                    ) {
                        return Err(format!(
                            "line {}: captured variable '{capture}' has type {ty:?}, which cannot cross a typed closure boundary yet",
                            function.line
                        ));
                    }
                }
                let lifted_name = format!(
                    "__closure_{}_{}_{}_{}",
                    sanitize(parent),
                    sanitize(&function.name),
                    function.line,
                    lifted.len()
                );
                let binding = Binding {
                    lifted_name: lifted_name.clone(),
                    captures: captures.clone(),
                };
                bindings.insert(function.name.clone(), binding.clone());

                let mut closure_environment = HashMap::new();
                let mut capture_params = Vec::with_capacity(captures.len());
                for capture in &captures {
                    let ty = environment[capture].clone();
                    closure_environment.insert(capture.clone(), ty.clone());
                    capture_params.push((capture.clone(), ty));
                }
                for (name, ty) in &function.params {
                    closure_environment.insert(name.clone(), ty.clone());
                }
                capture_params.extend(function.params);
                function.params = capture_params;
                let mut capture_annotations = vec![true; captures.len()];
                capture_annotations.extend(function.param_annotations);
                function.param_annotations = capture_annotations;
                function.name = lifted_name.clone();
                function.source_file = source_file.clone();
                rewrite_block(&mut function.body, &bindings)?;
                let nested_mutations = assigned_names(&function.body);
                function.body = lower_block(
                    function.body,
                    &mut closure_environment,
                    &bindings,
                    lifted,
                    &lifted_name,
                    &nested_mutations,
                    source_file.clone(),
                )?;
                lifted.push(function);
            }
            Stmt::Local {
                ref name,
                ref ty,
                ref mut value,
                line,
                ..
            } => {
                rewrite_expr(value, &bindings)?;
                // Unknown local types are harmless unless the local is captured.
                // Preserve those programs and let capture validation issue the
                // more specific closure-boundary diagnostic when needed.
                let inferred = ty
                    .clone()
                    .or_else(|| infer_type(value, environment))
                    .unwrap_or(TypeName::Any);
                if bindings
                    .values()
                    .any(|binding| binding.captures.contains(name))
                {
                    return Err(format!(
                        "line {line}: local '{name}' shadows an active captured variable"
                    ));
                }
                environment.insert(name.clone(), inferred);
                output.push(statement);
            }
            Stmt::Block(body) => {
                let mut nested_environment = environment.clone();
                output.push(Stmt::Block(lower_block(
                    body,
                    &mut nested_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?));
            }
            Stmt::If {
                ref mut cond,
                then_block,
                else_block,
                line,
            } => {
                rewrite_expr(cond, &bindings)?;
                let mut then_environment = environment.clone();
                let then_block = lower_block(
                    then_block,
                    &mut then_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?;
                let else_block = match else_block {
                    Some(block) => {
                        let mut else_environment = environment.clone();
                        Some(lower_block(
                            block,
                            &mut else_environment,
                            &bindings,
                            lifted,
                            parent,
                            _mutations,
                            source_file.clone(),
                        )?)
                    }
                    None => None,
                };
                output.push(Stmt::If {
                    cond: cond.clone(),
                    then_block,
                    else_block,
                    line,
                });
            }
            Stmt::While {
                ref mut cond,
                body,
                line,
            } => {
                rewrite_expr(cond, &bindings)?;
                let mut nested_environment = environment.clone();
                let body = lower_block(
                    body,
                    &mut nested_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?;
                output.push(Stmt::While {
                    cond: cond.clone(),
                    body,
                    line,
                });
            }
            Stmt::Repeat {
                body,
                ref mut cond,
                line,
            } => {
                rewrite_expr(cond, &bindings)?;
                let mut nested_environment = environment.clone();
                let body = lower_block(
                    body,
                    &mut nested_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?;
                output.push(Stmt::Repeat {
                    body,
                    cond: cond.clone(),
                    line,
                });
            }
            Stmt::NumericFor {
                ref var,
                ref mut start,
                ref mut stop,
                ref mut step,
                body,
                line,
            } => {
                rewrite_expr(start, &bindings)?;
                rewrite_expr(stop, &bindings)?;
                if let Some(step) = step {
                    rewrite_expr(step, &bindings)?;
                }
                let mut nested_environment = environment.clone();
                nested_environment.insert(var.clone(), TypeName::I64);
                let body = lower_block(
                    body,
                    &mut nested_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?;
                output.push(Stmt::NumericFor {
                    var: var.clone(),
                    start: start.clone(),
                    stop: stop.clone(),
                    step: step.clone(),
                    body,
                    line,
                });
            }
            Stmt::GenericFor {
                ref vars,
                ref mut iterators,
                body,
                line,
            } => {
                iterators
                    .iter_mut()
                    .try_for_each(|value| rewrite_expr(value, &bindings))?;
                let mut nested_environment = environment.clone();
                for var in vars {
                    nested_environment.insert(var.clone(), TypeName::Any);
                }
                let body = lower_block(
                    body,
                    &mut nested_environment,
                    &bindings,
                    lifted,
                    parent,
                    _mutations,
                    source_file.clone(),
                )?;
                output.push(Stmt::GenericFor {
                    vars: vars.clone(),
                    iterators: iterators.clone(),
                    body,
                    line,
                });
            }
            _ => {
                rewrite_stmt(&mut statement, &bindings)?;
                output.push(statement);
            }
        }
    }
    Ok(output)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn infer_type(expr: &Expr, environment: &HashMap<String, TypeName>) -> Option<TypeName> {
    match &expr.kind {
        ExprKind::IntLit(_) => Some(TypeName::I64),
        ExprKind::FloatLit(_) => Some(TypeName::F64),
        ExprKind::BoolLit(_) => Some(TypeName::Bool),
        ExprKind::StringLit(_) => Some(TypeName::String),
        ExprKind::Name(name) => environment.get(name).cloned(),
        _ => None,
    }
}

fn free_names(function: &Function) -> HashSet<String> {
    let mut declared: HashSet<String> = function
        .params
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    declared.insert(function.name.clone());
    let mut free = HashSet::new();
    collect_free_block(&function.body, &mut declared, &mut free);
    free
}

fn collect_free_block(block: &[Stmt], declared: &mut HashSet<String>, free: &mut HashSet<String>) {
    for statement in block {
        match statement {
            Stmt::Global { values, .. } => {
                values
                    .iter()
                    .for_each(|value| collect_free_expr(value, declared, free));
            }
            Stmt::MultiLocal { names, values, .. } => {
                values
                    .iter()
                    .for_each(|value| collect_free_expr(value, declared, free));
                declared.extend(names.iter().map(|(name, _, _, _)| name.clone()));
            }
            Stmt::GlobalFunction(function) => {
                for name in free_names(function) {
                    record_use(&name, declared, free);
                }
            }
            Stmt::LocalFunction(function) => {
                declared.insert(function.name.clone());
                for name in free_names(function) {
                    record_use(&name, declared, free);
                }
            }
            Stmt::Block(body) => {
                let mut nested = declared.clone();
                collect_free_block(body, &mut nested, free);
            }
            Stmt::Repeat { body, cond, .. } | Stmt::While { body, cond, .. } => {
                let mut nested = declared.clone();
                collect_free_block(body, &mut nested, free);
                collect_free_expr(cond, &nested, free);
            }
            Stmt::Expr(expr) => collect_free_expr(expr, declared, free),
            Stmt::Local { name, value, .. } => {
                collect_free_expr(value, declared, free);
                declared.insert(name.clone());
            }
            Stmt::Assign { target, value, .. } => {
                collect_free_target(target, declared, free);
                collect_free_expr(value, declared, free);
            }
            Stmt::MultiAssign {
                targets, values, ..
            } => {
                targets
                    .iter()
                    .for_each(|target| collect_free_target(target, declared, free));
                values
                    .iter()
                    .for_each(|value| collect_free_expr(value, declared, free));
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                collect_free_expr(cond, declared, free);
                let mut nested = declared.clone();
                collect_free_block(then_block, &mut nested, free);
                if let Some(block) = else_block {
                    let mut nested = declared.clone();
                    collect_free_block(block, &mut nested, free);
                }
            }
            Stmt::NumericFor {
                var,
                start,
                stop,
                step,
                body,
                ..
            } => {
                collect_free_expr(start, declared, free);
                collect_free_expr(stop, declared, free);
                if let Some(step) = step {
                    collect_free_expr(step, declared, free);
                }
                let mut nested = declared.clone();
                nested.insert(var.clone());
                collect_free_block(body, &mut nested, free);
            }
            Stmt::GenericFor {
                vars,
                iterators,
                body,
                ..
            } => {
                iterators
                    .iter()
                    .for_each(|value| collect_free_expr(value, declared, free));
                let mut nested = declared.clone();
                nested.extend(vars.iter().cloned());
                collect_free_block(body, &mut nested, free);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    collect_free_expr(value, declared, free);
                }
            }
            Stmt::MultiReturn { values, .. } => {
                values
                    .iter()
                    .for_each(|value| collect_free_expr(value, declared, free));
            }
            Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
        }
    }
}

fn record_use(name: &str, declared: &HashSet<String>, free: &mut HashSet<String>) {
    if !declared.contains(name) {
        free.insert(name.to_string());
    }
}

fn collect_free_target(
    target: &AssignTarget,
    declared: &HashSet<String>,
    free: &mut HashSet<String>,
) {
    match target {
        AssignTarget::Name(name) => record_use(name, declared, free),
        AssignTarget::Index(base, index) => {
            collect_free_expr(base, declared, free);
            collect_free_expr(index, declared, free);
        }
        AssignTarget::Field(base, _) => collect_free_expr(base, declared, free),
    }
}

fn collect_free_expr(expr: &Expr, declared: &HashSet<String>, free: &mut HashSet<String>) {
    match &expr.kind {
        ExprKind::Name(name) => record_use(name, declared, free),
        ExprKind::Call(name, args) => {
            record_use(name, declared, free);
            args.iter()
                .for_each(|arg| collect_free_expr(arg, declared, free));
        }
        ExprKind::CallExpr(callee, args) => {
            collect_free_expr(callee, declared, free);
            args.iter()
                .for_each(|arg| collect_free_expr(arg, declared, free));
        }
        ExprKind::MethodCall(base, _, args) => {
            collect_free_expr(base, declared, free);
            args.iter()
                .for_each(|arg| collect_free_expr(arg, declared, free));
        }
        ExprKind::Unary(_, inner)
        | ExprKind::Len(inner)
        | ExprKind::Field(inner, _)
        | ExprKind::Paren(inner) => collect_free_expr(inner, declared, free),
        ExprKind::TypeTest(inner, _) | ExprKind::Cast(inner, _) => {
            collect_free_expr(inner, declared, free)
        }
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            collect_free_expr(left, declared, free);
            collect_free_expr(right, declared, free);
        }
        ExprKind::Table(fields) => fields.iter().for_each(|field| match field {
            ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                collect_free_expr(value, declared, free)
            }
            ast::TableField::Key(key, value) => {
                collect_free_expr(key, declared, free);
                collect_free_expr(value, declared, free);
            }
        }),
        ExprKind::Function(function) => {
            for name in free_names(function) {
                record_use(&name, declared, free);
            }
        }
        ExprKind::StructLiteral(_, fields) => fields
            .iter()
            .for_each(|(_, value)| collect_free_expr(value, declared, free)),
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_) => {}
    }
}

fn assigned_names(block: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    for statement in block {
        match statement {
            Stmt::Assign {
                target: AssignTarget::Name(name),
                ..
            } => {
                names.insert(name.clone());
            }
            Stmt::MultiAssign { targets, .. } => {
                names.extend(targets.iter().filter_map(|target| match target {
                    AssignTarget::Name(name) => Some(name.clone()),
                    _ => None,
                }));
            }
            Stmt::Block(body)
            | Stmt::While { body, .. }
            | Stmt::Repeat { body, .. }
            | Stmt::NumericFor { body, .. }
            | Stmt::GenericFor { body, .. } => names.extend(assigned_names(body)),
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                names.extend(assigned_names(then_block));
                if let Some(block) = else_block {
                    names.extend(assigned_names(block));
                }
            }
            _ => {}
        }
    }
    names
}

fn rewrite_block(block: &mut [Stmt], bindings: &HashMap<String, Binding>) -> Result<(), String> {
    block
        .iter_mut()
        .try_for_each(|statement| rewrite_stmt(statement, bindings))
}

fn rewrite_stmt(statement: &mut Stmt, bindings: &HashMap<String, Binding>) -> Result<(), String> {
    match statement {
        Stmt::Global { values, .. } | Stmt::MultiLocal { values, .. } => values
            .iter_mut()
            .try_for_each(|value| rewrite_expr(value, bindings)),
        Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
            rewrite_block(&mut function.body, bindings)
        }
        Stmt::Block(body) => rewrite_block(body, bindings),
        Stmt::Repeat { body, cond, .. } | Stmt::While { body, cond, .. } => {
            rewrite_block(body, bindings)?;
            rewrite_expr(cond, bindings)
        }
        Stmt::Expr(expr) => rewrite_expr(expr, bindings),
        Stmt::Local { value, .. } => rewrite_expr(value, bindings),
        Stmt::Assign { target, value, .. } => {
            rewrite_target(target, bindings)?;
            rewrite_expr(value, bindings)
        }
        Stmt::MultiAssign {
            targets, values, ..
        } => {
            targets
                .iter_mut()
                .try_for_each(|target| rewrite_target(target, bindings))?;
            values
                .iter_mut()
                .try_for_each(|value| rewrite_expr(value, bindings))
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            rewrite_expr(cond, bindings)?;
            rewrite_block(then_block, bindings)?;
            if let Some(block) = else_block {
                rewrite_block(block, bindings)?;
            }
            Ok(())
        }
        Stmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            rewrite_expr(start, bindings)?;
            rewrite_expr(stop, bindings)?;
            if let Some(step) = step {
                rewrite_expr(step, bindings)?;
            }
            rewrite_block(body, bindings)
        }
        Stmt::GenericFor {
            iterators, body, ..
        } => {
            iterators
                .iter_mut()
                .try_for_each(|value| rewrite_expr(value, bindings))?;
            rewrite_block(body, bindings)
        }
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                rewrite_expr(value, bindings)?;
            }
            Ok(())
        }
        Stmt::MultiReturn { values, .. } => values
            .iter_mut()
            .try_for_each(|value| rewrite_expr(value, bindings)),
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => Ok(()),
    }
}

fn rewrite_target(
    target: &mut AssignTarget,
    bindings: &HashMap<String, Binding>,
) -> Result<(), String> {
    match target {
        AssignTarget::Name(name) if bindings.contains_key(name) => {
            Err(format!("cannot assign to local function '{name}'"))
        }
        AssignTarget::Name(_) => Ok(()),
        AssignTarget::Index(base, index) => {
            rewrite_expr(base, bindings)?;
            rewrite_expr(index, bindings)
        }
        AssignTarget::Field(base, _) => rewrite_expr(base, bindings),
    }
}

fn rewrite_expr(expr: &mut Expr, bindings: &HashMap<String, Binding>) -> Result<(), String> {
    match &mut expr.kind {
        ExprKind::Name(name) if bindings.contains_key(name) => {
            let binding = &bindings[name];
            if binding.captures.is_empty() {
                *name = binding.lifted_name.clone();
                Ok(())
            } else {
                Err(format!(
                    "line {}: local function '{name}' escapes; heap closure environments are not implemented yet",
                    expr.line
                ))
            }
        }
        ExprKind::Call(name, args) => {
            args.iter_mut()
                .try_for_each(|arg| rewrite_expr(arg, bindings))?;
            if let Some(binding) = bindings.get(name) {
                let mut expanded = binding
                    .captures
                    .iter()
                    .map(|capture| Expr {
                        kind: ExprKind::Name(capture.clone()),
                        line: expr.line,
                    })
                    .collect::<Vec<_>>();
                expanded.append(args);
                *args = expanded;
                *name = binding.lifted_name.clone();
            }
            Ok(())
        }
        ExprKind::CallExpr(callee, args) => {
            rewrite_expr(callee, bindings)?;
            args.iter_mut()
                .try_for_each(|arg| rewrite_expr(arg, bindings))
        }
        ExprKind::MethodCall(base, _, args) => {
            rewrite_expr(base, bindings)?;
            args.iter_mut()
                .try_for_each(|arg| rewrite_expr(arg, bindings))
        }
        ExprKind::Unary(_, inner)
        | ExprKind::Len(inner)
        | ExprKind::Field(inner, _)
        | ExprKind::Paren(inner) => rewrite_expr(inner, bindings),
        ExprKind::TypeTest(inner, _) | ExprKind::Cast(inner, _) => rewrite_expr(inner, bindings),
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            rewrite_expr(left, bindings)?;
            rewrite_expr(right, bindings)
        }
        ExprKind::Table(fields) => fields.iter_mut().try_for_each(|field| match field {
            ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                rewrite_expr(value, bindings)
            }
            ast::TableField::Key(key, value) => {
                rewrite_expr(key, bindings)?;
                rewrite_expr(value, bindings)
            }
        }),
        ExprKind::Function(_) => Err(format!(
            "line {}: typed anonymous closures require a heap environment",
            expr.line
        )),
        ExprKind::StructLiteral(_, fields) => fields
            .iter_mut()
            .try_for_each(|(_, value)| rewrite_expr(value, bindings)),
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => Ok(()),
    }
}
