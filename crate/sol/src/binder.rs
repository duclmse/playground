//! Lexical name binding shared by the compiler and language tooling.
//!
//! Binding is deliberately independent of type checking and execution tier.
//! It models Lua block scopes, nested functions/upvalues, labels/gotos, and
//! global access through the nearest lexical `_ENV` binding.

use std::collections::HashMap;

use crate::ast::{
    AssignTarget, Block, Expr, ExprKind, Function, GlobalName, Program, Stmt, TableField,
};

pub type ScopeId = usize;
pub type FunctionId = usize;
pub type BindingId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    Parameter,
    Local,
    LocalFunction,
    LoopVariable,
    Global,
    Label,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    Read,
    Write,
    Call,
    Goto,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub function: FunctionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: BindingId,
    pub name: String,
    pub kind: BindingKind,
    pub scope: ScopeId,
    pub function: FunctionId,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub kind: ReferenceKind,
    pub binding: Option<BindingId>,
    /// The `_ENV` binding mediating an unresolved/global name.
    pub environment: Option<BindingId>,
    pub scope: ScopeId,
    pub function: FunctionId,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upvalue {
    pub function: FunctionId,
    pub binding: BindingId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingError {
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingIndex {
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    pub references: Vec<Reference>,
    pub upvalues: Vec<Upvalue>,
    pub errors: Vec<BindingError>,
}

pub fn bind(program: &Program) -> BindingIndex {
    let mut binder = Binder::default();
    for function in &program.functions {
        binder.bind_root_function(function);
    }
    binder.index
}

#[derive(Default)]
struct Binder {
    index: BindingIndex,
    names: Vec<HashMap<String, BindingId>>,
    labels: Vec<HashMap<String, BindingId>>,
    next_function: FunctionId,
}

impl Binder {
    fn new_scope(&mut self, parent: Option<ScopeId>, function: FunctionId) -> ScopeId {
        let id = self.index.scopes.len();
        self.index.scopes.push(Scope {
            id,
            parent,
            function,
        });
        self.names.push(HashMap::new());
        self.labels.push(HashMap::new());
        id
    }

    fn bind_root_function(&mut self, function: &Function) {
        let function_id = self.alloc_function();
        let scope = self.new_scope(None, function_id);
        self.declare(scope, "_ENV", BindingKind::Environment, function.line);
        for (name, _) in &function.params {
            self.declare(scope, name, BindingKind::Parameter, function.line);
        }
        if let Some(name) = &function.vararg_name {
            self.declare(scope, name, BindingKind::Parameter, function.line);
        }
        self.bind_block_in_scope(&function.body, scope);
    }

    fn alloc_function(&mut self) -> FunctionId {
        let id = self.next_function;
        self.next_function += 1;
        id
    }

    fn declare(&mut self, scope: ScopeId, name: &str, kind: BindingKind, line: u32) -> BindingId {
        let id = self.index.bindings.len();
        self.index.bindings.push(Binding {
            id,
            name: name.to_owned(),
            kind,
            scope,
            function: self.index.scopes[scope].function,
            line,
        });
        self.names[scope].insert(name.to_owned(), id);
        id
    }

    fn declare_label(&mut self, scope: ScopeId, name: &str, line: u32) {
        if self.labels[scope].contains_key(name) {
            self.index.errors.push(BindingError {
                line,
                message: format!("duplicate label '{name}' in the same block"),
            });
            return;
        }
        let id = self.index.bindings.len();
        self.index.bindings.push(Binding {
            id,
            name: name.to_owned(),
            kind: BindingKind::Label,
            scope,
            function: self.index.scopes[scope].function,
            line,
        });
        self.labels[scope].insert(name.to_owned(), id);
    }

    fn bind_block(&mut self, block: &Block, parent: ScopeId) {
        let function = self.index.scopes[parent].function;
        let scope = self.new_scope(Some(parent), function);
        self.bind_block_in_scope(block, scope);
    }

    fn bind_block_in_scope(&mut self, block: &Block, scope: ScopeId) {
        // Labels are visible throughout their containing block, including to
        // preceding gotos. Locals retain statement-order visibility.
        for stmt in block {
            if let Stmt::Label { name, line } = stmt {
                self.declare_label(scope, name, *line);
            }
        }
        for stmt in block {
            self.bind_stmt(stmt, scope);
        }
    }

    fn bind_stmt(&mut self, stmt: &Stmt, scope: ScopeId) {
        match stmt {
            Stmt::Global {
                names,
                values,
                line,
            } => {
                for value in values {
                    self.bind_expr(value, scope);
                }
                for name in names {
                    if let GlobalName::Name(name) = &name.name {
                        self.add_global_reference(name, ReferenceKind::Write, scope, *line);
                    }
                }
            }
            Stmt::GlobalFunction(function) => {
                self.add_global_reference(
                    &function.name,
                    ReferenceKind::Write,
                    scope,
                    function.line,
                );
                self.bind_nested_function(function, scope);
            }
            Stmt::LocalFunction(function) => {
                self.declare(
                    scope,
                    &function.name,
                    BindingKind::LocalFunction,
                    function.line,
                );
                self.bind_nested_function(function, scope);
            }
            Stmt::Label { .. } => {}
            Stmt::Goto { name, line } => self.bind_goto(name, scope, *line),
            Stmt::MultiLocal {
                names,
                values,
                line,
            } => {
                for value in values {
                    self.bind_expr(value, scope);
                }
                for (name, _, _, _) in names {
                    self.declare(scope, name, BindingKind::Local, *line);
                }
            }
            Stmt::MultiAssign {
                targets,
                values,
                line,
            } => {
                for target in targets {
                    self.bind_target(target, scope, *line);
                }
                for value in values {
                    self.bind_expr(value, scope);
                }
            }
            Stmt::Block(block) => self.bind_block(block, scope),
            Stmt::Repeat { body, cond, .. } => {
                let repeat_scope = self.new_scope(Some(scope), self.index.scopes[scope].function);
                self.bind_block_in_scope(body, repeat_scope);
                self.bind_expr(cond, repeat_scope);
            }
            Stmt::Break { .. } => {}
            Stmt::Expr(expr) => self.bind_expr(expr, scope),
            Stmt::Local {
                name, value, line, ..
            } => {
                self.bind_expr(value, scope);
                self.declare(scope, name, BindingKind::Local, *line);
            }
            Stmt::Assign {
                target,
                value,
                line,
            } => {
                self.bind_target(target, scope, *line);
                self.bind_expr(value, scope);
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.bind_expr(cond, scope);
                self.bind_block(then_block, scope);
                if let Some(block) = else_block {
                    self.bind_block(block, scope);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.bind_expr(cond, scope);
                self.bind_block(body, scope);
            }
            Stmt::NumericFor {
                var,
                start,
                stop,
                step,
                body,
                line,
            } => {
                self.bind_expr(start, scope);
                self.bind_expr(stop, scope);
                if let Some(step) = step {
                    self.bind_expr(step, scope);
                }
                let loop_scope = self.new_scope(Some(scope), self.index.scopes[scope].function);
                self.declare(loop_scope, var, BindingKind::LoopVariable, *line);
                self.bind_block_in_scope(body, loop_scope);
            }
            Stmt::GenericFor {
                vars,
                iterators,
                body,
                line,
            } => {
                for iterator in iterators {
                    self.bind_expr(iterator, scope);
                }
                let loop_scope = self.new_scope(Some(scope), self.index.scopes[scope].function);
                for var in vars {
                    self.declare(loop_scope, var, BindingKind::LoopVariable, *line);
                }
                self.bind_block_in_scope(body, loop_scope);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.bind_expr(value, scope);
                }
            }
            Stmt::MultiReturn { values, .. } => {
                for value in values {
                    self.bind_expr(value, scope);
                }
            }
        }
    }

    fn bind_nested_function(&mut self, function: &Function, parent: ScopeId) {
        let function_id = self.alloc_function();
        let scope = self.new_scope(Some(parent), function_id);
        for (name, _) in &function.params {
            self.declare(scope, name, BindingKind::Parameter, function.line);
        }
        if let Some(name) = &function.vararg_name {
            self.declare(scope, name, BindingKind::Parameter, function.line);
        }
        self.bind_block_in_scope(&function.body, scope);
    }

    fn bind_target(&mut self, target: &AssignTarget, scope: ScopeId, line: u32) {
        match target {
            AssignTarget::Name(name) => {
                self.add_name_reference(name, ReferenceKind::Write, scope, line)
            }
            AssignTarget::Index(base, index) => {
                self.bind_expr(base, scope);
                self.bind_expr(index, scope);
            }
            AssignTarget::Field(base, _) => self.bind_expr(base, scope),
        }
    }

    fn bind_expr(&mut self, expr: &Expr, scope: ScopeId) {
        match &expr.kind {
            ExprKind::Table(fields) => {
                for field in fields {
                    match field {
                        TableField::Value(value) | TableField::Named(_, value) => {
                            self.bind_expr(value, scope)
                        }
                        TableField::Key(key, value) => {
                            self.bind_expr(key, scope);
                            self.bind_expr(value, scope);
                        }
                    }
                }
            }
            ExprKind::Function(function) => self.bind_nested_function(function, scope),
            ExprKind::Vararg
            | ExprKind::StringLit(_)
            | ExprKind::NilLit
            | ExprKind::IntLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::BoolLit(_) => {}
            ExprKind::Name(name) => {
                self.add_name_reference(name, ReferenceKind::Read, scope, expr.line)
            }
            ExprKind::Unary(_, value)
            | ExprKind::Len(value)
            | ExprKind::TypeTest(value, _)
            | ExprKind::Cast(value, _)
            | ExprKind::Paren(value) => {
                self.bind_expr(value, scope);
            }
            ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
                self.bind_expr(left, scope);
                self.bind_expr(right, scope);
            }
            ExprKind::Call(name, args) => {
                self.add_name_reference(name, ReferenceKind::Call, scope, expr.line);
                for arg in args {
                    self.bind_expr(arg, scope);
                }
            }
            ExprKind::CallExpr(callee, args) => {
                self.bind_expr(callee, scope);
                for arg in args {
                    self.bind_expr(arg, scope);
                }
            }
            ExprKind::MethodCall(receiver, _, args) => {
                self.bind_expr(receiver, scope);
                for arg in args {
                    self.bind_expr(arg, scope);
                }
            }
            ExprKind::StructLiteral(_, fields) => {
                for (_, value) in fields {
                    self.bind_expr(value, scope);
                }
            }
            ExprKind::Field(base, _) => self.bind_expr(base, scope),
        }
    }

    fn resolve_name(&self, name: &str, mut scope: ScopeId) -> Option<BindingId> {
        loop {
            if let Some(binding) = self.names[scope].get(name) {
                return Some(*binding);
            }
            scope = self.index.scopes[scope].parent?;
        }
    }

    fn add_name_reference(&mut self, name: &str, kind: ReferenceKind, scope: ScopeId, line: u32) {
        if let Some(binding) = self.resolve_name(name, scope) {
            self.capture_if_needed(scope, binding);
            self.index.references.push(Reference {
                name: name.to_owned(),
                kind,
                binding: Some(binding),
                environment: None,
                scope,
                function: self.index.scopes[scope].function,
                line,
            });
        } else {
            self.add_global_reference(name, kind, scope, line);
        }
    }

    fn add_global_reference(&mut self, name: &str, kind: ReferenceKind, scope: ScopeId, line: u32) {
        let environment = self.resolve_name("_ENV", scope);
        if let Some(binding) = environment {
            self.capture_if_needed(scope, binding);
        }
        self.index.references.push(Reference {
            name: name.to_owned(),
            kind,
            binding: None,
            environment,
            scope,
            function: self.index.scopes[scope].function,
            line,
        });
    }

    fn capture_if_needed(&mut self, scope: ScopeId, binding: BindingId) {
        let defining_function = self.index.bindings[binding].function;
        let mut cursor = Some(scope);
        let mut previous_function = None;
        while let Some(scope) = cursor {
            let function = self.index.scopes[scope].function;
            if function == defining_function {
                break;
            }
            if previous_function != Some(function)
                && !self
                    .index
                    .upvalues
                    .iter()
                    .any(|upvalue| upvalue.function == function && upvalue.binding == binding)
            {
                self.index.upvalues.push(Upvalue {
                    function,
                    binding,
                    name: self.index.bindings[binding].name.clone(),
                });
            }
            previous_function = Some(function);
            cursor = self.index.scopes[scope].parent;
        }
    }

    fn bind_goto(&mut self, name: &str, scope: ScopeId, line: u32) {
        let function = self.index.scopes[scope].function;
        let mut current = Some(scope);
        let mut binding = None;
        while let Some(candidate) = current {
            if self.index.scopes[candidate].function != function {
                break;
            }
            if let Some(found) = self.labels[candidate].get(name) {
                binding = Some(*found);
                break;
            }
            current = self.index.scopes[candidate].parent;
        }
        if binding.is_none() {
            self.index.errors.push(BindingError {
                line,
                message: format!("no visible label '{name}' for goto"),
            });
        }
        self.index.references.push(Reference {
            name: name.to_owned(),
            kind: ReferenceKind::Goto,
            binding,
            environment: None,
            scope,
            function,
            line,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind_source(source: &str) -> BindingIndex {
        let tokens = crate::lexer::lex(source).unwrap();
        let program = crate::parser::parse_lua(tokens).unwrap();
        bind(&program)
    }

    #[test]
    fn resolves_shadowing_upvalues_and_environment_access() {
        let index = bind_source(
            "local outer = 1\nlocal function nested(arg)\n local outer = arg\n return function() return outer + external end\nend\n",
        );
        assert!(index.errors.is_empty(), "{:?}", index.errors);
        let captured_outer = index.upvalues.iter().any(|upvalue| upvalue.name == "outer");
        let captured_env = index.upvalues.iter().any(|upvalue| upvalue.name == "_ENV");
        assert!(captured_outer && captured_env);
        let external = index
            .references
            .iter()
            .find(|reference| reference.name == "external")
            .unwrap();
        assert!(external.binding.is_none());
        assert!(external.environment.is_some());
    }

    #[test]
    fn labels_are_block_scoped_and_forward_visible() {
        let valid = bind_source("goto done\n::done::\nreturn 1\n");
        assert!(valid.errors.is_empty());
        let invalid = bind_source("do ::inside:: end\ngoto inside\n");
        assert_eq!(invalid.errors.len(), 1);
        assert!(invalid.errors[0].message.contains("no visible label"));
    }

    #[test]
    fn local_initializers_resolve_before_the_new_binding() {
        let index = bind_source("local value = value\nreturn value\n");
        let references: Vec<_> = index
            .references
            .iter()
            .filter(|reference| reference.name == "value")
            .collect();
        assert_eq!(references.len(), 2);
        assert!(references[0].binding.is_none());
        assert!(references[1].binding.is_some());
    }
}
