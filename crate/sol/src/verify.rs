//! Typed-IR validation between compiler phases.
//!
//! Type checking constructs this IR, but optimizers also rewrite it. Keeping
//! the checks here prevents a broken transformation from reaching bytecode or
//! Cranelift as a register-number panic or an invalid native signature.

use std::collections::HashMap;

use crate::types::*;

pub fn verify(program: &TProgram) -> Result<(), String> {
    let mut signatures: HashMap<&str, (&[Type], &Type)> = HashMap::new();
    for external in &program.externs {
        signatures.insert(&external.name, (&external.params, &external.return_type));
    }
    for function in &program.functions {
        verify_function(function, program, &signatures)?;
    }
    Ok(())
}

fn verify_function(
    function: &TFunction,
    program: &TProgram,
    externs: &HashMap<&str, (&[Type], &Type)>,
) -> Result<(), String> {
    for (id, _) in &function.params {
        if *id >= function.local_count {
            return Err(format!(
                "IR verification: function '{}' parameter local {id} is outside local_count {}",
                function.name, function.local_count
            ));
        }
    }
    verify_block(&function.body, function, program, externs)
}

fn verify_block(
    block: &[TStmt],
    function: &TFunction,
    program: &TProgram,
    externs: &HashMap<&str, (&[Type], &Type)>,
) -> Result<(), String> {
    for statement in block {
        match statement {
            TStmt::Break => {}
            TStmt::Local { id, value } | TStmt::Assign { id, value } => {
                verify_local(*id, function)?;
                verify_expr(value, function, program, externs)?;
            }
            TStmt::AssignIndex {
                array,
                index,
                value,
            } => {
                verify_expr(array, function, program, externs)?;
                verify_expr(index, function, program, externs)?;
                verify_expr(value, function, program, externs)?;
            }
            TStmt::AssignField { base, value, .. } => {
                verify_expr(base, function, program, externs)?;
                verify_expr(value, function, program, externs)?;
            }
            TStmt::If {
                cond,
                then_block,
                else_block,
            } => {
                verify_expr(cond, function, program, externs)?;
                if cond.ty != Type::Bool {
                    return Err(format!(
                        "IR verification: function '{}' has non-boolean if condition",
                        function.name
                    ));
                }
                verify_block(then_block, function, program, externs)?;
                verify_block(else_block, function, program, externs)?;
            }
            TStmt::While { cond, body } => {
                verify_expr(cond, function, program, externs)?;
                if cond.ty != Type::Bool {
                    return Err(format!(
                        "IR verification: function '{}' has non-boolean while condition",
                        function.name
                    ));
                }
                verify_block(body, function, program, externs)?;
            }
            TStmt::NumericFor {
                id,
                stop_id,
                step_id,
                start,
                stop,
                step,
                body,
            } => {
                for id in [id, stop_id, step_id] {
                    verify_local(*id, function)?;
                }
                for value in [start, stop, step] {
                    verify_expr(value, function, program, externs)?;
                    if value.ty != Type::I64 {
                        return Err(format!(
                            "IR verification: function '{}' has non-i64 numeric-for bound",
                            function.name
                        ));
                    }
                }
                verify_block(body, function, program, externs)?;
            }
            TStmt::Return { value } => {
                if let Some(value) = value {
                    verify_expr(value, function, program, externs)?;
                    if value.ty != function.return_type {
                        return Err(format!(
                            "IR verification: function '{}' returns {}, expected {}",
                            function.name, value.ty, function.return_type
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn verify_expr(
    expr: &TExpr,
    function: &TFunction,
    program: &TProgram,
    externs: &HashMap<&str, (&[Type], &Type)>,
) -> Result<(), String> {
    match &expr.kind {
        TExprKind::Local(id) => verify_local(*id, function),
        TExprKind::FunctionRef(name) => {
            let signature = signature(name, program, externs).ok_or_else(|| {
                format!(
                    "IR verification: function '{}' references unknown function '{name}'",
                    function.name
                )
            })?;
            let expected = Type::Function {
                params: signature.0,
                return_type: Box::new(signature.1),
            };
            same_type(&expr.ty, &expected, function, "function reference")
        }
        TExprKind::Call(name, args) => {
            let (params, result) = signature(name, program, externs).ok_or_else(|| {
                format!(
                    "IR verification: function '{}' calls unknown function '{name}'",
                    function.name
                )
            })?;
            verify_call(args, &params, &result, expr, function, program, externs)
        }
        TExprKind::CallIndirect { callee, args } => {
            verify_expr(callee, function, program, externs)?;
            let Type::Function {
                params,
                return_type,
            } = &callee.ty
            else {
                return Err(format!(
                    "IR verification: function '{}' indirectly calls a non-function",
                    function.name
                ));
            };
            verify_call(args, params, return_type, expr, function, program, externs)
        }
        TExprKind::Truth(inner)
        | TExprKind::Neg(inner)
        | TExprKind::Not(inner)
        | TExprKind::IntToFloat(inner)
        | TExprKind::Box(inner)
        | TExprKind::Unbox(inner, _)
        | TExprKind::Field { base: inner, .. } => verify_expr(inner, function, program, externs),
        TExprKind::Len(inner) => {
            verify_expr(inner, function, program, externs)?;
            if !matches!(inner.ty, Type::Array(_) | Type::Map(_, _) | Type::String)
                || expr.ty != Type::I64
            {
                return Err(format!(
                    "IR verification: function '{}' has invalid length operation",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::Arith(_, left, right)
        | TExprKind::Compare(_, left, right)
        | TExprKind::Logical(_, left, right) => {
            verify_expr(left, function, program, externs)?;
            verify_expr(right, function, program, externs)
        }
        TExprKind::Index(collection, index) => {
            verify_expr(collection, function, program, externs)?;
            verify_expr(index, function, program, externs)?;
            let (expected_index, expected_value) = match &collection.ty {
                Type::Array(value) => (&Type::I64, value.as_ref()),
                Type::Map(key, value) => (key.as_ref(), value.as_ref()),
                _ => {
                    return Err(format!(
                        "IR verification: function '{}' indexes a non-collection",
                        function.name
                    ));
                }
            };
            if &index.ty != expected_index || &expr.ty != expected_value {
                return Err(format!(
                    "IR verification: function '{}' has invalid collection index types",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::NewArray { elem, len } => {
            verify_expr(len, function, program, externs)?;
            if len.ty != Type::I64 || expr.ty != Type::Array(Box::new(elem.clone())) {
                return Err(format!(
                    "IR verification: function '{}' has invalid new-array types",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::ArrayLiteral { elem, values } => {
            for value in values {
                verify_expr(value, function, program, externs)?;
                if &value.ty != elem {
                    return Err(format!(
                        "{}: array literal element type {} does not match {elem}",
                        function.name, value.ty
                    ));
                }
            }
            if expr.ty != Type::Array(Box::new(elem.clone())) {
                return Err(format!(
                    "{}: array literal result type does not match Array<{elem}>",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::ArrayMap {
            array,
            callback,
            elem,
        } => {
            verify_expr(array, function, program, externs)?;
            verify_expr(callback, function, program, externs)?;
            if !matches!(elem, Type::I64 | Type::F64) {
                return Err(format!(
                    "IR verification: function '{}' has an array map specialized on unsupported element type {elem}",
                    function.name
                ));
            }
            let expected_array = Type::Array(Box::new(elem.clone()));
            let expected_callback = Type::Function {
                params: vec![elem.clone()],
                return_type: Box::new(elem.clone()),
            };
            if array.ty != expected_array
                || callback.ty != expected_callback
                || expr.ty != expected_array
            {
                return Err(format!(
                    "IR verification: function '{}' has invalid specialized array map types",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::NewMap { key, value } => match &expr.ty {
            Type::Map(actual_key, actual_value)
                if actual_key.as_ref() == key && actual_value.as_ref() == value =>
            {
                Ok(())
            }
            _ => Err(format!(
                "{}: new map type does not match expression type {}",
                function.name, expr.ty
            )),
        },
        TExprKind::MapLiteral {
            key,
            value,
            entries,
        } => {
            if expr.ty != Type::Map(Box::new(key.clone()), Box::new(value.clone())) {
                return Err(format!(
                    "{}: map literal result type does not match Map<{key}, {value}>",
                    function.name
                ));
            }
            for (entry_key, entry_value) in entries {
                verify_expr(entry_key, function, program, externs)?;
                verify_expr(entry_value, function, program, externs)?;
                if &entry_key.ty != key || &entry_value.ty != value {
                    return Err(format!(
                        "{}: map literal entry does not match Map<{key}, {value}>",
                        function.name
                    ));
                }
            }
            Ok(())
        }
        TExprKind::MapNext { map, cursor }
        | TExprKind::MapKey { map, cursor }
        | TExprKind::MapValue { map, cursor } => {
            verify_expr(map, function, program, externs)?;
            verify_expr(cursor, function, program, externs)?;
            if !matches!(map.ty, Type::Map(_, _)) || cursor.ty != Type::I64 {
                return Err(format!(
                    "{}: invalid internal map cursor operation",
                    function.name
                ));
            }
            let expected = match (&expr.kind, &map.ty) {
                (TExprKind::MapNext { .. }, _) => &Type::I64,
                (TExprKind::MapKey { .. }, Type::Map(key, _)) => key,
                (TExprKind::MapValue { .. }, Type::Map(_, value)) => value,
                _ => unreachable!(),
            };
            if &expr.ty != expected {
                return Err(format!(
                    "{}: invalid internal map cursor result type",
                    function.name
                ));
            }
            Ok(())
        }
        TExprKind::StructLiteral { fields, .. } => {
            for field in fields {
                verify_expr(field, function, program, externs)?;
            }
            Ok(())
        }
        TExprKind::StringLit(_)
        | TExprKind::NilLit
        | TExprKind::IntLit(_)
        | TExprKind::FloatLit(_)
        | TExprKind::BoolLit(_) => Ok(()),
    }
}

fn signature(
    name: &str,
    program: &TProgram,
    externs: &HashMap<&str, (&[Type], &Type)>,
) -> Option<(Vec<Type>, Type)> {
    program
        .functions
        .iter()
        .find(|function| function.name == name)
        .map(|function| {
            (
                function.params.iter().map(|(_, ty)| ty.clone()).collect(),
                function.return_type.clone(),
            )
        })
        .or_else(|| {
            externs
                .get(name)
                .map(|(params, result)| (params.to_vec(), (*result).clone()))
        })
}

fn verify_call(
    args: &[TExpr],
    params: &[Type],
    result: &Type,
    expr: &TExpr,
    function: &TFunction,
    program: &TProgram,
    externs: &HashMap<&str, (&[Type], &Type)>,
) -> Result<(), String> {
    if args.len() != params.len() {
        return Err(format!(
            "IR verification: function '{}' has a call with the wrong argument count",
            function.name
        ));
    }
    for (arg, expected) in args.iter().zip(params) {
        verify_expr(arg, function, program, externs)?;
        same_type(&arg.ty, expected, function, "call argument")?;
    }
    same_type(&expr.ty, result, function, "call result")
}

fn verify_local(id: LocalId, function: &TFunction) -> Result<(), String> {
    if id >= function.local_count {
        return Err(format!(
            "IR verification: function '{}' references local {id} outside local_count {}",
            function.name, function.local_count
        ));
    }
    Ok(())
}

fn same_type(
    actual: &Type,
    expected: &Type,
    function: &TFunction,
    context: &str,
) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "IR verification: function '{}' has {context} type {actual}, expected {expected}",
            function.name
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{TExprKind, TStmt, Type};
    use crate::{lexer, parser, typeck};

    #[test]
    fn rejects_a_malformed_typed_call_before_codegen() {
        let source =
            "function inc(x: i64): i64 return x end\nfunction main(): i64 return inc(1) end";
        let mut program =
            typeck::check(&parser::parse(lexer::lex(source).unwrap()).unwrap(), false).unwrap();
        let TStmt::Return { value: Some(call) } = &mut program.functions[1].body[0] else {
            panic!("expected a direct call return");
        };
        let TExprKind::Call(_, args) = &mut call.kind else {
            panic!("expected a direct call");
        };
        args[0].ty = Type::Bool;
        let error = super::verify(&program).unwrap_err();
        assert!(
            error.contains("call argument type bool, expected i64"),
            "{error}"
        );
    }
}
