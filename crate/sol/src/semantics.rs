/// Chooses the specialized tier from semantic type surface, never from the
/// filename. Annotation-free Lua and Sol programs therefore enter the same
/// generic runtime; explicit contracts and typed-only declarations keep the
/// existing specialized/AOT path.
pub fn requires_specialized_execution(program: &crate::ast::Program) -> bool {
    !program.imports.is_empty()
        || !program.exports.is_empty()
        || !program.aliases.is_empty()
        || !program.structs.is_empty()
        || !program.externs.is_empty()
        || program.functions.iter().any(function_requires_types)
}

pub(crate) fn function_requires_types(function: &crate::ast::Function) -> bool {
    function.return_type.is_some()
        || function
            .params
            .iter()
            .any(|(_, ty)| !matches!(ty, crate::ast::TypeName::Any))
        || function.body.iter().any(statement_requires_types)
}

fn target_requires_types(target: &crate::ast::AssignTarget) -> bool {
    match target {
        crate::ast::AssignTarget::Name(_) => false,
        crate::ast::AssignTarget::Index(base, index) => {
            expression_requires_types(base) || expression_requires_types(index)
        }
        crate::ast::AssignTarget::Field(base, _) => expression_requires_types(base),
    }
}

fn statement_requires_types(statement: &crate::ast::Stmt) -> bool {
    use crate::ast::Stmt;
    match statement {
        Stmt::Global { values, .. } => values.iter().any(expression_requires_types),
        Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
            function_requires_types(function)
        }
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => false,
        Stmt::MultiLocal { names, values, .. } => {
            names.iter().any(|(_, ty, _, _)| ty.is_some())
                || values.iter().any(expression_requires_types)
        }
        Stmt::MultiAssign {
            targets, values, ..
        } => {
            targets.iter().any(target_requires_types)
                || values.iter().any(expression_requires_types)
        }
        Stmt::Block(body) => body.iter().any(statement_requires_types),
        Stmt::Repeat { body, cond, .. } => {
            body.iter().any(statement_requires_types) || expression_requires_types(cond)
        }
        Stmt::Expr(expression) => expression_requires_types(expression),
        Stmt::Local { ty, value, .. } => ty.is_some() || expression_requires_types(value),
        Stmt::Assign { target, value, .. } => {
            target_requires_types(target) || expression_requires_types(value)
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expression_requires_types(cond)
                || then_block.iter().any(statement_requires_types)
                || else_block
                    .as_ref()
                    .is_some_and(|body| body.iter().any(statement_requires_types))
        }
        Stmt::While { cond, body, .. } => {
            expression_requires_types(cond) || body.iter().any(statement_requires_types)
        }
        Stmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            expression_requires_types(start)
                || expression_requires_types(stop)
                || step.as_ref().is_some_and(expression_requires_types)
                || body.iter().any(statement_requires_types)
        }
        Stmt::GenericFor {
            iterators, body, ..
        } => {
            iterators.iter().any(expression_requires_types)
                || body.iter().any(statement_requires_types)
        }
        Stmt::Return { value, .. } => value.as_ref().is_some_and(expression_requires_types),
        Stmt::MultiReturn { values, .. } => values.iter().any(expression_requires_types),
    }
}

fn expression_requires_types(expression: &crate::ast::Expr) -> bool {
    use crate::ast::{ExprKind, TableField};
    match &expression.kind {
        ExprKind::StructLiteral(..) | ExprKind::TypeTest(..) | ExprKind::Cast(..) => true,
        ExprKind::Table(fields) => fields.iter().any(|field| match field {
            TableField::Value(value) | TableField::Named(_, value) => {
                expression_requires_types(value)
            }
            TableField::Key(key, value) => {
                expression_requires_types(key) || expression_requires_types(value)
            }
        }),
        ExprKind::Function(function) => function_requires_types(function),
        ExprKind::Unary(_, value)
        | ExprKind::Len(value)
        | ExprKind::Field(value, _)
        | ExprKind::Paren(value) => expression_requires_types(value),
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            expression_requires_types(left) || expression_requires_types(right)
        }
        ExprKind::Call(_, args) => args.iter().any(expression_requires_types),
        ExprKind::CallExpr(callee, args) | ExprKind::MethodCall(callee, _, args) => {
            expression_requires_types(callee) || args.iter().any(expression_requires_types)
        }
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected(source: &str, config: crate::parser::LanguageConfig) -> bool {
        let program = crate::parser::parse_with_config(
            crate::lexer::lex_bytes(source.as_bytes()).unwrap(),
            config,
        )
        .unwrap();
        requires_specialized_execution(&program)
    }
    #[test]
    fn identical_generic_source_has_identical_execution_selection_in_both_profiles() {
        for source in [
            "print(5/2)",
            "local t={answer=42}; print(t.answer)",
            "local function f(x) return x+1 end; print(f(41))",
        ] {
            assert!(!selected(source, crate::parser::LanguageConfig::LUA));
            assert!(!selected(source, crate::parser::LanguageConfig::SOL));
        }
        assert!(!selected(
            "fn main() print(42) end",
            crate::parser::LanguageConfig::SOL
        ));
    }
    #[test]
    fn explicit_typed_surfaces_and_nested_contracts_require_specialization() {
        for source in [
            "function main(): i64 return 42 end",
            "local x:i64=42",
            "local function f(x:i64) return x end",
            "local x=42 as i64",
            "import math.base",
        ] {
            assert!(
                selected(source, crate::parser::LanguageConfig::SOL),
                "{source}"
            );
        }
    }
}
