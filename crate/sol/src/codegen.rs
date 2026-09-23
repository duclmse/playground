// Typed AST -> Cranelift IR. `cranelift-frontend`'s `FunctionBuilder` does
// SSA construction for us (faster_lua.md §5). See jit.rs for how the
// resulting IR gets defined into a `JITModule` and run.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, BlockArg, InstBuilder, MemFlagsData, Signature, TrapCode, Type as ClifType,
    Value as ClifValue,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{FuncId, Linkage, Module};

use crate::ast::BinaryOp;
use crate::types::*;
use crate::value;

/// Cranelift's bool type: `I8` holding 0 or 1 (no dedicated `b1` type anymore).
const BOOL: ClifType = types::I8;

/// `any` unboxed as the wrong type - distinct from `inline_call`'s trap code 1.
const TRAP_ANY_TYPE_MISMATCH: TrapCode = TrapCode::unwrap_user(2);

pub fn clif_type(ty: &Type) -> ClifType {
    match ty {
        Type::String | Type::Nil | Type::I64 => types::I64,
        Type::F64 => types::F64,
        Type::Bool => BOOL,
        Type::Array(_) | Type::Map(_, _) => types::I64, // managed pointer
        Type::Struct(_) => types::I64,                  // a pointer, see runtime.rs's sol_alloc
        Type::Function { .. } => types::I64,            // a native code pointer
        Type::Any => types::I64, // a pointer to a boxed {tag, payload} pair, see value.rs
    }
}

pub fn signature_of(call_conv: CallConv, params: &[Type], ret: &Type) -> Signature {
    let mut sig = Signature::new(call_conv);
    for p in params {
        sig.params.push(AbiParam::new(clif_type(p)));
    }
    sig.returns.push(AbiParam::new(clif_type(ret)));
    sig
}

/// Host runtime functions every module needs - see runtime.rs.
pub struct RuntimeFuncs {
    pub new_array_i64: FuncId,
    pub new_array_f64: FuncId,
    pub new_array_ptr: FuncId,
    pub array_map_i64: FuncId,
    pub new_map_i64: FuncId,
    pub map_get_i64: FuncId,
    pub map_set_i64: FuncId,
    pub map_next_i64: FuncId,
    pub map_key_at_i64: FuncId,
    pub map_value_at_i64: FuncId,
    pub alloc: FuncId,
    pub alloc_layout: FuncId,
    pub pow: FuncId,
    pub mod_float: FuncId,
    pub dynamic_binary: FuncId,
    pub dynamic_compare: FuncId,
    pub dynamic_neg: FuncId,
    pub truth: FuncId,
    pub string_compare: FuncId,
    pub gc_write_barrier: FuncId,
}

pub fn declare_runtime(module: &mut dyn Module) -> Result<RuntimeFuncs, String> {
    let call_conv = module.target_config().default_call_conv;
    let array_sig = signature_of(call_conv, &[Type::I64], &Type::Array(Box::new(Type::I64)));
    let new_array_i64 = module
        .declare_function("sol_new_array_i64", Linkage::Import, &array_sig)
        .map_err(|e| e.to_string())?;
    let new_array_f64 = module
        .declare_function("sol_new_array_f64", Linkage::Import, &array_sig)
        .map_err(|e| e.to_string())?;
    let new_array_ptr = module
        .declare_function("sol_new_array_ptr", Linkage::Import, &array_sig)
        .map_err(|e| e.to_string())?;
    let array_i64 = Type::Array(Box::new(Type::I64));
    let callback_i64 = Type::Function {
        params: vec![Type::I64],
        return_type: Box::new(Type::I64),
    };
    let array_map_sig = signature_of(call_conv, &[array_i64.clone(), callback_i64], &array_i64);
    let array_map_i64 = module
        .declare_function("sol_array_map_i64", Linkage::Import, &array_map_sig)
        .map_err(|e| e.to_string())?;
    let map_ty = Type::Map(Box::new(Type::I64), Box::new(Type::I64));
    let map_new_sig = signature_of(call_conv, &[], &map_ty);
    let new_map_i64 = module
        .declare_function("sol_new_map_i64", Linkage::Import, &map_new_sig)
        .map_err(|e| e.to_string())?;
    let map_get_sig = signature_of(call_conv, &[map_ty.clone(), Type::I64], &Type::I64);
    let map_get_i64 = module
        .declare_function("sol_map_get_i64", Linkage::Import, &map_get_sig)
        .map_err(|e| e.to_string())?;
    let map_set_sig = signature_of(
        call_conv,
        &[map_ty.clone(), Type::I64, Type::I64],
        &Type::I64,
    );
    let map_set_i64 = module
        .declare_function("sol_map_set_i64", Linkage::Import, &map_set_sig)
        .map_err(|e| e.to_string())?;
    let map_cursor_sig = signature_of(call_conv, &[map_ty, Type::I64], &Type::I64);
    let map_next_i64 = module
        .declare_function("sol_map_next_i64", Linkage::Import, &map_cursor_sig)
        .map_err(|e| e.to_string())?;
    let map_key_at_i64 = module
        .declare_function("sol_map_key_at_i64", Linkage::Import, &map_cursor_sig)
        .map_err(|e| e.to_string())?;
    let map_value_at_i64 = module
        .declare_function("sol_map_value_at_i64", Linkage::Import, &map_cursor_sig)
        .map_err(|e| e.to_string())?;
    // `fn(i64) -> *mut u8` has no sol `Type` equivalent - built by hand.
    let mut alloc_sig = Signature::new(call_conv);
    alloc_sig.params.push(AbiParam::new(types::I64));
    alloc_sig.returns.push(AbiParam::new(types::I64));
    let alloc = module
        .declare_function("sol_alloc", Linkage::Import, &alloc_sig)
        .map_err(|e| e.to_string())?;
    let mut alloc_layout_sig = Signature::new(call_conv);
    alloc_layout_sig.params.push(AbiParam::new(types::I64));
    alloc_layout_sig.params.push(AbiParam::new(types::I64));
    alloc_layout_sig.returns.push(AbiParam::new(types::I64));
    let alloc_layout = module
        .declare_function("sol_alloc_layout", Linkage::Import, &alloc_layout_sig)
        .map_err(|e| e.to_string())?;
    let binary_float = signature_of(call_conv, &[Type::F64, Type::F64], &Type::F64);
    let pow = module
        .declare_function("sol_pow", Linkage::Import, &binary_float)
        .map_err(|e| e.to_string())?;
    let mod_float = module
        .declare_function("sol_mod_float", Linkage::Import, &binary_float)
        .map_err(|e| e.to_string())?;
    let ternary = signature_of(call_conv, &[Type::I64, Type::I64, Type::I64], &Type::I64);
    let unary = signature_of(call_conv, &[Type::I64], &Type::I64);
    let dynamic_binary = module
        .declare_function("sol_dynamic_binary", Linkage::Import, &ternary)
        .map_err(|e| e.to_string())?;
    let dynamic_compare = module
        .declare_function("sol_dynamic_compare", Linkage::Import, &ternary)
        .map_err(|e| e.to_string())?;
    let dynamic_neg = module
        .declare_function("sol_dynamic_neg", Linkage::Import, &unary)
        .map_err(|e| e.to_string())?;
    let truth = module
        .declare_function("sol_truth", Linkage::Import, &unary)
        .map_err(|e| e.to_string())?;
    let binary = signature_of(call_conv, &[Type::String, Type::String], &Type::I64);
    let string_compare = module
        .declare_function("sol_string_compare", Linkage::Import, &binary)
        .map_err(|e| e.to_string())?;
    // `fn(i64, i64)` (no return) has no sol `Type` equivalent - built by hand,
    // same as `alloc_sig` above.
    let mut write_barrier_sig = Signature::new(call_conv);
    write_barrier_sig.params.push(AbiParam::new(types::I64));
    write_barrier_sig.params.push(AbiParam::new(types::I64));
    let gc_write_barrier = module
        .declare_function("sol_gc_write_barrier", Linkage::Import, &write_barrier_sig)
        .map_err(|e| e.to_string())?;
    Ok(RuntimeFuncs {
        new_array_i64,
        new_array_f64,
        new_array_ptr,
        array_map_i64,
        new_map_i64,
        map_get_i64,
        map_set_i64,
        map_next_i64,
        map_key_at_i64,
        map_value_at_i64,
        alloc,
        alloc_layout,
        pow,
        mod_float,
        dynamic_binary,
        dynamic_compare,
        dynamic_neg,
        truth,
        string_compare,
        gc_write_barrier,
    })
}

/// Declares every function's signature up front so calls resolve regardless
/// of definition order (including recursive/forward calls).
pub fn declare_functions(
    module: &mut dyn Module,
    program: &TProgram,
) -> Result<HashMap<String, FuncId>, String> {
    let call_conv = module.target_config().default_call_conv;
    let mut ids = HashMap::new();
    for f in &program.functions {
        let param_types: Vec<Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
        let sig = signature_of(call_conv, &param_types, &f.return_type);
        let id = module
            .declare_function(&f.name, Linkage::Export, &sig)
            .map_err(|e| e.to_string())?;
        ids.insert(f.name.clone(), id);
    }
    for e in &program.externs {
        let sig = signature_of(call_conv, &e.params, &e.return_type);
        let id = module
            .declare_function(&e.name, Linkage::Import, &sig)
            .map_err(|e| e.to_string())?;
        ids.insert(e.name.clone(), id);
    }
    Ok(ids)
}

/// Every `LocalId`'s type, indexed by id - needed up front since
/// `declare_var` requires the type before any use is translated.
pub fn collect_local_types(f: &TFunction) -> Vec<Type> {
    let mut types = vec![Type::I64; f.local_count]; // placeholder, all overwritten below
    for (id, ty) in &f.params {
        types[*id] = ty.clone();
    }
    fn walk(stmts: &[TStmt], types: &mut [Type]) {
        for s in stmts {
            match s {
                TStmt::Local { id, value } => types[*id] = value.ty.clone(),
                TStmt::NumericFor {
                    id,
                    stop_id,
                    step_id,
                    body,
                    ..
                } => {
                    types[*id] = Type::I64;
                    types[*stop_id] = Type::I64;
                    types[*step_id] = Type::I64;
                    walk(body, types);
                }
                TStmt::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    walk(then_block, types);
                    walk(else_block, types);
                }
                TStmt::While { body, .. } => walk(body, types),
                TStmt::Break
                | TStmt::Assign { .. }
                | TStmt::AssignIndex { .. }
                | TStmt::AssignField { .. }
                | TStmt::Return { .. } => {}
            }
        }
    }
    walk(&f.body, &mut types);
    types
}

/// Inline a callee if its body has this many statements or fewer (§7).
const INLINE_MAX_STMTS: usize = 20;
/// Caps inlining depth - guards against a mutual-recursion cycle
/// `is_directly_recursive` doesn't catch (it only detects direct self-calls).
const MAX_INLINE_DEPTH: usize = 8;

/// Functions eligible for inlining: not `main`, small enough, not
/// directly self-recursive.
pub fn compute_inlinable(program: &TProgram) -> HashSet<String> {
    let address_taken: HashSet<String> = program
        .functions
        .iter()
        .flat_map(function_references)
        .collect();
    program
        .functions
        .iter()
        .filter(|f| {
            f.name != "main"
                && !is_directly_recursive(f)
                && !uses_function_value(&f.body)
                && !address_taken.contains(&f.name)
                && count_stmts(&f.body) <= INLINE_MAX_STMTS
        })
        .map(|f| f.name.clone())
        .collect()
}

/// Top-level functions whose addresses are materialized as `FunctionRef`.
/// They must be emitted even when all direct callers would otherwise inline.
fn function_references(function: &TFunction) -> HashSet<String> {
    fn visit_expr(expr: &TExpr, refs: &mut HashSet<String>) {
        match &expr.kind {
            TExprKind::FunctionRef(name) => {
                refs.insert(name.clone());
            }
            TExprKind::CallIndirect { callee, args } => {
                visit_expr(callee, refs);
                args.iter().for_each(|arg| visit_expr(arg, refs));
            }
            TExprKind::Call(_, args) => args.iter().for_each(|arg| visit_expr(arg, refs)),
            TExprKind::Truth(inner)
            | TExprKind::Neg(inner)
            | TExprKind::Not(inner)
            | TExprKind::IntToFloat(inner)
            | TExprKind::Len(inner)
            | TExprKind::Box(inner)
            | TExprKind::Unbox(inner, _)
            | TExprKind::Field { base: inner, .. } => visit_expr(inner, refs),
            TExprKind::Arith(_, left, right)
            | TExprKind::Compare(_, left, right)
            | TExprKind::Logical(_, left, right)
            | TExprKind::Index(left, right) => {
                visit_expr(left, refs);
                visit_expr(right, refs);
            }
            TExprKind::NewArray { len, .. } => visit_expr(len, refs),
            TExprKind::ArrayLiteral { values, .. } => {
                values.iter().for_each(|value| visit_expr(value, refs))
            }
            TExprKind::ArrayMap { array, callback } => {
                visit_expr(array, refs);
                visit_expr(callback, refs);
            }
            TExprKind::NewMap { .. } => {}
            TExprKind::MapLiteral { entries, .. } => {
                for (key, value) in entries {
                    visit_expr(key, refs);
                    visit_expr(value, refs);
                }
            }
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                visit_expr(map, refs);
                visit_expr(cursor, refs);
            }
            TExprKind::StructLiteral { fields, .. } => {
                fields.iter().for_each(|field| visit_expr(field, refs))
            }
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::Local(_) => {}
        }
    }
    fn visit_stmts(stmts: &[TStmt], refs: &mut HashSet<String>) {
        for stmt in stmts {
            match stmt {
                TStmt::Break => {}
                TStmt::Local { value, .. } | TStmt::Assign { value, .. } => visit_expr(value, refs),
                TStmt::AssignIndex {
                    array,
                    index,
                    value,
                } => {
                    visit_expr(array, refs);
                    visit_expr(index, refs);
                    visit_expr(value, refs);
                }
                TStmt::AssignField { base, value, .. } => {
                    visit_expr(base, refs);
                    visit_expr(value, refs);
                }
                TStmt::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    visit_expr(cond, refs);
                    visit_stmts(then_block, refs);
                    visit_stmts(else_block, refs);
                }
                TStmt::While { cond, body } => {
                    visit_expr(cond, refs);
                    visit_stmts(body, refs);
                }
                TStmt::NumericFor {
                    start,
                    stop,
                    step,
                    body,
                    ..
                } => {
                    visit_expr(start, refs);
                    visit_expr(stop, refs);
                    visit_expr(step, refs);
                    visit_stmts(body, refs);
                }
                TStmt::Return { value } => {
                    if let Some(value) = value {
                        visit_expr(value, refs);
                    }
                }
            }
        }
    }
    let mut refs = HashSet::new();
    visit_stmts(&function.body, &mut refs);
    refs
}

/// Inlining a body that takes or creates a function pointer would require its
/// transitive pointer targets to be added to the caller's dependency set.
/// Keep those bodies out of the small direct-call inliner until closure
/// conversion owns that dependency analysis in M11.
fn uses_function_value(stmts: &[TStmt]) -> bool {
    fn expr_uses_function_value(expr: &TExpr) -> bool {
        match &expr.kind {
            TExprKind::FunctionRef(_) | TExprKind::CallIndirect { .. } => true,
            TExprKind::Truth(inner)
            | TExprKind::Neg(inner)
            | TExprKind::Not(inner)
            | TExprKind::IntToFloat(inner)
            | TExprKind::Len(inner)
            | TExprKind::Box(inner)
            | TExprKind::Unbox(inner, _)
            | TExprKind::Field { base: inner, .. } => expr_uses_function_value(inner),
            TExprKind::Arith(_, left, right)
            | TExprKind::Compare(_, left, right)
            | TExprKind::Logical(_, left, right)
            | TExprKind::Index(left, right) => {
                expr_uses_function_value(left) || expr_uses_function_value(right)
            }
            TExprKind::Call(_, args) => args.iter().any(expr_uses_function_value),
            TExprKind::NewArray { len, .. } => expr_uses_function_value(len),
            TExprKind::ArrayLiteral { values, .. } => values.iter().any(expr_uses_function_value),
            TExprKind::ArrayMap { array, callback } => {
                expr_uses_function_value(array) || expr_uses_function_value(callback)
            }
            TExprKind::NewMap { .. } => false,
            TExprKind::MapLiteral { entries, .. } => entries.iter().any(|(key, value)| {
                expr_uses_function_value(key) || expr_uses_function_value(value)
            }),
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                expr_uses_function_value(map) || expr_uses_function_value(cursor)
            }
            TExprKind::StructLiteral { fields, .. } => fields.iter().any(expr_uses_function_value),
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::Local(_) => false,
        }
    }
    stmts.iter().any(|stmt| match stmt {
        TStmt::Break => false,
        TStmt::Local { value, .. } | TStmt::Assign { value, .. } => expr_uses_function_value(value),
        TStmt::AssignIndex {
            array,
            index,
            value,
        } => {
            expr_uses_function_value(array)
                || expr_uses_function_value(index)
                || expr_uses_function_value(value)
        }
        TStmt::AssignField { base, value, .. } => {
            expr_uses_function_value(base) || expr_uses_function_value(value)
        }
        TStmt::If {
            cond,
            then_block,
            else_block,
        } => {
            expr_uses_function_value(cond)
                || uses_function_value(then_block)
                || uses_function_value(else_block)
        }
        TStmt::While { cond, body } => expr_uses_function_value(cond) || uses_function_value(body),
        TStmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            expr_uses_function_value(start)
                || expr_uses_function_value(stop)
                || expr_uses_function_value(step)
                || uses_function_value(body)
        }
        TStmt::Return { value } => value.as_ref().is_some_and(expr_uses_function_value),
    })
}

fn count_stmts(stmts: &[TStmt]) -> usize {
    stmts
        .iter()
        .map(|s| {
            1 + match s {
                TStmt::If {
                    then_block,
                    else_block,
                    ..
                } => count_stmts(then_block) + count_stmts(else_block),
                TStmt::While { body, .. } | TStmt::NumericFor { body, .. } => count_stmts(body),
                TStmt::Local { .. }
                | TStmt::Break
                | TStmt::Assign { .. }
                | TStmt::AssignIndex { .. }
                | TStmt::AssignField { .. }
                | TStmt::Return { .. } => 0,
            }
        })
        .sum()
}

/// Detects only direct self-recursion, not mutual cycles (A calls B calls
/// A) - `MAX_INLINE_DEPTH` is the safety net for the rest.
fn is_directly_recursive(f: &TFunction) -> bool {
    fn expr_calls(e: &TExpr, name: &str) -> bool {
        match &e.kind {
            TExprKind::Call(n, args) => n == name || args.iter().any(|a| expr_calls(a, name)),
            TExprKind::CallIndirect { callee, args } => {
                expr_calls(callee, name) || args.iter().any(|a| expr_calls(a, name))
            }
            TExprKind::Truth(e)
            | TExprKind::Neg(e)
            | TExprKind::Not(e)
            | TExprKind::IntToFloat(e)
            | TExprKind::Len(e) => expr_calls(e, name),
            TExprKind::Arith(_, l, r)
            | TExprKind::Compare(_, l, r)
            | TExprKind::Logical(_, l, r)
            | TExprKind::Index(l, r) => expr_calls(l, name) || expr_calls(r, name),
            TExprKind::NewArray { len, .. } => expr_calls(len, name),
            TExprKind::ArrayLiteral { values, .. } => {
                values.iter().any(|value| expr_calls(value, name))
            }
            TExprKind::ArrayMap { array, callback } => {
                expr_calls(array, name) || expr_calls(callback, name)
            }
            TExprKind::NewMap { .. } => false,
            TExprKind::MapLiteral { entries, .. } => entries
                .iter()
                .any(|(key, value)| expr_calls(key, name) || expr_calls(value, name)),
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                expr_calls(map, name) || expr_calls(cursor, name)
            }
            TExprKind::StructLiteral { fields, .. } => fields.iter().any(|f| expr_calls(f, name)),
            TExprKind::Field { base, .. } => expr_calls(base, name),
            TExprKind::Box(inner) | TExprKind::Unbox(inner, _) => expr_calls(inner, name),
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::Local(_)
            | TExprKind::FunctionRef(_) => false,
        }
    }
    fn stmts_call(stmts: &[TStmt], name: &str) -> bool {
        stmts.iter().any(|s| match s {
            TStmt::Break => false,
            TStmt::Local { value, .. } | TStmt::Assign { value, .. } => expr_calls(value, name),
            TStmt::AssignIndex {
                array,
                index,
                value,
            } => expr_calls(array, name) || expr_calls(index, name) || expr_calls(value, name),
            TStmt::AssignField { base, value, .. } => {
                expr_calls(base, name) || expr_calls(value, name)
            }
            TStmt::If {
                cond,
                then_block,
                else_block,
            } => {
                expr_calls(cond, name)
                    || stmts_call(then_block, name)
                    || stmts_call(else_block, name)
            }
            TStmt::While { cond, body } => expr_calls(cond, name) || stmts_call(body, name),
            TStmt::NumericFor {
                start,
                stop,
                step,
                body,
                ..
            } => {
                expr_calls(start, name)
                    || expr_calls(stop, name)
                    || expr_calls(step, name)
                    || stmts_call(body, name)
            }
            TStmt::Return { value } => value.as_ref().is_some_and(|v| expr_calls(v, name)),
        })
    }
    stmts_call(&f.body, &f.name)
}

/// Program-wide codegen context, bundled to avoid a huge parameter list.
pub struct ProgramCtx<'a> {
    pub func_ids: &'a HashMap<String, FuncId>,
    pub functions: &'a HashMap<String, &'a TFunction>,
    pub inlinable: &'a HashSet<String>,
    pub runtime: &'a RuntimeFuncs,
}

pub fn compile_function(
    module: &mut dyn Module,
    builder_ctx: &mut FunctionBuilderContext,
    clif_func: &mut cranelift_codegen::ir::Function,
    tfunc: &TFunction,
    prog: &ProgramCtx,
) -> Result<(), String> {
    let frontend_config = module.target_config();
    clif_func.signature = signature_of(
        frontend_config.default_call_conv,
        &tfunc
            .params
            .iter()
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>(),
        &tfunc.return_type,
    );

    let mut builder = FunctionBuilder::new(clif_func, builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    // `vars[id]` is the `LocalId -> Variable` mapping used everywhere else.
    let local_types = collect_local_types(tfunc);
    let vars: Vec<Variable> = local_types
        .iter()
        .map(|ty| builder.declare_var(clif_type(ty)))
        .collect();
    for (i, (id, _)) in tfunc.params.iter().enumerate() {
        let val = builder.block_params(entry)[i];
        builder.def_var(vars[*id], val);
    }

    let mut fc = FuncCtx {
        module,
        builder: &mut builder,
        func_ids: prog.func_ids,
        functions: prog.functions,
        inlinable: prog.inlinable,
        runtime: prog.runtime,
        var_scopes: vec![vars],
        return_targets: Vec::new(),
        inline_depth: 0,
        bounds_checked_safe: Vec::new(),
        break_targets: vec![],
        resume_for: false,
    };
    let terminated = fc.translate_block(&tfunc.body);
    if !terminated {
        return Err(format!(
            "function '{}' does not return a value on every path",
            tfunc.name
        ));
    }

    builder.finalize(frontend_config);
    Ok(())
}

/// A uniform-ABI entry point (`extern "C" fn(*const u64, i64) -> u64`) so
/// `interp.rs` can call any native function through one fixed type, whatever
/// its real signature - unpacks `args[i]`, calls the real function, packs
/// the result back. `param_types`/`return_type` are taken explicitly (not
/// derived from `tfunc`) since OSR wraps a synthetic per-local signature,
/// not `tfunc`'s own params.
pub fn compile_wrapper(
    module: &mut dyn Module,
    builder_ctx: &mut FunctionBuilderContext,
    clif_func: &mut cranelift_codegen::ir::Function,
    param_types: &[Type],
    return_type: &Type,
    real_func_id: FuncId,
) -> Result<(), String> {
    let frontend_config = module.target_config();
    let call_conv = frontend_config.default_call_conv;
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(types::I64)); // args: *const u64
    sig.returns.push(AbiParam::new(types::I64)); // result, reinterpreted as i64 bits
    clif_func.signature = sig;

    let mut builder = FunctionBuilder::new(clif_func, builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let args_ptr = builder.block_params(entry)[0];

    let arg_vals: Vec<ClifValue> = param_types
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let raw = builder.ins().load(
                types::I64,
                MemFlagsData::trusted(),
                args_ptr,
                (i as i32) * 8,
            );
            match ty {
                Type::F64 => builder.ins().bitcast(types::F64, MemFlagsData::new(), raw),
                Type::Bool => builder.ins().ireduce(BOOL, raw),
                _ => raw, // I64/Array/Struct/Any - already a plain i64/pointer register value
            }
        })
        .collect();
    let func_ref = module.declare_func_in_func(real_func_id, builder.func);
    let call = builder.ins().call(func_ref, &arg_vals);
    let result = builder.inst_results(call)[0];
    let result_i64 = match return_type {
        Type::F64 => builder
            .ins()
            .bitcast(types::I64, MemFlagsData::new(), result),
        Type::Bool => builder.ins().uextend(types::I64, result),
        _ => result,
    };
    builder.ins().return_(&[result_i64]);
    builder.finalize(frontend_config);
    Ok(())
}

/// On-stack replacement: a synthetic entry point starting execution at
/// `tfunc.body[from_stmt]`, not the function's start. Its params are every
/// local (not just `tfunc.params`) since the interpreter hands it its whole
/// register file - statements before `from_stmt` already ran interpreted and
/// must not be recomputed (side effects would double-run). Scoped to top-level
/// loops; a nested loop would also need to reconstruct which branch led there.
pub fn compile_osr_entry(
    module: &mut dyn Module,
    builder_ctx: &mut FunctionBuilderContext,
    clif_func: &mut cranelift_codegen::ir::Function,
    tfunc: &TFunction,
    from_stmt: usize,
    prog: &ProgramCtx,
) -> Result<(), String> {
    let frontend_config = module.target_config();
    let local_types = collect_local_types(tfunc);
    clif_func.signature = signature_of(
        frontend_config.default_call_conv,
        &local_types,
        &tfunc.return_type,
    );

    let mut builder = FunctionBuilder::new(clif_func, builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);

    let vars: Vec<Variable> = local_types
        .iter()
        .map(|ty| builder.declare_var(clif_type(ty)))
        .collect();
    for (i, var) in vars.iter().enumerate() {
        let val = builder.block_params(entry)[i];
        builder.def_var(*var, val);
    }

    let mut fc = FuncCtx {
        module,
        builder: &mut builder,
        func_ids: prog.func_ids,
        functions: prog.functions,
        inlinable: prog.inlinable,
        runtime: prog.runtime,
        var_scopes: vec![vars],
        return_targets: Vec::new(),
        inline_depth: 0,
        bounds_checked_safe: Vec::new(),
        break_targets: vec![],
        resume_for: false,
    };
    fc.resume_for = matches!(tfunc.body[from_stmt], TStmt::NumericFor { .. });
    let terminated = fc.translate_block(&tfunc.body[from_stmt..]);
    if !terminated {
        return Err(format!("function '{}' does not return a value on every path (OSR entry at statement {from_stmt})", tfunc.name));
    }

    builder.finalize(frontend_config);
    Ok(())
}

struct FuncCtx<'a, 'b> {
    module: &'a mut dyn Module,
    builder: &'a mut FunctionBuilder<'b>,
    func_ids: &'a HashMap<String, FuncId>,
    /// Typed AST by name, needed only to fetch an inline candidate's body.
    functions: &'a HashMap<String, &'a TFunction>,
    inlinable: &'a HashSet<String>,
    runtime: &'a RuntimeFuncs,
    /// Stack of `LocalId -> Variable` mappings; more than one entry only
    /// while inlining (callee gets its own fresh `Variable`s). `cur_var`
    /// reads the top.
    var_scopes: Vec<Vec<Variable>>,
    /// Blocks an inlined callee's `return` should jump to instead of
    /// emitting a real return; empty for the outermost function.
    return_targets: Vec<cranelift_codegen::ir::Block>,
    inline_depth: usize,
    /// `(loop_var_id, array_id)` pairs currently provably in-bounds (M2
    /// bounds-check elimination) - `array_elem_addr` skips the check when
    /// the exact pair it's addressing is on this stack.
    bounds_checked_safe: Vec<(LocalId, LocalId)>,
    break_targets: Vec<cranelift_codegen::ir::Block>,
    resume_for: bool,
}

impl<'a, 'b> FuncCtx<'a, 'b> {
    fn pack_map_value(&mut self, value: ClifValue, ty: &Type) -> ClifValue {
        match ty {
            Type::F64 => self
                .builder
                .ins()
                .bitcast(types::I64, MemFlagsData::new(), value),
            Type::Bool => self.builder.ins().uextend(types::I64, value),
            Type::I64 => value,
            _ => unreachable!("type checking rejects pointer-bearing map values"),
        }
    }

    fn unpack_map_value(&mut self, value: ClifValue, ty: &Type) -> ClifValue {
        match ty {
            Type::F64 => self
                .builder
                .ins()
                .bitcast(types::F64, MemFlagsData::new(), value),
            Type::Bool => self.builder.ins().ireduce(BOOL, value),
            Type::I64 => value,
            _ => unreachable!("type checking rejects pointer-bearing map values"),
        }
    }

    fn cur_var(&self, id: LocalId) -> Variable {
        self.var_scopes
            .last()
            .expect("at least the outer function's scope is always present")[id]
    }

    /// Inlines `callee`'s body at this call site: args evaluate in the
    /// caller's scope, then bind to fresh `Variable`s for the callee's
    /// locals. Each `return` inside becomes a `jump` to a merge block.
    fn inline_call(&mut self, callee: &TFunction, args: &[TExpr]) -> ClifValue {
        let arg_vals: Vec<ClifValue> = args.iter().map(|a| self.translate_expr(a)).collect();

        let local_types = collect_local_types(callee);
        let new_vars: Vec<Variable> = local_types
            .iter()
            .map(|ty| self.builder.declare_var(clif_type(ty)))
            .collect();
        for ((id, _), v) in callee.params.iter().zip(arg_vals) {
            self.builder.def_var(new_vars[*id], v);
        }

        let merge_blk = self.builder.create_block();
        self.builder
            .append_block_param(merge_blk, clif_type(&callee.return_type));

        self.var_scopes.push(new_vars);
        self.return_targets.push(merge_blk);
        self.inline_depth += 1;
        let terminated = self.translate_block(&callee.body);
        self.inline_depth -= 1;
        self.return_targets.pop();
        self.var_scopes.pop();

        if !terminated {
            // typeck guarantees every path returns; reaching here is a codegen bug.
            self.builder.ins().trap(TrapCode::unwrap_user(1));
        }

        self.builder.switch_to_block(merge_blk);
        self.builder.seal_block(merge_blk);
        self.builder.block_params(merge_blk)[0]
    }
    /// Translates a statement sequence; returns `true` if it ends terminated
    /// (by a `return`), so the caller skips appending an unreachable jump.
    fn translate_block(&mut self, stmts: &[TStmt]) -> bool {
        for stmt in stmts {
            if self.translate_stmt(stmt) {
                return true;
            }
        }
        false
    }

    fn translate_stmt(&mut self, stmt: &TStmt) -> bool {
        match stmt {
            TStmt::Break => {
                let target = *self.break_targets.last().expect("typechecked break");
                self.builder.ins().jump(target, &[]);
                true
            }
            TStmt::Local { id, value } => {
                let v = self.translate_expr(value);
                debug_assert_eq!(
                    self.builder.func.dfg.value_type(v),
                    clif_type(&value.ty),
                    "local {id} expression {:?}",
                    value.kind
                );
                self.builder.def_var(self.cur_var(*id), v);
                false
            }
            TStmt::Assign { id, value } => {
                let v = self.translate_expr(value);
                self.builder.def_var(self.cur_var(*id), v);
                false
            }
            TStmt::AssignIndex {
                array,
                index,
                value,
            } => {
                if matches!(array.ty, Type::Map(_, _)) {
                    let map = self.translate_expr(array);
                    let key = self.translate_expr(index);
                    let value_type = value.ty.clone();
                    let value = self.translate_expr(value);
                    let value = self.pack_map_value(value, &value_type);
                    let function = self
                        .module
                        .declare_func_in_func(self.runtime.map_set_i64, self.builder.func);
                    self.builder.ins().call(function, &[map, key, value]);
                } else {
                    let elem_ty = element_type(array);
                    let addr = self.array_elem_addr(array, index, &elem_ty);
                    let v = self.translate_expr(value);
                    self.builder
                        .ins()
                        .store(MemFlagsData::trusted(), v, addr, 0);
                    if elem_ty.is_gc_pointer() {
                        self.emit_write_barrier(addr, v);
                    }
                }
                false
            }
            TStmt::AssignField {
                base,
                field_index,
                value,
            } => {
                let base_ptr = self.translate_expr(base);
                let v = self.translate_expr(value);
                self.builder.ins().store(
                    MemFlagsData::trusted(),
                    v,
                    base_ptr,
                    (*field_index as i32) * 8,
                );
                if value.ty.is_gc_pointer() {
                    self.emit_write_barrier(base_ptr, v);
                }
                false
            }
            TStmt::If {
                cond,
                then_block,
                else_block,
            } => {
                let cond_val = self.translate_expr(cond);
                let then_blk = self.builder.create_block();
                let else_blk = self.builder.create_block();
                let merge_blk = self.builder.create_block();

                self.builder
                    .ins()
                    .brif(cond_val, then_blk, &[], else_blk, &[]);

                self.builder.switch_to_block(then_blk);
                self.builder.seal_block(then_blk);
                let then_terminated = self.translate_block(then_block);
                if !then_terminated {
                    self.builder.ins().jump(merge_blk, &[]);
                }

                self.builder.switch_to_block(else_blk);
                self.builder.seal_block(else_blk);
                let else_terminated = self.translate_block(else_block);
                if !else_terminated {
                    self.builder.ins().jump(merge_blk, &[]);
                }

                self.builder.switch_to_block(merge_blk);
                self.builder.seal_block(merge_blk);
                // If both arms returned, merge_blk has no predecessors - still valid.
                then_terminated && else_terminated
            }
            TStmt::While { cond, body } => {
                let header = self.builder.create_block();
                let body_blk = self.builder.create_block();
                let exit = self.builder.create_block();

                self.builder.ins().jump(header, &[]);
                self.builder.switch_to_block(header);
                let cond_val = self.translate_expr(cond);
                self.builder.ins().brif(cond_val, body_blk, &[], exit, &[]);

                self.builder.switch_to_block(body_blk);
                self.builder.seal_block(body_blk);
                self.break_targets.push(exit);
                let terminated = self.translate_block(body);
                self.break_targets.pop();
                if !terminated {
                    self.builder.ins().jump(header, &[]);
                }
                self.builder.seal_block(header);

                self.builder.switch_to_block(exit);
                self.builder.seal_block(exit);
                false
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
                if !self.resume_for
                    && self.try_vectorize_elementwise_loop(*id, start, stop, step, body)
                {
                    return false;
                }
                let var = self.cur_var(*id);
                let (stop_val, step_val) = if self.resume_for {
                    self.resume_for = false;
                    (
                        self.builder.use_var(self.cur_var(*stop_id)),
                        self.builder.use_var(self.cur_var(*step_id)),
                    )
                } else {
                    let start_val = self.translate_expr(start);
                    let stop_val = self.translate_expr(stop);
                    let step_val = self.translate_expr(step);
                    self.builder.def_var(var, start_val);
                    self.builder.def_var(self.cur_var(*stop_id), stop_val);
                    self.builder.def_var(self.cur_var(*step_id), step_val);
                    self.builder.ins().trapz(step_val, TrapCode::unwrap_user(3));
                    (stop_val, step_val)
                };
                let step_nonneg =
                    self.builder
                        .ins()
                        .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, step_val, 0);

                let header = self.builder.create_block();
                let body_blk = self.builder.create_block();
                let exit = self.builder.create_block();

                self.builder.ins().jump(header, &[]);
                self.builder.switch_to_block(header);
                let i_val = self.builder.use_var(var);
                let cond_pos =
                    self.builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, i_val, stop_val);
                let cond_neg =
                    self.builder
                        .ins()
                        .icmp(IntCC::SignedGreaterThanOrEqual, i_val, stop_val);
                let cond = self.builder.ins().select(step_nonneg, cond_pos, cond_neg);
                self.builder.ins().brif(cond, body_blk, &[], exit, &[]);

                self.builder.switch_to_block(body_blk);
                self.builder.seal_block(body_blk);
                let safe_array = recognize_safe_for_loop(start, stop, step).filter(|&array_id| {
                    !assigns_to_local(body, array_id) && !assigns_to_local(body, *id)
                });
                if let Some(array_id) = safe_array {
                    self.bounds_checked_safe.push((*id, array_id));
                }
                self.break_targets.push(exit);
                let terminated = self.translate_block(body);
                self.break_targets.pop();
                if safe_array.is_some() {
                    self.bounds_checked_safe.pop();
                }
                if !terminated {
                    let i_val2 = self.builder.use_var(var);
                    let (next, overflow) = self.builder.ins().sadd_overflow(i_val2, step_val);
                    self.builder.def_var(var, next);
                    self.builder.ins().brif(overflow, exit, &[], header, &[]);
                }
                self.builder.seal_block(header);

                self.builder.switch_to_block(exit);
                self.builder.seal_block(exit);
                false
            }
            TStmt::Return { value } => {
                let v = value.as_ref().map(|e| self.translate_expr(e));
                match self.return_targets.last().copied() {
                    // Inlined: jump to the call site's merge block instead
                    // of a real function return - see `inline_call`.
                    Some(target) => match v {
                        Some(v) => self.builder.ins().jump(target, &[BlockArg::Value(v)]),
                        None => self.builder.ins().jump(target, &[]),
                    },
                    None => match v {
                        Some(v) => self.builder.ins().return_(&[v]),
                        None => self.builder.ins().return_(&[]),
                    },
                };
                true
            }
        }
    }

    /// Emits a call to `sol_gc_write_barrier(container, value)` - `container`
    /// is any address inside the block just mutated (exact byte offset
    /// doesn't matter, only which chunk it resolves to; see gc.rs). Only
    /// needed at mutation sites for an already-existing object
    /// (`AssignField`/non-map `AssignIndex`) - a literal's own fresh
    /// allocation is always Young at construction time, so storing into it
    /// can never create a new Old -> Young edge.
    fn emit_write_barrier(&mut self, container: ClifValue, value: ClifValue) {
        let func_ref = self
            .module
            .declare_func_in_func(self.runtime.gc_write_barrier, self.builder.func);
        self.builder.ins().call(func_ref, &[container, value]);
    }

    fn array_elem_addr(&mut self, array: &TExpr, index: &TExpr, elem_ty: &Type) -> ClifValue {
        let array_val = self.translate_expr(array);
        let index_val = self.translate_expr(index);

        if !self.is_provably_in_bounds(array, index) {
            // Unsigned compare catches both negative index (wraps huge) and index >= len.
            let len = self
                .builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), array_val, 0);
            let oob = self
                .builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, index_val, len);
            self.builder.ins().trapnz(oob, TrapCode::HEAP_OUT_OF_BOUNDS);
        }

        let data_ptr = self
            .builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), array_val, 8);
        let elem_size = 8i64; // both i64 and f64 are 8 bytes in M1
        let offset = self.builder.ins().imul_imm_s(index_val, elem_size);
        let _ = elem_ty;
        self.builder.ins().iadd(data_ptr, offset)
    }

    /// M7 §21: recognizes `for i = start, stop do out[i] = a[i] OP b[i] end`
    /// (elementwise binary op over three `f64` arrays, step 1) and emits
    /// Cranelift's own vector types (portable across ISAs, no per-arch
    /// intrinsics) 2 lanes at a time, plus a scalar tail for any leftover
    /// element. Returns `false` (caller falls through to the normal
    /// scalar loop) for anything else - a narrow pattern match, not
    /// general auto-vectorization, matching M2's bounds-check-elimination
    /// precedent.
    fn try_vectorize_elementwise_loop(
        &mut self,
        id: LocalId,
        start: &TExpr,
        stop: &TExpr,
        step: &TExpr,
        body: &[TStmt],
    ) -> bool {
        if std::env::var_os("SOL_NO_VECTORIZE")
            .or_else(|| std::env::var_os("SOL_NO_VECTORIZE"))
            .is_some()
        {
            return false; // escape hatch for debugging/benchmark comparison
        }
        let Some(pat) = detect_vectorizable_loop(id, step, body) else {
            return false;
        };

        let start_val = self.translate_expr(start);
        let stop_val = self.translate_expr(stop);
        let out_ptr = self.builder.use_var(self.cur_var(pat.out));
        let a_ptr = self.builder.use_var(self.cur_var(pat.a));
        let b_ptr = self.builder.use_var(self.cur_var(pat.b));

        let has_iters = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThanOrEqual, start_val, stop_val);
        let check_blk = self.builder.create_block();
        let exit_blk = self.builder.create_block();
        self.builder
            .ins()
            .brif(has_iters, check_blk, &[], exit_blk, &[]);

        // Whole-range bounds check for all three arrays up front - sound
        // because the matched body has no side effects besides these
        // writes, so trapping before any of them run is indistinguishable
        // from the scalar loop's first out-of-bounds access.
        self.builder.switch_to_block(check_blk);
        self.builder.seal_block(check_blk);
        for arr_ptr in [out_ptr, a_ptr, b_ptr] {
            let len = self
                .builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), arr_ptr, 0);
            let start_oob =
                self.builder
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, start_val, len);
            let stop_oob =
                self.builder
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, stop_val, len);
            let oob = self.builder.ins().bor(start_oob, stop_oob);
            self.builder.ins().trapnz(oob, TrapCode::HEAP_OUT_OF_BOUNDS);
        }
        let out_data = self
            .builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), out_ptr, 8);
        let a_data = self
            .builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), a_ptr, 8);
        let b_data = self
            .builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), b_ptr, 8);
        let count = self.builder.ins().isub(stop_val, start_val);
        let count_incl = self.builder.ins().iadd_imm_s(count, 1); // total iterations (inclusive stop)
        let two = self.builder.ins().iconst(types::I64, 2);
        let vec_iters = self.builder.ins().udiv(count_incl, two);

        let vec_header = self.builder.create_block();
        let vec_body = self.builder.create_block();
        let scalar_setup = self.builder.create_block();
        self.builder.append_block_param(vec_header, types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder
            .ins()
            .jump(vec_header, &[BlockArg::Value(zero)]);

        self.builder.switch_to_block(vec_header);
        let k = self.builder.block_params(vec_header)[0];
        let more = self.builder.ins().icmp(IntCC::SignedLessThan, k, vec_iters);
        self.builder
            .ins()
            .brif(more, vec_body, &[], scalar_setup, &[]);

        self.builder.switch_to_block(vec_body);
        self.builder.seal_block(vec_body);
        let byte_off = self.builder.ins().imul_imm_s(k, 16); // 2 lanes * 8 bytes
        let a_addr = self.builder.ins().iadd(a_data, byte_off);
        let b_addr = self.builder.ins().iadd(b_data, byte_off);
        let out_addr = self.builder.ins().iadd(out_data, byte_off);
        let a_vec = self
            .builder
            .ins()
            .load(types::F64X2, MemFlagsData::trusted(), a_addr, 0);
        let b_vec = self
            .builder
            .ins()
            .load(types::F64X2, MemFlagsData::trusted(), b_addr, 0);
        let result_vec = arith_f(self.builder, pat.op, a_vec, b_vec);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), result_vec, out_addr, 0);
        let k_next = self.builder.ins().iadd_imm_s(k, 1);
        self.builder
            .ins()
            .jump(vec_header, &[BlockArg::Value(k_next)]);
        self.builder.seal_block(vec_header);

        // Scalar tail: at most one leftover element (count_incl may be odd).
        self.builder.switch_to_block(scalar_setup);
        self.builder.seal_block(scalar_setup);
        let handled = self.builder.ins().imul_imm_s(vec_iters, 2);
        let has_tail = self
            .builder
            .ins()
            .icmp(IntCC::SignedLessThan, handled, count_incl);
        let tail_blk = self.builder.create_block();
        self.builder
            .ins()
            .brif(has_tail, tail_blk, &[], exit_blk, &[]);

        self.builder.switch_to_block(tail_blk);
        self.builder.seal_block(tail_blk);
        let tail_off = self.builder.ins().imul_imm_s(handled, 8);
        let a_tail = self.builder.ins().iadd(a_data, tail_off);
        let b_tail = self.builder.ins().iadd(b_data, tail_off);
        let out_tail = self.builder.ins().iadd(out_data, tail_off);
        let a_s = self
            .builder
            .ins()
            .load(types::F64, MemFlagsData::trusted(), a_tail, 0);
        let b_s = self
            .builder
            .ins()
            .load(types::F64, MemFlagsData::trusted(), b_tail, 0);
        let result_s = arith_f(self.builder, pat.op, a_s, b_s);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), result_s, out_tail, 0);
        self.builder.ins().jump(exit_blk, &[]);

        self.builder.switch_to_block(exit_blk);
        self.builder.seal_block(exit_blk);
        true
    }

    /// True only for the narrow `(index, array)` pair `NumericFor` already
    /// proved safe for this loop body - see `recognize_safe_for_loop`.
    fn is_provably_in_bounds(&self, array: &TExpr, index: &TExpr) -> bool {
        let (TExprKind::Local(array_id), TExprKind::Local(index_id)) = (&array.kind, &index.kind)
        else {
            return false;
        };
        self.bounds_checked_safe.contains(&(*index_id, *array_id))
    }

    // Floor quotient/modulus with Lua's sign rules. Avoid the MIN/-1
    // hardware overflow trap; division by zero still traps in native code.
    fn integer_floor(&mut self, a: ClifValue, b: ClifValue, modulo: bool) -> ClifValue {
        let minus_one = self.builder.ins().icmp_imm_s(IntCC::Equal, b, -1);
        let one = self.builder.ins().iconst(types::I64, 1);
        let divisor = self.builder.ins().select(minus_one, one, b);
        let rem = self.builder.ins().srem(a, divisor);
        let nonzero = self.builder.ins().icmp_imm_s(IntCC::NotEqual, rem, 0);
        let signs = self.builder.ins().bxor(rem, b);
        let different = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedLessThan, signs, 0);
        let adjust = self.builder.ins().band(nonzero, different);
        if modulo {
            let added = self.builder.ins().iadd(rem, b);
            self.builder.ins().select(adjust, added, rem)
        } else {
            let q = self.builder.ins().sdiv(a, divisor);
            let negative = self.builder.ins().ineg(a);
            let q = self.builder.ins().select(minus_one, negative, q);
            let below = self.builder.ins().iadd_imm_s(q, -1);
            self.builder.ins().select(adjust, below, q)
        }
    }

    fn shift(&mut self, a: ClifValue, b: ClifValue, left: bool) -> ClifValue {
        let negative = self.builder.ins().icmp_imm_s(IntCC::SignedLessThan, b, 0);
        let negated = self.builder.ins().ineg(b);
        let count = self.builder.ins().select(negative, negated, b);
        let too_large = self
            .builder
            .ins()
            .icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, count, 64);
        let l = self.builder.ins().ishl(a, count);
        let r = self.builder.ins().ushr(a, count);
        let shifted = if left {
            self.builder.ins().select(negative, r, l)
        } else {
            self.builder.ins().select(negative, l, r)
        };
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().select(too_large, zero, shifted)
    }

    fn runtime_call(&mut self, id: FuncId, args: &[ClifValue]) -> ClifValue {
        let f = self.module.declare_func_in_func(id, self.builder.func);
        let call = self.builder.ins().call(f, args);
        self.builder.inst_results(call)[0]
    }
    fn truth_value(&mut self, v: ClifValue, ty: &Type) -> ClifValue {
        match ty {
            Type::Bool => v,
            Type::Any => {
                let v = self.runtime_call(self.runtime.truth, &[v]);
                self.builder.ins().ireduce(BOOL, v)
            }
            Type::Nil => self.builder.ins().iconst(BOOL, 0),
            _ => self.builder.ins().iconst(BOOL, 1),
        }
    }

    fn translate_expr(&mut self, expr: &TExpr) -> ClifValue {
        match &expr.kind {
            TExprKind::StringLit(bytes) => {
                let mut data = cranelift_module::DataDescription::new();
                let mut encoded = (bytes.len() as u64).to_ne_bytes().to_vec();
                encoded.extend_from_slice(bytes);
                data.define(encoded.into_boxed_slice());
                data.set_align(8);
                let id = self
                    .module
                    .declare_anonymous_data(false, false)
                    .expect("string literal declaration");
                self.module
                    .define_data(id, &data)
                    .expect("string literal definition");
                let gv = self.module.declare_data_in_func(id, self.builder.func);
                self.builder.ins().symbol_value(types::I64, gv)
            }
            TExprKind::NilLit => self.builder.ins().iconst(types::I64, 0),
            TExprKind::Truth(e) => {
                let v = self.translate_expr(e);
                self.truth_value(v, &e.ty)
            }
            TExprKind::IntLit(n) => self.builder.ins().iconst(types::I64, *n),
            TExprKind::FloatLit(n) => self.builder.ins().f64const(*n),
            TExprKind::BoolLit(b) => self.builder.ins().iconst(BOOL, if *b { 1 } else { 0 }),
            TExprKind::Local(id) => self.builder.use_var(self.cur_var(*id)),
            TExprKind::FunctionRef(name) => {
                let func_id = *self
                    .func_ids
                    .get(name)
                    .expect("typeck already verified this function exists");
                let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                self.builder.ins().func_addr(types::I64, func_ref)
            }
            TExprKind::Neg(e) => {
                let v = self.translate_expr(e);
                if e.ty == Type::Any {
                    return self.runtime_call(self.runtime.dynamic_neg, &[v]);
                }
                if e.ty == Type::F64 {
                    self.builder.ins().fneg(v)
                } else {
                    self.builder.ins().ineg(v)
                }
            }
            TExprKind::Not(e) => {
                let v = self.translate_expr(e);
                self.builder.ins().icmp_imm_s(IntCC::Equal, v, 0)
            }
            TExprKind::IntToFloat(e) => {
                let v = self.translate_expr(e);
                self.builder.ins().fcvt_from_sint(types::F64, v)
            }
            TExprKind::Arith(op, l, r) => {
                let lv = self.translate_expr(l);
                let rv = self.translate_expr(r);
                if l.ty == Type::Any {
                    let opval = self.builder.ins().iconst(types::I64, *op as i64);
                    return self.runtime_call(self.runtime.dynamic_binary, &[lv, rv, opval]);
                }
                let is_float = l.ty == Type::F64;
                match (op, is_float) {
                    (BinaryOp::Add, false) => self.builder.ins().iadd(lv, rv),
                    (BinaryOp::Add, true) => self.builder.ins().fadd(lv, rv),
                    (BinaryOp::Sub, false) => self.builder.ins().isub(lv, rv),
                    (BinaryOp::Sub, true) => self.builder.ins().fsub(lv, rv),
                    (BinaryOp::Mul, false) => self.builder.ins().imul(lv, rv),
                    (BinaryOp::Mul, true) => self.builder.ins().fmul(lv, rv),
                    (BinaryOp::FloorDiv, false) => self.integer_floor(lv, rv, false),
                    (BinaryOp::FloorDiv, true) => {
                        let q = self.builder.ins().fdiv(lv, rv);
                        self.builder.ins().floor(q)
                    }
                    (BinaryOp::BitAnd, false) => self.builder.ins().band(lv, rv),
                    (BinaryOp::BitOr, false) => self.builder.ins().bor(lv, rv),
                    (BinaryOp::BitXor, false) => self.builder.ins().bxor(lv, rv),
                    (BinaryOp::Shl, false) => self.shift(lv, rv, true),
                    (BinaryOp::Shr, false) => self.shift(lv, rv, false),
                    (BinaryOp::Pow, true) | (BinaryOp::Mod, true) => {
                        let id = if *op == BinaryOp::Pow {
                            self.runtime.pow
                        } else {
                            self.runtime.mod_float
                        };
                        let f = self.module.declare_func_in_func(id, self.builder.func);
                        let call = self.builder.ins().call(f, &[lv, rv]);
                        self.builder.inst_results(call)[0]
                    }
                    (BinaryOp::Div, true) => self.builder.ins().fdiv(lv, rv),
                    (BinaryOp::Mod, false) => self.integer_floor(lv, rv, true),
                    _ => unreachable!("typeck only allows Mod for i64"),
                }
            }
            TExprKind::Compare(op, l, r) => {
                let mut lv = self.translate_expr(l);
                let mut rv = self.translate_expr(r);
                if l.ty == Type::String {
                    lv = self.runtime_call(self.runtime.string_compare, &[lv, rv]);
                    rv = self.builder.ins().iconst(types::I64, 0);
                }
                if l.ty == Type::Any {
                    let opval = self.builder.ins().iconst(types::I64, *op as i64);
                    let v = self.runtime_call(self.runtime.dynamic_compare, &[lv, rv, opval]);
                    return self.builder.ins().ireduce(BOOL, v);
                }
                let is_float = l.ty == Type::F64;
                if is_float {
                    let cc = match op {
                        BinaryOp::Eq => FloatCC::Equal,
                        BinaryOp::NotEq => FloatCC::NotEqual,
                        BinaryOp::Lt => FloatCC::LessThan,
                        BinaryOp::Le => FloatCC::LessThanOrEqual,
                        BinaryOp::Gt => FloatCC::GreaterThan,
                        BinaryOp::Ge => FloatCC::GreaterThanOrEqual,
                        _ => unreachable!("not a comparison op"),
                    };
                    self.builder.ins().fcmp(cc, lv, rv)
                } else {
                    let cc = match op {
                        BinaryOp::Eq => IntCC::Equal,
                        BinaryOp::NotEq => IntCC::NotEqual,
                        BinaryOp::Lt => IntCC::SignedLessThan,
                        BinaryOp::Le => IntCC::SignedLessThanOrEqual,
                        BinaryOp::Gt => IntCC::SignedGreaterThan,
                        BinaryOp::Ge => IntCC::SignedGreaterThanOrEqual,
                        _ => unreachable!("not a comparison op"),
                    };
                    self.builder.ins().icmp(cc, lv, rv)
                }
            }
            TExprKind::Logical(op, l, r) => {
                let lv = self.translate_expr(l);
                let rhs = self.builder.create_block();
                let merge = self.builder.create_block();
                self.builder.append_block_param(merge, clif_type(&l.ty));
                let test = self.truth_value(lv, &l.ty);
                if *op == BinaryOp::And {
                    self.builder
                        .ins()
                        .brif(test, rhs, &[], merge, &[BlockArg::Value(lv)]);
                } else {
                    self.builder
                        .ins()
                        .brif(test, merge, &[BlockArg::Value(lv)], rhs, &[]);
                }
                self.builder.switch_to_block(rhs);
                self.builder.seal_block(rhs);
                let rv = self.translate_expr(r);
                self.builder.ins().jump(merge, &[BlockArg::Value(rv)]);
                self.builder.switch_to_block(merge);
                self.builder.seal_block(merge);
                self.builder.block_params(merge)[0]
            }
            TExprKind::Len(e) => {
                let v = self.translate_expr(e);
                self.builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), v, 0)
            }
            TExprKind::Index(array, index) => {
                if matches!(array.ty, Type::Map(_, _)) {
                    let map = self.translate_expr(array);
                    let key = self.translate_expr(index);
                    let function = self
                        .module
                        .declare_func_in_func(self.runtime.map_get_i64, self.builder.func);
                    let call = self.builder.ins().call(function, &[map, key]);
                    let value = self.builder.inst_results(call)[0];
                    self.unpack_map_value(value, &expr.ty)
                } else {
                    let elem_ty = element_type(array);
                    let addr = self.array_elem_addr(array, index, &elem_ty);
                    self.builder
                        .ins()
                        .load(clif_type(&elem_ty), MemFlagsData::trusted(), addr, 0)
                }
            }
            TExprKind::NewArray { elem, len } => {
                let len_val = self.translate_expr(len);
                let func_id = if *elem == Type::I64 {
                    self.runtime.new_array_i64
                } else {
                    self.runtime.new_array_f64
                };
                let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                let call = self.builder.ins().call(func_ref, &[len_val]);
                self.builder.inst_results(call)[0]
            }
            TExprKind::ArrayLiteral { elem, values } => {
                let values: Vec<ClifValue> = values
                    .iter()
                    .map(|value| self.translate_expr(value))
                    .collect();
                let len = self.builder.ins().iconst(types::I64, values.len() as i64);
                let function = if elem.is_gc_pointer() {
                    self.runtime.new_array_ptr
                } else if *elem == Type::I64 {
                    self.runtime.new_array_i64
                } else {
                    self.runtime.new_array_f64
                };
                let function = self
                    .module
                    .declare_func_in_func(function, self.builder.func);
                let call = self.builder.ins().call(function, &[len]);
                let array = self.builder.inst_results(call)[0];
                let data = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), array, 8);
                for (index, value) in values.into_iter().enumerate() {
                    self.builder.ins().store(
                        MemFlagsData::trusted(),
                        value,
                        data,
                        index as i32 * 8,
                    );
                }
                array
            }
            TExprKind::ArrayMap { array, callback } => {
                let array = self.translate_expr(array);
                let callback = self.translate_expr(callback);
                self.runtime_call(self.runtime.array_map_i64, &[array, callback])
            }
            TExprKind::NewMap { .. } => {
                let function = self
                    .module
                    .declare_func_in_func(self.runtime.new_map_i64, self.builder.func);
                let call = self.builder.ins().call(function, &[]);
                self.builder.inst_results(call)[0]
            }
            TExprKind::MapLiteral { entries, .. } => {
                let entries: Vec<(ClifValue, ClifValue)> = entries
                    .iter()
                    .map(|(key, value)| {
                        let key = self.translate_expr(key);
                        let value_type = value.ty.clone();
                        let value = self.translate_expr(value);
                        (key, self.pack_map_value(value, &value_type))
                    })
                    .collect();
                let new_map = self
                    .module
                    .declare_func_in_func(self.runtime.new_map_i64, self.builder.func);
                let call = self.builder.ins().call(new_map, &[]);
                let map = self.builder.inst_results(call)[0];
                let set = self
                    .module
                    .declare_func_in_func(self.runtime.map_set_i64, self.builder.func);
                for (key, value) in entries {
                    self.builder.ins().call(set, &[map, key, value]);
                }
                map
            }
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                let map = self.translate_expr(map);
                let cursor = self.translate_expr(cursor);
                let function = match &expr.kind {
                    TExprKind::MapNext { .. } => self.runtime.map_next_i64,
                    TExprKind::MapKey { .. } => self.runtime.map_key_at_i64,
                    TExprKind::MapValue { .. } => self.runtime.map_value_at_i64,
                    _ => unreachable!(),
                };
                let function = self
                    .module
                    .declare_func_in_func(function, self.builder.func);
                let call = self.builder.ins().call(function, &[map, cursor]);
                let value = self.builder.inst_results(call)[0];
                if matches!(&expr.kind, TExprKind::MapValue { .. }) {
                    self.unpack_map_value(value, &expr.ty)
                } else {
                    value
                }
            }
            TExprKind::StructLiteral { fields, .. } => {
                // Already reordered to declared field order by typeck - `i * 8` is the offset.
                let field_vals: Vec<ClifValue> =
                    fields.iter().map(|f| self.translate_expr(f)).collect();
                let size = self
                    .builder
                    .ins()
                    .iconst(types::I64, fields.len() as i64 * 8);
                let pointer_mask = self.builder.ins().iconst(
                    types::I64,
                    pointer_layout_mask(fields.iter().map(|field| &field.ty)) as i64,
                );
                let func_ref = self
                    .module
                    .declare_func_in_func(self.runtime.alloc_layout, self.builder.func);
                let call = self.builder.ins().call(func_ref, &[size, pointer_mask]);
                let ptr = self.builder.inst_results(call)[0];
                for (i, v) in field_vals.into_iter().enumerate() {
                    self.builder
                        .ins()
                        .store(MemFlagsData::trusted(), v, ptr, (i as i32) * 8);
                }
                ptr
            }
            TExprKind::Field { base, field_index } => {
                let base_ptr = self.translate_expr(base);
                self.builder.ins().load(
                    clif_type(&expr.ty),
                    MemFlagsData::trusted(),
                    base_ptr,
                    (*field_index as i32) * 8,
                )
            }
            TExprKind::Call(name, args) => {
                if self.inlinable.contains(name) && self.inline_depth < MAX_INLINE_DEPTH {
                    let callee = *self
                        .functions
                        .get(name)
                        .expect("typeck already verified this function exists");
                    return self.inline_call(callee, args);
                }
                let arg_vals: Vec<ClifValue> =
                    args.iter().map(|a| self.translate_expr(a)).collect();
                let func_id = *self
                    .func_ids
                    .get(name)
                    .expect("typeck already verified this function exists");
                let func_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                let call = self.builder.ins().call(func_ref, &arg_vals);
                self.builder.inst_results(call)[0]
            }
            TExprKind::CallIndirect { callee, args } => {
                let Type::Function {
                    params,
                    return_type,
                } = &callee.ty
                else {
                    unreachable!("typeck only builds indirect calls for function values");
                };
                let callee = self.translate_expr(callee);
                let arg_vals: Vec<ClifValue> =
                    args.iter().map(|arg| self.translate_expr(arg)).collect();
                let signature = signature_of(
                    self.module.target_config().default_call_conv,
                    params,
                    return_type,
                );
                let signature = self.builder.import_signature(signature);
                let call = self
                    .builder
                    .ins()
                    .call_indirect(signature, callee, &arg_vals);
                self.builder.inst_results(call)[0]
            }
            TExprKind::Box(inner) => {
                let v = self.translate_expr(inner);
                let tag =
                    value::tag_for(&inner.ty).expect("typeck's coerce only boxes scalar types");
                // Reinterpret bits (not convert) into the payload's i64 slot.
                let payload = match inner.ty {
                    Type::F64 => self
                        .builder
                        .ins()
                        .bitcast(types::I64, MemFlagsData::new(), v),
                    Type::Bool => self.builder.ins().uextend(types::I64, v),
                    _ => v, // I64
                };
                let tag_val = self.builder.ins().iconst(types::I64, tag);
                let size = self.builder.ins().iconst(types::I64, 16);
                let pointer_mask = self
                    .builder
                    .ins()
                    .iconst(types::I64, if inner.ty.is_gc_pointer() { 0b10 } else { 0 });
                let func_ref = self
                    .module
                    .declare_func_in_func(self.runtime.alloc_layout, self.builder.func);
                let call = self.builder.ins().call(func_ref, &[size, pointer_mask]);
                let ptr = self.builder.inst_results(call)[0];
                self.builder
                    .ins()
                    .store(MemFlagsData::trusted(), tag_val, ptr, 0);
                self.builder
                    .ins()
                    .store(MemFlagsData::trusted(), payload, ptr, 8);
                ptr
            }
            TExprKind::Unbox(inner, target) => {
                let ptr = self.translate_expr(inner);
                let expected_tag =
                    value::tag_for(target).expect("typeck's coerce only unboxes to scalar types");
                let tag = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), ptr, 0);
                let mismatch = self
                    .builder
                    .ins()
                    .icmp_imm_s(IntCC::NotEqual, tag, expected_tag);
                self.builder.ins().trapnz(mismatch, TRAP_ANY_TYPE_MISMATCH);
                let payload = self
                    .builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), ptr, 8);
                match target {
                    Type::F64 => {
                        self.builder
                            .ins()
                            .bitcast(types::F64, MemFlagsData::new(), payload)
                    }
                    Type::Bool => self.builder.ins().ireduce(BOOL, payload),
                    _ => payload, // I64
                }
            }
        }
    }
}

fn element_type(array_expr: &TExpr) -> Type {
    match &array_expr.ty {
        Type::Array(inner) => (**inner).clone(),
        _ => unreachable!("typeck only builds Index/AssignIndex nodes over Array types"),
    }
}

/// A scalar or vector `fadd`/`fsub`/`fmul`/`fdiv` - the instruction is the
/// same either way, just parameterized by the operand's own Cranelift type
/// (scalar `F64` or vector `F64X2`).
fn arith_f(builder: &mut FunctionBuilder, op: BinaryOp, l: ClifValue, r: ClifValue) -> ClifValue {
    match op {
        BinaryOp::Add => builder.ins().fadd(l, r),
        BinaryOp::Sub => builder.ins().fsub(l, r),
        BinaryOp::Mul => builder.ins().fmul(l, r),
        BinaryOp::Div => builder.ins().fdiv(l, r),
        _ => unreachable!("detect_vectorizable_loop only matches these ops"),
    }
}

struct VectorizablePattern {
    out: LocalId,
    a: LocalId,
    b: LocalId,
    op: BinaryOp,
}

/// Matches `for i = start, stop do out[i] = a[i] OP b[i] end` (step 1,
/// `out`/`a`/`b` all `Array<f64>`, `OP` one of `+ - * /`) - see
/// `try_vectorize_elementwise_loop`.
fn detect_vectorizable_loop(
    id: LocalId,
    step: &TExpr,
    body: &[TStmt],
) -> Option<VectorizablePattern> {
    if !matches!(step.kind, TExprKind::IntLit(1)) {
        return None;
    }
    let [TStmt::AssignIndex {
        array,
        index,
        value,
    }] = body
    else {
        return None;
    };
    let (TExprKind::Local(out), TExprKind::Local(idx_id)) = (&array.kind, &index.kind) else {
        return None;
    };
    if *idx_id != id || !matches!(&array.ty, Type::Array(inner) if **inner == Type::F64) {
        return None;
    }
    let TExprKind::Arith(op, l, r) = &value.kind else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div
    ) {
        return None;
    }
    let (TExprKind::Index(a_base, a_idx), TExprKind::Index(b_base, b_idx)) = (&l.kind, &r.kind)
    else {
        return None;
    };
    let (TExprKind::Local(a), TExprKind::Local(a_i)) = (&a_base.kind, &a_idx.kind) else {
        return None;
    };
    let (TExprKind::Local(b), TExprKind::Local(b_i)) = (&b_base.kind, &b_idx.kind) else {
        return None;
    };
    if *a_i != id || *b_i != id {
        return None;
    }
    if !matches!(&a_base.ty, Type::Array(inner) if **inner == Type::F64)
        || !matches!(&b_base.ty, Type::Array(inner) if **inner == Type::F64)
    {
        return None;
    }
    Some(VectorizablePattern {
        out: *out,
        a: *a,
        b: *b,
        op: *op,
    })
}

/// Recognizes `for i = 0, #a - 1 do ... end` and returns `a`'s `LocalId` -
/// the one bounds-check-elimination pattern implemented; anything else
/// stays fully checked.
fn recognize_safe_for_loop(start: &TExpr, stop: &TExpr, step: &TExpr) -> Option<LocalId> {
    if !matches!(start.kind, TExprKind::IntLit(0)) || !matches!(step.kind, TExprKind::IntLit(1)) {
        return None;
    }
    let TExprKind::Arith(BinaryOp::Sub, lhs, rhs) = &stop.kind else {
        return None;
    };
    if !matches!(rhs.kind, TExprKind::IntLit(1)) {
        return None;
    }
    let TExprKind::Len(inner) = &lhs.kind else {
        return None;
    };
    match &inner.kind {
        TExprKind::Local(array_id) => Some(*array_id),
        _ => None,
    }
}

/// True if `stmts` (recursively) assigns to local `id` - used to bail out
/// of bounds-check elimination if the array or loop variable is reassigned.
fn assigns_to_local(stmts: &[TStmt], id: LocalId) -> bool {
    stmts.iter().any(|s| match s {
        TStmt::Break => false,
        TStmt::Assign { id: assigned, .. } => *assigned == id,
        TStmt::AssignIndex { .. }
        | TStmt::AssignField { .. }
        | TStmt::Local { .. }
        | TStmt::Return { .. } => false,
        TStmt::If {
            then_block,
            else_block,
            ..
        } => assigns_to_local(then_block, id) || assigns_to_local(else_block, id),
        TStmt::While { body, .. } => assigns_to_local(body, id),
        TStmt::NumericFor { body, .. } => assigns_to_local(body, id),
    })
}
