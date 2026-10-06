//! Frame-scoped expressions for the live specialized debugger. Arguments
//! retain their original specialized layouts, including arrays/maps/records.
use crate::{
    ast,
    interp::live::Frame,
    types::{self, TFunction, TProgram, Type},
};
use std::collections::BTreeMap;

pub struct Evaluated {
    pub ty: Type,
    pub raw: u64,
    // Results may contain nested references to temporary literal storage.
    pub(crate) _owner: crate::tier0::Engine,
    _roots: Box<[u64; 1]>,
    _guard: crate::gc::RootGuard,
}

fn annotation(ty: &Type) -> ast::TypeName {
    match ty {
        Type::I64 => ast::TypeName::I64,
        Type::F64 => ast::TypeName::F64,
        Type::Bool => ast::TypeName::Bool,
        Type::Nil => ast::TypeName::Nil,
        Type::String => ast::TypeName::String,
        Type::Any => ast::TypeName::Any,
        Type::Struct(name) => ast::TypeName::Struct(name.clone()),
        Type::Array(element) => ast::TypeName::Array(Box::new(annotation(element))),
        Type::Map(key, value) => {
            ast::TypeName::Map(Box::new(annotation(key)), Box::new(annotation(value)))
        }
        Type::Function {
            params,
            return_type,
        } => ast::TypeName::Function {
            params: params.iter().map(annotation).collect(),
            return_type: Box::new(annotation(return_type)),
        },
    }
}

pub fn evaluate(
    program: &TProgram,
    function: &TFunction,
    frame: &Frame,
    expression: &str,
    expected: Option<&Type>,
) -> Result<Evaluated, String> {
    let tokens = crate::lexer::lex_bytes(
        format!("function __sol_eval(): any\nreturn {expression}\nend").as_bytes(),
    )?;
    let mut source = crate::parser::parse_with_known_structs(
        tokens,
        crate::parser::LanguageConfig::SOL,
        program.structs.keys().cloned().collect(),
    )?;
    // Do not let an expression close the synthesized function and inject
    // additional declarations/statements.
    if source.functions.len() != 1
        || !source.structs.is_empty()
        || !source.externs.is_empty()
        || !source.aliases.is_empty()
        || !source.imports.is_empty()
        || !source.exports.is_empty()
        || source.functions[0].body.len() != 1
    {
        return Err("expected one debugger expression".into());
    }
    let ast::Stmt::Return {
        value: Some(expr), ..
    } = &source.functions[0].body[0]
    else {
        return Err("expected one debugger expression".into());
    };
    fn validate(expr: &ast::Expr) -> Result<(), String> {
        use ast::ExprKind::*;
        match &expr.kind {
            CallExpr(..) | MethodCall(..) | Function(..) | Vararg => {
                return Err("debugger expressions cannot call specialized function values".into())
            }
            Unary(_, e) | Len(e) | Field(e, _) | TypeTest(e, _) | Cast(e, _) | Paren(e) => {
                validate(e)?
            }
            Binary(_, a, b) | Index(a, b) => {
                validate(a)?;
                validate(b)?;
            }
            Call(_, args) => {
                for arg in args {
                    validate(arg)?;
                }
            }
            StructLiteral(_, fields) => {
                for (_, value) in fields {
                    validate(value)?;
                }
            }
            Table(fields) => {
                for field in fields {
                    match field {
                        ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                            validate(value)?
                        }
                        ast::TableField::Key(key, value) => {
                            validate(key)?;
                            validate(value)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    validate(expr)?;
    let local_types = types::collect_local_types(function);
    let mut parameters = BTreeMap::new();
    for local in frame.visible_locals() {
        let ty = local_types
            .get(local.local_id)
            .ok_or("missing local type")?;
        if matches!(ty, Type::Function { .. } | Type::Any) {
            continue;
        }
        // Highest LocalId wins for a shadowed source name. Numeric aliases
        // remain accepted for clients of the historical adapter.
        let entry = parameters
            .entry(local.name.clone())
            .or_insert((0, ty.clone()));
        if entry.0 <= local.local_id {
            *entry = (local.local_id, ty.clone());
        }
    }
    for local in frame.visible_locals() {
        let ty = &local_types[local.local_id];
        if !matches!(ty, Type::Function { .. } | Type::Any) {
            parameters
                .entry(format!("local{}", local.local_id))
                .or_insert((local.local_id, ty.clone()));
        }
    }
    source.functions[0].params = parameters
        .iter()
        .map(|(name, (_, ty))| (name.clone(), annotation(ty)))
        .collect();
    source.functions[0].param_annotations = vec![true; parameters.len()];
    source.structs = program
        .structs
        .iter()
        .map(|(name, layout)| ast::StructDef {
            name: name.clone(),
            fields: layout
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), annotation(ty)))
                .collect(),
            line: 1,
        })
        .collect();
    let return_type = if let Some(expected) = expected {
        expected.clone()
    } else {
        let probe = crate::typeck::check(&source, false)?;
        super::eval_find_return_type(
            &probe
                .functions
                .iter()
                .find(|f| f.name == "__sol_eval")
                .unwrap()
                .body,
        )
        .ok_or("expression has no result")?
    };
    if matches!(return_type, Type::Function { .. } | Type::Any) {
        return Err("specialized function/any expression results are not supported".into());
    }
    source.functions[0].return_type = Some(annotation(&return_type));
    let compiled = crate::typeck::check(&source, false)?;
    crate::verify::verify(&compiled)?;
    let engine: crate::tier0::Engine =
        crate::tier0::Engine::new_with_budget(compiled, (), 100_000)?;
    // Generic arithmetic still has native trap helpers. Do not admit those
    // into an expression evaluation until they have recoverable adapters.
    // Walk real instructions, skipping inline operand words (which are not
    // opcodes and must never be decoded as such).
    let bytecode = engine
        .function_bytecode("__sol_eval")
        .ok_or("missing expression bytecode")?;
    let mut pc = 0;
    while pc < bytecode.code.len() {
        use crate::bytecode::Op;
        let op = bytecode.code[pc].op();
        if matches!(
            op,
            Op::DynamicBinary
                | Op::DynamicCompare
                | Op::DynamicNeg
                | Op::CallIndirect
                | Op::ArrayMapI64
        ) {
            return Err("generic arithmetic and specialized callbacks are not supported in debugger expressions".into());
        }
        pc += match op {
            Op::StructAlloc | Op::Box => 3,
            Op::Unbox => 2,
            _ => 1,
        };
    }
    let args = parameters
        .values()
        .map(|(id, _)| frame.registers[*id])
        .collect::<Vec<_>>();
    let mut execution = engine.start_live("__sol_eval", &args)?;
    match execution.resume(100_001) {
        crate::interp::live::Stop::Returned(raw) => {
            let roots = Box::new([raw]);
            let guard = crate::gc::RootGuard::new(roots.as_ptr(), roots.len());
            Ok(Evaluated {
                ty: return_type,
                raw,
                _owner: engine,
                _roots: roots,
                _guard: guard,
            })
        }
        crate::interp::live::Stop::Raised(error) => Err(error),
        _ => Err("debugger expression budget exceeded".into()),
    }
}
