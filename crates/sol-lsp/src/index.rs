//! A lightweight, per-document symbol table built by walking the untyped
//! `ast::Program` (not the typed `TProgram`), so it's available even for
//! `.lua` files whose typeck fails with "requires dynamic runtime" - which
//! is the *normal* case for real Lua source, not an error condition (see
//! `sol::typeck::requires_dynamic_runtime`).
//!
//! This remains a presentation-oriented index for hover/completion/symbols.
//! Lexical resolution comes from `sol::binder::BindingIndex`, retained by the
//! document store. See `docs/sol-lsp.md`.

use sol::ast::{self, Expr, ExprKind, Stmt, TypeName};

#[derive(Debug, Clone)]
pub struct FunctionSymbol {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub return_type: Option<String>,
    pub line: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    pub is_extern: bool,
}

#[derive(Debug, Clone)]
pub struct StructSymbol {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct LocalSymbol {
    pub name: String,
    pub ty: Option<String>,
    pub line: u32,
    pub enclosing_function: String,
}

#[derive(Debug, Default)]
pub struct SymbolIndex {
    pub functions: Vec<FunctionSymbol>,
    pub structs: Vec<StructSymbol>,
    pub locals: Vec<LocalSymbol>,
}

pub fn render_type(ty: &TypeName) -> String {
    match ty {
        TypeName::I64 => "i64".to_string(),
        TypeName::F64 => "f64".to_string(),
        TypeName::Bool => "bool".to_string(),
        TypeName::Nil => "nil".to_string(),
        TypeName::String => "string".to_string(),
        TypeName::Any => "any".to_string(),
        TypeName::Array(inner) => format!("Array<{}>", render_type(inner)),
        TypeName::Map(k, v) => format!("Map<{}, {}>", render_type(k), render_type(v)),
        TypeName::Struct(name) => name.clone(),
        TypeName::Function {
            params,
            return_type,
        } => format!(
            "fn({}) -> {}",
            params.iter().map(render_type).collect::<Vec<_>>().join(", "),
            render_type(return_type)
        ),
    }
}

fn render_params(params: &[(String, TypeName)]) -> Vec<(String, String)> {
    params
        .iter()
        .map(|(name, ty)| (name.clone(), render_type(ty)))
        .collect()
}

pub fn build(program: &ast::Program) -> SymbolIndex {
    let mut index = SymbolIndex::default();

    for s in &program.structs {
        index.structs.push(StructSymbol {
            name: s.name.clone(),
            fields: s
                .fields
                .iter()
                .map(|(n, t)| (n.clone(), render_type(t)))
                .collect(),
            line: s.line,
        });
    }

    for e in &program.externs {
        index.functions.push(FunctionSymbol {
            name: e.name.clone(),
            params: render_params(&e.params),
            return_type: Some(render_type(&e.return_type)),
            line: e.line,
            start_byte: 0,
            end_byte: 0,
            is_extern: true,
        });
    }

    for f in &program.functions {
        index.functions.push(FunctionSymbol {
            name: f.name.clone(),
            params: render_params(&f.params),
            return_type: f.return_type.as_ref().map(render_type),
            line: f.line,
            start_byte: f.source_span.start,
            end_byte: f.source_span.end,
            is_extern: false,
        });
        for (name, ty) in &f.params {
            index.locals.push(LocalSymbol {
                name: name.clone(),
                ty: Some(render_type(ty)),
                line: f.line,
                enclosing_function: f.name.clone(),
            });
        }
        walk_block(&f.body, &f.name, &mut index);
    }

    index
}

fn walk_block(block: &ast::Block, enclosing: &str, index: &mut SymbolIndex) {
    for stmt in block {
        walk_stmt(stmt, enclosing, index);
    }
}

fn walk_stmt(stmt: &Stmt, enclosing: &str, index: &mut SymbolIndex) {
    match stmt {
        Stmt::Local { name, ty, value, line, .. } => {
            index.locals.push(LocalSymbol {
                name: name.clone(),
                ty: ty.as_ref().map(render_type),
                line: *line,
                enclosing_function: enclosing.to_string(),
            });
            walk_expr(value, index);
        }
        Stmt::MultiLocal { names, values, .. } => {
            for (name, ty, _) in names {
                index.locals.push(LocalSymbol {
                    name: name.clone(),
                    ty: ty.as_ref().map(render_type),
                    line: values.first().map(|e| e.line).unwrap_or(0),
                    enclosing_function: enclosing.to_string(),
                });
            }
            for v in values {
                walk_expr(v, index);
            }
        }
        Stmt::LocalFunction(f) | Stmt::GlobalFunction(f) => {
            index.functions.push(FunctionSymbol {
                name: f.name.clone(),
                params: render_params(&f.params),
                return_type: f.return_type.as_ref().map(render_type),
                line: f.line,
                start_byte: f.source_span.start,
                end_byte: f.source_span.end,
                is_extern: false,
            });
            for (name, ty) in &f.params {
                index.locals.push(LocalSymbol {
                    name: name.clone(),
                    ty: Some(render_type(ty)),
                    line: f.line,
                    enclosing_function: f.name.clone(),
                });
            }
            walk_block(&f.body, &f.name, index);
        }
        Stmt::Assign { target, value, .. } => {
            if let ast::AssignTarget::Index(a, b) = target {
                walk_expr(a, index);
                walk_expr(b, index);
            } else if let ast::AssignTarget::Field(a, _) = target {
                walk_expr(a, index);
            }
            walk_expr(value, index);
        }
        Stmt::MultiAssign { targets, values, .. } => {
            for target in targets {
                match target {
                    ast::AssignTarget::Index(a, b) => {
                        walk_expr(a, index);
                        walk_expr(b, index);
                    }
                    ast::AssignTarget::Field(a, _) => walk_expr(a, index),
                    ast::AssignTarget::Name(_) => {}
                }
            }
            for v in values {
                walk_expr(v, index);
            }
        }
        Stmt::Block(b) => walk_block(b, enclosing, index),
        Stmt::Repeat { body, cond, .. } => {
            walk_block(body, enclosing, index);
            walk_expr(cond, index);
        }
        Stmt::If { cond, then_block, else_block, .. } => {
            walk_expr(cond, index);
            walk_block(then_block, enclosing, index);
            if let Some(e) = else_block {
                walk_block(e, enclosing, index);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, index);
            walk_block(body, enclosing, index);
        }
        Stmt::NumericFor { var, start, stop, step, body, line } => {
            walk_expr(start, index);
            walk_expr(stop, index);
            if let Some(step) = step {
                walk_expr(step, index);
            }
            index.locals.push(LocalSymbol {
                name: var.clone(),
                ty: Some("i64".to_string()),
                line: *line,
                enclosing_function: enclosing.to_string(),
            });
            walk_block(body, enclosing, index);
        }
        Stmt::GenericFor { vars, iterators, body, line } => {
            for it in iterators {
                walk_expr(it, index);
            }
            for v in vars {
                index.locals.push(LocalSymbol {
                    name: v.clone(),
                    ty: None,
                    line: *line,
                    enclosing_function: enclosing.to_string(),
                });
            }
            walk_block(body, enclosing, index);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, index);
            }
        }
        Stmt::MultiReturn { values, .. } => {
            for v in values {
                walk_expr(v, index);
            }
        }
        Stmt::Expr(e) => walk_expr(e, index),
        Stmt::Global { values, .. } => {
            for v in values {
                walk_expr(v, index);
            }
        }
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
    }
}

fn walk_expr(expr: &Expr, index: &mut SymbolIndex) {
    match &expr.kind {
        ExprKind::Function(f) => {
            index.functions.push(FunctionSymbol {
                name: f.name.clone(),
                params: render_params(&f.params),
                return_type: f.return_type.as_ref().map(render_type),
                line: f.line,
                start_byte: f.source_span.start,
                end_byte: f.source_span.end,
                is_extern: false,
            });
            for (name, ty) in &f.params {
                index.locals.push(LocalSymbol {
                    name: name.clone(),
                    ty: Some(render_type(ty)),
                    line: f.line,
                    enclosing_function: f.name.clone(),
                });
            }
            walk_block(&f.body, &f.name, index);
        }
        ExprKind::Table(fields) => {
            for field in fields {
                match field {
                    ast::TableField::Value(e) => walk_expr(e, index),
                    ast::TableField::Named(_, e) => walk_expr(e, index),
                    ast::TableField::Key(k, v) => {
                        walk_expr(k, index);
                        walk_expr(v, index);
                    }
                }
            }
        }
        ExprKind::Unary(_, e) | ExprKind::Len(e) | ExprKind::Field(e, _) => {
            walk_expr(e, index)
        }
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => {
            walk_expr(a, index);
            walk_expr(b, index);
        }
        ExprKind::Call(_, args) => {
            for a in args {
                walk_expr(a, index);
            }
        }
        ExprKind::CallExpr(callee, args) => {
            walk_expr(callee, index);
            for a in args {
                walk_expr(a, index);
            }
        }
        ExprKind::MethodCall(recv, _, args) => {
            walk_expr(recv, index);
            for a in args {
                walk_expr(a, index);
            }
        }
        ExprKind::StructLiteral(_, fields) => {
            for (_, e) in fields {
                walk_expr(e, index);
            }
        }
        ExprKind::TypeTest(e, _) | ExprKind::Cast(e, _) => walk_expr(e, index),
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => {}
    }
}

impl SymbolIndex {
    pub fn find_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.functions.iter().find(|f| f.name == name)
    }

    pub fn find_struct(&self, name: &str) -> Option<&StructSymbol> {
        self.structs.iter().find(|s| s.name == name)
    }

    /// Nearest preceding local declaration with `name` inside the top-level
    /// function `enclosing`, at or before `at_line` - see the module doc for
    /// why this is a heuristic rather than real scope resolution.
    pub fn find_local(&self, name: &str, enclosing: &str, at_line: u32) -> Option<&LocalSymbol> {
        self.locals
            .iter()
            .filter(|l| l.name == name && l.enclosing_function == enclosing && l.line <= at_line)
            .max_by_key(|l| l.line)
    }

    /// Which top-level function's line range contains `line`, if any -
    /// used to scope a reference to the right function before resolving a
    /// local inside it. Falls back to no enclosing function (empty string)
    /// for references outside any function body (shouldn't normally happen
    /// since top-level statements are synthesized into `main`).
    pub fn enclosing_function_at(&self, text: &str, line: u32) -> String {
        for f in &self.functions {
            if f.is_extern {
                continue;
            }
            let start_line = f.line;
            let end_line = crate::text::offset_to_position(text, f.end_byte).line + 1;
            if line >= start_line && line <= end_line {
                return f.name.clone();
            }
        }
        String::new()
    }
}
