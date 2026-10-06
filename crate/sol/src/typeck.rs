// Type inference + checking: untyped AST (ast.rs) -> typed AST (types.rs).
// Two passes: collect every function's signature first (so forward refs and
// recursion work), then type-check each body against the full table. Every
// function must declare a return type - no `void`, no `print` builtin.

use std::collections::{HashMap, HashSet};

use crate::ast::{self, BinaryOp, UnaryOp};
use crate::types::*;

/// Flow-sensitive inference and optimization explanations for annotation-free
/// Lua/Sol. This is deliberately separate from lowering: it can prove facts
/// for every source accepted by the generic runtime without making those
/// facts part of program validity.
pub mod inference;

/// Error-code tag marking a type error that means "this is valid Lua syntax,
/// but it's dynamic-only and must run through `lua_runtime`'s interpreter
/// instead of the typed native pipeline" — as opposed to a genuine type
/// error. The two `check_stmt`/`check_expr` arms below are the only
/// producers; `requires_dynamic_runtime` is the only consumer (`main.rs`'s
/// `run()` fallback). Keeping this as a dedicated tag rather than matching
/// on the message text means rewording either message, or any unrelated
/// error that happens to mention "dynamic Lua" in prose, can't silently
/// change which errors trigger the fallback.
const DYNAMIC_RUNTIME_CODE: &str = "EDYNLUA";

/// Whether a `typeck::check`/`compile_bytes` error means the source needs
/// the dynamic Lua interpreter rather than indicating a genuine type error.
pub fn requires_dynamic_runtime(error: &str) -> bool {
    error.ends_with(&format!("[{DYNAMIC_RUNTIME_CODE}]"))
}

/// Struct/signature/extern collection shared by `check()` and
/// `check_partitioned()`: independent of whether any function body later
/// succeeds, so both entry points can build on the exact same table.
struct Prepared {
    structs: Structs,
    sigs: Sigs,
    externs: Vec<TExternFunction>,
}

fn prepare(program: &ast::Program, lua_mode: bool) -> Result<Prepared, String> {
    // Collect struct names up front so mutually-referencing structs work
    // (every field is a fixed 8-byte slot, so no "infinite size" cycle risk).
    let struct_names: HashSet<String> = program.structs.iter().map(|s| s.name.clone()).collect();

    let mut structs = HashMap::new();
    for s in &program.structs {
        let mut fields = Vec::with_capacity(s.fields.len());
        for (fname, ty) in &s.fields {
            fields.push((fname.clone(), lower_type(ty, &struct_names, s.line)?));
        }
        if structs
            .insert(s.name.clone(), StructLayout { fields })
            .is_some()
        {
            return Err(format!("line {}: duplicate struct '{}'", s.line, s.name));
        }
    }

    let mut sigs = HashMap::new();
    for f in &program.functions {
        let param_types: Vec<Type> = f
            .params
            .iter()
            .map(|(_, t)| lower_type(t, &struct_names, f.line))
            .collect::<Result<Vec<_>, _>>()?;
        let return_type = match &f.return_type {
            Some(t) => lower_type(t, &struct_names, f.line)?,
            None => Type::Any,
        };
        if sigs
            .insert(f.name.clone(), (param_types, return_type))
            .is_some()
        {
            // In `.lua`, `function f() ... end` at top level is sugar for a
            // global assignment - rebinding the same name later in the
            // chunk is ordinary, legal Lua (each call site resolves to
            // whichever body was most recently assigned when it runs), not
            // an error. The single flat `sigs` table has no way to model
            // "two different bodies live at different points in the
            // chunk", so tag it `EDYNLUA` and let the dynamic runtime,
            // which really does rebind on each execution, handle it. `.sol`
            // has one fixed global function table with no rebinding, so a
            // repeated name there stays a genuine hard error.
            if lua_mode {
                return Err(format!(
                    "line {}: duplicate function '{}' [{DYNAMIC_RUNTIME_CODE}]",
                    f.line, f.name
                ));
            }
            return Err(format!("line {}: duplicate function '{}'", f.line, f.name));
        }
    }

    // FFI (M7 §25): extern signatures share the same call-resolution table
    // as regular functions - typeck/codegen never need to know the
    // difference at a call site. Only scalar types cross the C ABI for now.
    let mut externs = Vec::with_capacity(program.externs.len());
    for e in &program.externs {
        let param_types: Vec<Type> = e
            .params
            .iter()
            .map(|(_, t)| lower_type(t, &struct_names, e.line))
            .collect::<Result<Vec<_>, _>>()?;
        let return_type = lower_type(&e.return_type, &struct_names, e.line)?;
        for ty in param_types.iter().chain([&return_type]) {
            if !matches!(ty, Type::I64 | Type::F64 | Type::Bool) {
                return Err(format!(
                    "line {}: extern '{}' - only i64/f64/bool cross the FFI boundary, found {ty}",
                    e.line, e.name
                ));
            }
        }
        if sigs
            .insert(e.name.clone(), (param_types.clone(), return_type.clone()))
            .is_some()
        {
            return Err(format!("line {}: duplicate function '{}'", e.line, e.name));
        }
        externs.push(TExternFunction {
            name: e.name.clone(),
            params: param_types,
            return_type,
        });
    }

    Ok(Prepared {
        structs,
        sigs,
        externs,
    })
}

/// Resolves omitted result annotations to a fixed point, so a caller can
/// infer a forward callee's scalar result without boxing its own locals.
/// Shared by `check()` and `check_partitioned()`: a function whose body
/// doesn't check yet (including one that will end up dynamic-only) simply
/// doesn't contribute an inferred type, same as today.
fn infer_return_types(program: &ast::Program, sigs: &mut Sigs, structs: &Structs) {
    for _ in 0..=program.functions.len() {
        let mut changed = false;
        for f in &program.functions {
            if f.return_type.is_none() {
                let Ok(inferred) = check_function(f, sigs, structs, true, false) else {
                    continue;
                };
                let signature = sigs.get_mut(&f.name).unwrap();
                if signature.1 != inferred.return_type {
                    signature.1 = inferred.return_type;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

pub fn check(program: &ast::Program, lua_mode: bool) -> Result<TProgram, String> {
    let Prepared {
        structs,
        mut sigs,
        mut externs,
        ..
    } = prepare(program, lua_mode)?;
    infer_return_types(program, &mut sigs, &structs);

    let functions = program
        .functions
        .iter()
        .map(|f| {
            check_function(f, &sigs, &structs, false, lua_mode).map_err(|error| {
                match &f.source_file {
                    Some(file) => format!("{file}:{error}"),
                    None => error,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    add_builtin_externs(&functions, &mut externs);
    Ok(TProgram {
        structs,
        functions,
        externs,
    })
}

/// Every distinct function name `f` calls, direct or nested - the
/// dependency set `jit::promote`'s worklist walk needs, and (via
/// `add_builtin_externs`/`check_partitioned` below) the Tier-0-reachable
/// extern-collection and native/dynamic demotion logic also needs. Lives
/// here rather than in `jit.rs` because it's pure `TFunction`/`TExpr` AST
/// walking with no Cranelift dependency, and `check_partitioned` must stay
/// reachable from a `--no-default-features` (no `jit` feature) build - see
/// docs/features/milestones/u12-wasm-playground.md.
pub fn called_functions(f: &TFunction) -> HashSet<String> {
    fn walk_expr(e: &TExpr, out: &mut HashSet<String>) {
        match &e.kind {
            TExprKind::Call(name, args) => {
                out.insert(name.clone());
                args.iter().for_each(|a| walk_expr(a, out));
            }
            TExprKind::FunctionRef(name) => {
                out.insert(name.clone());
            }
            TExprKind::CallIndirect { callee, args } => {
                walk_expr(callee, out);
                args.iter().for_each(|a| walk_expr(a, out));
            }
            TExprKind::Truth(e)
            | TExprKind::Neg(e)
            | TExprKind::Not(e)
            | TExprKind::IntToFloat(e)
            | TExprKind::Len(e) => walk_expr(e, out),
            TExprKind::Arith(_, l, r)
            | TExprKind::Compare(_, l, r)
            | TExprKind::Logical(_, l, r)
            | TExprKind::Index(l, r) => {
                walk_expr(l, out);
                walk_expr(r, out);
            }
            TExprKind::NewArray { len, .. } => walk_expr(len, out),
            TExprKind::ArrayLiteral { values, .. } => {
                values.iter().for_each(|value| walk_expr(value, out))
            }
            TExprKind::ArrayMap { array, callback, .. } => {
                walk_expr(array, out);
                walk_expr(callback, out);
            }
            TExprKind::NewMap { .. } => {}
            TExprKind::MapLiteral { entries, .. } => {
                for (key, value) in entries {
                    walk_expr(key, out);
                    walk_expr(value, out);
                }
            }
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                walk_expr(map, out);
                walk_expr(cursor, out);
            }
            TExprKind::StructLiteral { fields, .. } => {
                fields.iter().for_each(|f| walk_expr(f, out))
            }
            TExprKind::Field { base, .. } => walk_expr(base, out),
            TExprKind::Box(inner) | TExprKind::Unbox(inner, _) => walk_expr(inner, out),
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::Local(_) => {}
        }
    }
    fn walk_stmts(stmts: &TBlock, out: &mut HashSet<String>) {
        for (_, s) in stmts {
            match s {
                TStmt::Break => {}
                TStmt::Local { value, .. } | TStmt::Assign { value, .. } => walk_expr(value, out),
                TStmt::AssignIndex {
                    array,
                    index,
                    value,
                } => {
                    walk_expr(array, out);
                    walk_expr(index, out);
                    walk_expr(value, out);
                }
                TStmt::AssignField { base, value, .. } => {
                    walk_expr(base, out);
                    walk_expr(value, out);
                }
                TStmt::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    walk_expr(cond, out);
                    walk_stmts(then_block, out);
                    walk_stmts(else_block, out);
                }
                TStmt::While { cond, body } => {
                    walk_expr(cond, out);
                    walk_stmts(body, out);
                }
                TStmt::NumericFor {
                    start,
                    stop,
                    step,
                    body,
                    ..
                } => {
                    walk_expr(start, out);
                    walk_expr(stop, out);
                    walk_expr(step, out);
                    walk_stmts(body, out);
                }
                TStmt::Return { value } => {
                    if let Some(v) = value {
                        walk_expr(v, out);
                    }
                }
            }
        }
    }
    let mut out = HashSet::new();
    walk_stmts(&f.body, &mut out);
    out
}

/// Appends the runtime-library externs (`strings.rs`'s helpers,
/// `__sol_any_is`) actually referenced by `functions`' bodies. Shared by
/// `check()` and `check_partitioned()`, which each call it over their own
/// final function list (the partitioned one is a subset).
fn add_builtin_externs(functions: &[TFunction], externs: &mut Vec<TExternFunction>) {
    let used: HashSet<_> = functions
        .iter()
        .flat_map(called_functions)
        .collect();
    for (name, params, ret) in crate::strings::signatures() {
        if used.contains(name) {
            externs.push(TExternFunction {
                name: name.into(),
                params,
                return_type: ret,
            });
        }
    }
    if used.contains("__sol_any_is") {
        externs.push(TExternFunction {
            name: "__sol_any_is".into(),
            params: vec![Type::Any, Type::I64],
            return_type: Type::Bool,
        });
    }
}

/// Per-function typed/dynamic split for `.lua` files. Unlike `check()`, a
/// function whose body needs the dynamic runtime doesn't fail the whole
/// program - it's set aside in `dynamic`, and any native candidate that
/// (directly or transitively, via `called_functions` above) calls one of
/// those set-aside names is itself demoted, so the returned `native`
/// program never contains a call to a dynamic-only function. A genuine
/// type error (anything not tagged `EDYNLUA`) still fails the whole
/// program, same as `check()`.
pub struct LuaPartition {
    pub native: TProgram,
    /// Every function name that must run through the bytecode interpreter
    /// instead of `native` - either its own body required it, or it was
    /// demoted for calling (transitively) something that did.
    pub dynamic: HashSet<String>,
    /// Functions whose bodies type-check but call an interpreted function.
    /// The unified dispatcher can execute these as specialized bytecode with
    /// semantic adapter slots; the legacy partition path treats them as
    /// dynamic for differential comparison.
    pub mixed: Vec<TFunction>,
    /// Functions whose own bodies require generic Lua semantics, excluding
    /// otherwise-typed callers added to `dynamic` by the legacy fixed point.
    pub interpreted: HashSet<String>,
}

pub fn check_partitioned(program: &ast::Program) -> Result<LuaPartition, String> {
    let Prepared {
        structs,
        mut sigs,
        mut externs,
        ..
    } = prepare(program, true)?;
    infer_return_types(program, &mut sigs, &structs);

    let mut native: HashMap<String, TFunction> = HashMap::new();
    let mut dynamic: HashSet<String> = HashSet::new();
    for f in &program.functions {
        match check_function(f, &sigs, &structs, false, true) {
            Ok(tf) => {
                native.insert(f.name.clone(), tf);
            }
            Err(error) if requires_dynamic_runtime(&error) => {
                dynamic.insert(f.name.clone());
            }
            Err(error) => {
                return Err(match &f.source_file {
                    Some(file) => format!("{file}:{error}"),
                    None => error,
                });
            }
        }
    }

    let interpreted = dynamic.clone();
    let mut mixed = Vec::new();
    loop {
        let demoted: Vec<String> = native
            .iter()
            .filter(|(_, tf)| {
                called_functions(tf)
                    .iter()
                    .any(|callee| dynamic.contains(callee))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if demoted.is_empty() {
            break;
        }
        for name in demoted {
            mixed.push(
                native
                    .remove(&name)
                    .expect("demoted function came from the native candidate map"),
            );
            dynamic.insert(name);
        }
    }

    let functions: Vec<TFunction> = native.into_values().collect();
    add_builtin_externs(&functions, &mut externs);
    Ok(LuaPartition {
        native: TProgram {
            structs,
            functions,
            externs,
        },
        dynamic,
        mixed,
        interpreted,
    })
}

fn lower_type(
    t: &ast::TypeName,
    struct_names: &HashSet<String>,
    line: u32,
) -> Result<Type, String> {
    Ok(match t {
        ast::TypeName::I64 => Type::I64,
        ast::TypeName::F64 => Type::F64,
        ast::TypeName::Bool => Type::Bool,
        ast::TypeName::Nil => Type::Nil,
        ast::TypeName::String => Type::String,
        ast::TypeName::Array(inner) => {
            Type::Array(Box::new(lower_type(inner, struct_names, line)?))
        }
        ast::TypeName::Map(key, value) => {
            let key = lower_type(key, struct_names, line)?;
            let value = lower_type(value, struct_names, line)?;
            if key != Type::I64 || !matches!(value, Type::I64 | Type::F64 | Type::Bool) {
                return Err(format!("line {line}: this M10 slice supports i64 map keys and scalar i64/f64/bool values; pointer-bearing map entries require precise GC layouts"));
            }
            Type::Map(Box::new(key), Box::new(value))
        }
        ast::TypeName::Function {
            params,
            return_type,
        } => Type::Function {
            params: params
                .iter()
                .map(|param| lower_type(param, struct_names, line))
                .collect::<Result<Vec<_>, _>>()?,
            return_type: Box::new(lower_type(return_type, struct_names, line)?),
        },
        ast::TypeName::Struct(name) => {
            if !struct_names.contains(name) {
                return Err(format!("line {line}: unknown type '{name}'"));
            }
            Type::Struct(name.clone())
        }
        ast::TypeName::Any => Type::Any,
    })
}

type Sigs = HashMap<String, (Vec<Type>, Type)>;
type Structs = HashMap<String, StructLayout>;

struct Checker<'a> {
    sigs: &'a Sigs,
    structs: &'a Structs,
    struct_names: HashSet<String>,
    scopes: Vec<HashMap<String, (LocalId, Type)>>,
    next_local: LocalId,
    local_names: Vec<String>,
    return_type: Type,
    constants: HashSet<LocalId>,
    loop_depth: usize,
    infer_return: bool,
    returns: Vec<Type>,
    /// Whether this program is `.lua` source. In `.lua` mode, calling a name
    /// with no known signature is treated the same as the other `EDYNLUA`
    /// triggers (it might be a dynamic-only builtin like `os.time` that
    /// typeck has no static signature for) rather than a hard type error,
    /// since `.lua` has a dynamic-runtime fallback to catch it at run time.
    /// `.sol` has no such fallback, so an unknown function there must stay a
    /// genuine, hard `ETYPE001` error.
    lua_mode: bool,
}

impl<'a> Checker<'a> {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    fn declare(&mut self, name: &str, ty: Type) -> LocalId {
        let id = self.next_local;
        self.next_local += 1;
        self.local_names.push(name.to_string());
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), (id, ty));
        id
    }
    fn resolve(&self, name: &str) -> Option<(LocalId, Type)> {
        self.scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }
}

fn check_function(
    f: &ast::Function,
    sigs: &Sigs,
    structs: &Structs,
    infer_return: bool,
    lua_mode: bool,
) -> Result<TFunction, String> {
    let (param_types, return_type) = sigs.get(&f.name).unwrap().clone();
    let struct_names = structs.keys().cloned().collect();
    let mut checker = Checker {
        sigs,
        structs,
        struct_names,
        scopes: vec![HashMap::new()],
        next_local: 0,
        local_names: Vec::new(),
        return_type: return_type.clone(),
        constants: HashSet::new(),
        loop_depth: 0,
        infer_return,
        returns: vec![],
        lua_mode,
    };

    let mut params = Vec::with_capacity(f.params.len());
    for ((name, _), ty) in f.params.iter().zip(param_types.iter()) {
        let id = checker.declare(name, ty.clone());
        params.push((id, ty.clone()));
    }

    let mut source_body = f.body.clone();
    fn returns(block: &[ast::Stmt]) -> bool {
        block.iter().any(|s| match s {
            ast::Stmt::Return { .. } => true,
            ast::Stmt::Block(b) => returns(b),
            ast::Stmt::If {
                then_block,
                else_block: Some(b),
                ..
            } => returns(then_block) && returns(b),
            _ => false,
        })
    }
    if !returns(&source_body) {
        source_body.push(ast::Stmt::Return {
            value: None,
            line: f.line,
        });
    }
    let body = check_block(&mut checker, &source_body)?;
    let return_type = if infer_return {
        checker.returns.iter().cloned().reduce(|a,b| {
            if a == b { a } else { numeric_join(&a,&b).unwrap_or(Type::Any) }
        }).ok_or_else(|| format!("function '{}' needs a return value; implicit nil results are not implemented yet", f.name))?
    } else {
        return_type
    };
    Ok(TFunction {
        name: f.name.clone(),
        source_file: f.source_file.clone(),
        source_line: f.line,
        source_span: f.source_span,
        params,
        return_type,
        body,
        local_count: checker.next_local,
        local_names: checker.local_names,
    })
}

/// Every `ast::Stmt` variant carries its own source line - either directly
/// (the common case) or via a nested `Function`/`Expr`/`Block` that does.
/// `Block(Block)` (a Lua `do ... end`) has no line of its own; it takes its
/// first inner statement's line, or `0` for an empty block (an empty block
/// compiles to no bytecode at all, so this never needs a breakpoint slot -
/// `bccompile.rs`'s per-instruction SourceMap only ever sees lines that this
/// function actually reports for a statement that emits bytecode).
fn ast_stmt_line(stmt: &ast::Stmt) -> u32 {
    match stmt {
        ast::Stmt::Global { line, .. }
        | ast::Stmt::Label { line, .. }
        | ast::Stmt::Goto { line, .. }
        | ast::Stmt::MultiLocal { line, .. }
        | ast::Stmt::MultiAssign { line, .. }
        | ast::Stmt::Repeat { line, .. }
        | ast::Stmt::Break { line }
        | ast::Stmt::Local { line, .. }
        | ast::Stmt::Assign { line, .. }
        | ast::Stmt::If { line, .. }
        | ast::Stmt::While { line, .. }
        | ast::Stmt::NumericFor { line, .. }
        | ast::Stmt::GenericFor { line, .. }
        | ast::Stmt::Return { line, .. }
        | ast::Stmt::MultiReturn { line, .. } => *line,
        ast::Stmt::GlobalFunction(f) | ast::Stmt::LocalFunction(f) => f.line,
        ast::Stmt::Expr(e) => e.line,
        ast::Stmt::Block(body) => body.first().map(ast_stmt_line).unwrap_or(0),
    }
}

fn check_block(checker: &mut Checker, block: &ast::Block) -> Result<TBlock, String> {
    checker.push_scope();
    let result = block
        .iter()
        .map(|s| Ok((ast_stmt_line(s), check_stmt(checker, s)?)))
        .collect::<Result<TBlock, String>>();
    checker.pop_scope();
    result
}

fn positive_narrowing(
    checker: &mut Checker,
    condition: &ast::Expr,
    line: u32,
) -> Result<TBlock, String> {
    let ast::ExprKind::TypeTest(value, target) = &condition.kind else {
        return Ok(Vec::new());
    };
    let ast::ExprKind::Name(name) = &value.kind else {
        return Ok(Vec::new());
    };
    let Some((source_id, Type::Any)) = checker.resolve(name) else {
        return Ok(Vec::new());
    };
    let target = lower_type(target, &checker.struct_names, line)?;
    let narrowed_id = checker.declare(name, target.clone());
    Ok(vec![(line, TStmt::Local {
        id: narrowed_id,
        value: TExpr {
            kind: TExprKind::Unbox(
                Box::new(TExpr {
                    kind: TExprKind::Local(source_id),
                    ty: Type::Any,
                }),
                target.clone(),
            ),
            ty: target,
        },
    })])
}

/// Lower an `if` while preserving short-circuiting and making positive type
/// tests visible on the right side of an `and`. `A and B` becomes nested typed
/// branches, so evaluating `B` may use the local narrowed by `A` without
/// evaluating either side twice.
fn check_conditional(
    checker: &mut Checker,
    condition: &ast::Expr,
    then_block: &ast::Block,
    else_block: TBlock,
    line: u32,
) -> Result<TStmt, String> {
    if let ast::ExprKind::Binary(BinaryOp::And, left, right) = &condition.kind {
        let typed_left = truth(check_expr(checker, left)?);
        checker.push_scope();
        let mut narrowed = positive_narrowing(checker, left, line)?;
        narrowed.push((line, check_conditional(
            checker,
            right,
            then_block,
            else_block.clone(),
            line,
        )?));
        checker.pop_scope();
        return Ok(TStmt::If {
            cond: typed_left,
            then_block: narrowed,
            else_block,
        });
    }

    let typed_cond = truth(check_expr(checker, condition)?);
    checker.push_scope();
    let mut typed_then = positive_narrowing(checker, condition, line)?;
    typed_then.extend(block_in_current_scope(checker, then_block)?);
    checker.pop_scope();
    Ok(TStmt::If {
        cond: typed_cond,
        then_block: typed_then,
        else_block,
    })
}

/// The single point every assignment/arg/return flows through: inserts
/// `i64->f64` widening and `any` box/unbox. Any other mismatch is an error.
fn coerce(expr: TExpr, target: &Type, line: u32) -> Result<TExpr, String> {
    if &expr.ty == target {
        return Ok(expr);
    }
    if expr.ty == Type::I64 && *target == Type::F64 {
        return Ok(TExpr {
            kind: TExprKind::IntToFloat(Box::new(expr)),
            ty: Type::F64,
        });
    }
    // Concrete -> `any`: scalar bits or a reference identity become the
    // payload of the two-word box.
    if *target == Type::Any {
        return Ok(TExpr {
            kind: TExprKind::Box(Box::new(expr)),
            ty: Type::Any,
        });
    }
    // `any` -> concrete: unbox it, checked at runtime (traps on mismatch).
    if expr.ty == Type::Any {
        return Ok(TExpr {
            kind: TExprKind::Unbox(Box::new(expr), target.clone()),
            ty: target.clone(),
        });
    }
    Err(format!(
        "line {line}: expected type {target}, found {}",
        expr.ty
    ))
}

fn sequence(body: TBlock) -> TStmt {
    TStmt::If {
        cond: TExpr {
            kind: TExprKind::BoolLit(true),
            ty: Type::Bool,
        },
        then_block: body,
        else_block: vec![],
    }
}

fn materialize_values(
    checker: &mut Checker,
    values: &[ast::Expr],
    body: &mut TBlock,
) -> Result<Vec<TExpr>, String> {
    values
        .iter()
        .map(|e| {
            let value = check_expr(checker, e)?;
            let ty = value.ty.clone();
            let id = checker.declare("$value", ty.clone());
            body.push((e.line, TStmt::Local { id, value }));
            Ok(TExpr {
                kind: TExprKind::Local(id),
                ty,
            })
        })
        .collect()
}
fn materialize_ast(
    checker: &mut Checker,
    e: &ast::Expr,
    body: &mut TBlock,
) -> Result<ast::Expr, String> {
    let value = check_expr(checker, e)?;
    let name = format!("$temp{}", checker.next_local);
    let id = checker.declare(&name, value.ty.clone());
    body.push((e.line, TStmt::Local { id, value }));
    Ok(ast::Expr {
        kind: ast::ExprKind::Name(name),
        line: e.line,
    })
}

fn check_stmt(checker: &mut Checker, stmt: &ast::Stmt) -> Result<TStmt, String> {
    match stmt {
        ast::Stmt::Global { line, .. }
        | ast::Stmt::GlobalFunction(ast::Function { line, .. })
        | ast::Stmt::LocalFunction(ast::Function { line, .. })
        | ast::Stmt::Label { line, .. }
        | ast::Stmt::Goto { line, .. }
        | ast::Stmt::MultiReturn { line, .. } => {
            Err(format!(
                "line {line}: dynamic Lua function/global syntax is parsed, but requires the M13 closure and _ENV runtime [{DYNAMIC_RUNTIME_CODE}]"
            ))
        }
        ast::Stmt::MultiLocal { names, values, line } => {
            if let Some((name,_,_,_)) = names.iter().find(|(_,_,_,close)| *close) {
                return Err(format!(
                    "line {line}: '{name}' has <close>, which is parsed, but requires the dynamic Lua runtime's scope-exit closing semantics [{DYNAMIC_RUNTIME_CODE}]"
                ));
            }
            let mut body: TBlock=vec![];
            let values=materialize_values(checker,values,&mut body)?;
            for (i,(name,ty,constant,_)) in names.iter().enumerate() {
                let value=values.get(i).cloned().unwrap_or(TExpr{kind:TExprKind::NilLit,ty:Type::Nil});
                let ty=match ty {Some(t)=>lower_type(t,&checker.struct_names,*line)?,None if value.ty==Type::Nil && !constant=>Type::Any,None=>value.ty.clone()};
                let value=coerce(value,&ty,*line)?;
                let id=checker.declare(name,ty);
                if *constant {checker.constants.insert(id);}
                body.push((*line, TStmt::Local{id,value}));
            }
            Ok(sequence(body))
        },
        ast::Stmt::MultiAssign { targets, values, line } => {
            let mut body: TBlock=vec![];
            // Freeze lvalue addresses before any assignment can change them.
            let mut frozen=vec![];
            for target in targets {
                frozen.push(match target {
                    ast::AssignTarget::Name(n)=>ast::AssignTarget::Name(n.clone()),
                    ast::AssignTarget::Index(base,index)=>ast::AssignTarget::Index(materialize_ast(checker,base,&mut body)?,materialize_ast(checker,index,&mut body)?),
                    ast::AssignTarget::Field(base,field)=>ast::AssignTarget::Field(materialize_ast(checker,base,&mut body)?,field.clone()),
                });
            }
            let mut temps=vec![];
            for value in values {temps.push(materialize_ast(checker,value,&mut body)?);}
            for (i,target) in frozen.into_iter().enumerate() {
                let value=temps.get(i).cloned().unwrap_or(ast::Expr{kind:ast::ExprKind::NilLit,line:*line});
                body.push((*line, check_stmt(checker,&ast::Stmt::Assign{target,value,line:*line})?));
            }
            Ok(sequence(body))
        },
        ast::Stmt::Block(body) => Ok(TStmt::If {
            cond: TExpr { kind: TExprKind::BoolLit(true), ty: Type::Bool },
            then_block: check_block(checker, body)?, else_block: vec![],
        }),
        ast::Stmt::Break { line } => {
            if checker.loop_depth == 0 { return Err(format!("line {line}: break outside a loop")); }
            Ok(TStmt::Break)
        },
        ast::Stmt::Repeat { body, cond, line } => {
            checker.push_scope(); checker.loop_depth += 1;
            let mut body = block_in_current_scope(checker, body)?;
            let cond = truth(check_expr(checker, cond)?);
            body.push((*line, TStmt::If { cond, then_block: vec![(*line, TStmt::Break)], else_block: vec![] }));
            checker.loop_depth -= 1; checker.pop_scope();
            Ok(TStmt::While { cond: TExpr { kind: TExprKind::BoolLit(true), ty: Type::Bool }, body })
        },
        ast::Stmt::Expr(expr) => {
            let value = check_expr(checker, expr)?;
            let id = checker.declare("$discard", value.ty.clone());
            Ok(TStmt::Local { id, value })
        },
        ast::Stmt::Local { name, ty, constant, close, value, line } => {
            if *close {
                return Err(format!(
                    "line {line}: '{name}' has <close>, which is parsed, but requires the dynamic Lua runtime's scope-exit closing semantics [{DYNAMIC_RUNTIME_CODE}]"
                ));
            }
            let value = match ty {
                Some(t) => {
                    let expected = lower_type(t, &checker.struct_names, *line)?;
                    let value = check_expr_expected(checker, value, &expected)?;
                    coerce(value, &expected, *line)?
                }
                None => {
                    let value = check_expr(checker, value)?;
                    if value.ty == Type::Nil && !*constant { coerce(value, &Type::Any, *line)? } else { value }
                }
            };
            let declared_ty = value.ty.clone();
            let id = checker.declare(name, declared_ty);
            if *constant { checker.constants.insert(id); }
            Ok(TStmt::Local { id, value })
        }
        ast::Stmt::Assign { target, value, line } => match target {
            ast::AssignTarget::Name(name) => {
                let (id, ty) = checker.resolve(name).ok_or_else(|| {
                    // In `.lua`, assigning to a name that's neither a local
                    // nor a known top-level function creates an implicit
                    // global (ordinary Lua) rather than being an error -
                    // that's dynamic-only behavior with no static home in
                    // `sigs`, so defer to the dynamic runtime. `.sol` has no
                    // implicit globals, so this stays a hard error there.
                    if checker.lua_mode {
                        format!("line {line}: undefined variable '{name}' [{DYNAMIC_RUNTIME_CODE}]")
                    } else {
                        format!("line {line}: undefined variable '{name}'")
                    }
                })?;
                if checker.constants.contains(&id) { return Err(format!("line {line}: cannot assign to read-only variable '{name}'")); }
                let value = check_expr(checker, value)?;
                let value = coerce(value, &ty, *line)?;
                Ok(TStmt::Assign { id, value })
            }
            ast::AssignTarget::Index(array_expr, index_expr) => {
                let array = check_expr(checker, array_expr)?;
                let (index_ty, elem_ty) = match &array.ty {
                    Type::Array(inner) => (Type::I64, (**inner).clone()),
                    Type::Map(key, value) => ((**key).clone(), (**value).clone()),
                    other => return Err(non_array_index_error(checker, *line, other)),
                };
                let index = check_expr(checker, index_expr)?;
                let index = coerce(index, &index_ty, *line)?;
                let value = check_expr(checker, value)?;
                let value = coerce(value, &elem_ty, *line)?;
                Ok(TStmt::AssignIndex { array, index, value })
            }
            ast::AssignTarget::Field(base_expr, field_name) => {
                let base = check_expr(checker, base_expr)?;
                let (field_index, field_ty) = resolve_field(checker, &base.ty, field_name, *line)?;
                let value = check_expr(checker, value)?;
                let value = coerce(value, &field_ty, *line)?;
                Ok(TStmt::AssignField { base, field_index, value })
            }
        },
        ast::Stmt::If { cond, then_block, else_block, line } => {
            let else_block = match else_block {
                Some(b) => check_block(checker, b)?,
                None => Vec::new(),
            };
            check_conditional(checker, cond, then_block, else_block, *line)
        }
        ast::Stmt::While { cond, body, line: _ } => {
            let cond = check_expr(checker, cond)?;
            let cond = truth(cond);
            checker.loop_depth += 1;
            let body = check_block(checker, body)?;
            checker.loop_depth -= 1;
            Ok(TStmt::While { cond, body })
        }
        ast::Stmt::NumericFor { var, start, stop, step, body, line } => {
            let start = coerce(check_expr(checker, start)?, &Type::I64, *line)?;
            let stop = coerce(check_expr(checker, stop)?, &Type::I64, *line)?;
            let step = match step {
                Some(e) => coerce(check_expr(checker, e)?, &Type::I64, *line)?,
                None => TExpr { kind: TExprKind::IntLit(1), ty: Type::I64 },
            };
            checker.push_scope();
            let id = checker.declare(var, Type::I64);
            checker.constants.insert(id);
            let stop_id = checker.declare("$for_stop", Type::I64);
            let step_id = checker.declare("$for_step", Type::I64);
            checker.loop_depth += 1;
            let body = block_in_current_scope(checker, body)?;
            checker.loop_depth -= 1;
            checker.pop_scope();
            Ok(TStmt::NumericFor { id, stop_id, step_id, start, stop, step, body })
        }
        ast::Stmt::GenericFor { vars, iterators, body, line } => {
            check_generic_for(checker, vars, iterators, body, *line)
        }
        ast::Stmt::Return { value, line } => {
            let return_type = checker.return_type.clone();
            let value = match value {
                Some(e) => {
                    let e = check_expr(checker, e)?;
                    checker.returns.push(e.ty.clone());
                    Some(if checker.infer_return { e } else { coerce(e, &return_type, *line)? })
                },
                None => {
                    checker.returns.push(Type::Nil);
                    let nil=TExpr {kind:TExprKind::NilLit,ty:Type::Nil};
                    Some(if checker.infer_return {nil} else {coerce(nil,&return_type,*line)?})
                },
            };
            Ok(TStmt::Return { value })
        }
    }
}

fn check_generic_for(
    checker: &mut Checker,
    vars: &[String],
    iterators: &[ast::Expr],
    source_body: &ast::Block,
    line: u32,
) -> Result<TStmt, String> {
    if vars.is_empty() || vars.len() > 2 || iterators.len() != 1 {
        return Err(format!(
            "line {line}: typed generic for expects one or two variables and one iterator"
        ));
    }
    let ast::ExprKind::Call(iterator, args) = &iterators[0].kind else {
        return Err(format!(
            "line {line}: typed generic for supports ipairs(array) and pairs(map)"
        ));
    };
    if args.len() != 1 || (iterator != "ipairs" && iterator != "pairs") {
        return Err(format!(
            "line {line}: typed generic for supports ipairs(array) and pairs(map)"
        ));
    }
    let collection = check_expr(checker, &args[0])?;
    let collection_ty = collection.ty.clone();
    checker.push_scope();
    let collection_id = checker.declare("$iterator_collection", collection_ty.clone());
    let collection_ref = || TExpr {
        kind: TExprKind::Local(collection_id),
        ty: collection_ty.clone(),
    };
    let mut statements: TBlock = vec![(line, TStmt::Local {
        id: collection_id,
        value: collection,
    })];

    match (iterator.as_str(), &collection_ty) {
        ("ipairs", Type::Array(element)) => {
            let index_id = checker.declare(&vars[0], Type::I64);
            checker.constants.insert(index_id);
            let value_id = vars.get(1).map(|name| {
                let id = checker.declare(name, (**element).clone());
                checker.constants.insert(id);
                id
            });
            let stop_id = checker.declare("$ipairs_stop", Type::I64);
            let step_id = checker.declare("$ipairs_step", Type::I64);
            checker.loop_depth += 1;
            let mut body = block_in_current_scope(checker, source_body)?;
            checker.loop_depth -= 1;
            if let Some(value_id) = value_id {
                body.insert(
                    0,
                    (line, TStmt::Local {
                        id: value_id,
                        value: TExpr {
                            ty: (**element).clone(),
                            kind: TExprKind::Index(
                                Box::new(collection_ref()),
                                Box::new(TExpr {
                                    kind: TExprKind::Local(index_id),
                                    ty: Type::I64,
                                }),
                            ),
                        },
                    }),
                );
            }
            let length = TExpr {
                kind: TExprKind::Len(Box::new(collection_ref())),
                ty: Type::I64,
            };
            statements.push((line, TStmt::NumericFor {
                id: index_id,
                stop_id,
                step_id,
                start: TExpr {
                    kind: TExprKind::IntLit(0),
                    ty: Type::I64,
                },
                stop: TExpr {
                    kind: TExprKind::Arith(
                        BinaryOp::Sub,
                        Box::new(length),
                        Box::new(TExpr {
                            kind: TExprKind::IntLit(1),
                            ty: Type::I64,
                        }),
                    ),
                    ty: Type::I64,
                },
                step: TExpr {
                    kind: TExprKind::IntLit(1),
                    ty: Type::I64,
                },
                body,
            }));
        }
        ("pairs", Type::Map(key, value)) => {
            let key_ty = (**key).clone();
            let value_ty = (**value).clone();
            let cursor_id = checker.declare("$pairs_cursor", Type::I64);
            let key_id = checker.declare(&vars[0], key_ty.clone());
            checker.constants.insert(key_id);
            let value_id = if let Some(name) = vars.get(1) {
                let id = checker.declare(name, value_ty.clone());
                checker.constants.insert(id);
                Some(id)
            } else {
                None
            };
            statements.push((line, TStmt::Local {
                id: cursor_id,
                value: TExpr {
                    kind: TExprKind::IntLit(0),
                    ty: Type::I64,
                },
            }));
            statements.push((line, TStmt::Local {
                id: key_id,
                value: TExpr {
                    kind: TExprKind::IntLit(0),
                    ty: key_ty.clone(),
                },
            }));
            if let Some(value_id) = value_id {
                let zero = match value_ty {
                    Type::F64 => TExprKind::FloatLit(0.0),
                    Type::Bool => TExprKind::BoolLit(false),
                    _ => TExprKind::IntLit(0),
                };
                statements.push((line, TStmt::Local {
                    id: value_id,
                    value: TExpr {
                        kind: zero,
                        ty: value_ty.clone(),
                    },
                }));
            }
            checker.loop_depth += 1;
            let body = block_in_current_scope(checker, source_body)?;
            checker.loop_depth -= 1;
            let cursor = || TExpr {
                kind: TExprKind::Local(cursor_id),
                ty: Type::I64,
            };
            let mut loop_body: TBlock = vec![
                (line, TStmt::Assign {
                    id: cursor_id,
                    value: TExpr {
                        kind: TExprKind::MapNext {
                            map: Box::new(collection_ref()),
                            cursor: Box::new(cursor()),
                        },
                        ty: Type::I64,
                    },
                }),
                (line, TStmt::If {
                    cond: TExpr {
                        kind: TExprKind::Compare(
                            BinaryOp::Eq,
                            Box::new(cursor()),
                            Box::new(TExpr {
                                kind: TExprKind::IntLit(0),
                                ty: Type::I64,
                            }),
                        ),
                        ty: Type::Bool,
                    },
                    then_block: vec![(line, TStmt::Break)],
                    else_block: vec![],
                }),
                (line, TStmt::Assign {
                    id: key_id,
                    value: TExpr {
                        kind: TExprKind::MapKey {
                            map: Box::new(collection_ref()),
                            cursor: Box::new(cursor()),
                        },
                        ty: key_ty,
                    },
                }),
            ];
            if let Some(value_id) = value_id {
                loop_body.push((line, TStmt::Assign {
                    id: value_id,
                    value: TExpr {
                        kind: TExprKind::MapValue {
                            map: Box::new(collection_ref()),
                            cursor: Box::new(cursor()),
                        },
                        ty: value_ty,
                    },
                }));
            }
            loop_body.extend(body);
            statements.push((line, TStmt::While {
                cond: TExpr {
                    kind: TExprKind::BoolLit(true),
                    ty: Type::Bool,
                },
                body: loop_body,
            }));
        }
        ("ipairs", other) => {
            // `.lua` tables are usually `any`-typed statically (their real
            // shape is only known dynamically), so `ipairs`/`pairs` over an
            // `any` value is completely ordinary Lua, not a type error -
            // defer to the dynamic runtime, which iterates the real table.
            // `.sol`'s typed `Array<T>`/`Map<K, V>` have no such fallback.
            if checker.lua_mode {
                return Err(format!(
                    "line {line}: ipairs expects Array<T>, found {other} [{DYNAMIC_RUNTIME_CODE}]"
                ));
            }
            return Err(format!(
                "line {line}: ipairs expects Array<T>, found {other}"
            ));
        }
        ("pairs", other) => {
            if checker.lua_mode {
                return Err(format!(
                    "line {line}: pairs expects Map<K, V>, found {other} [{DYNAMIC_RUNTIME_CODE}]"
                ));
            }
            return Err(format!(
                "line {line}: pairs expects Map<K, V>, found {other}"
            ));
        }
        _ => unreachable!(),
    }
    checker.pop_scope();
    Ok(sequence(statements))
}

fn check_expr_expected(
    checker: &mut Checker,
    expr: &ast::Expr,
    expected: &Type,
) -> Result<TExpr, String> {
    if let (ast::ExprKind::Table(fields), Type::Array(element)) = (&expr.kind, expected) {
        let mut values = Vec::with_capacity(fields.len());
        for field in fields {
            let ast::TableField::Value(value) = field else {
                return Err(format!(
                    "line {}: a typed array literal accepts only positional values",
                    expr.line
                ));
            };
            values.push(coerce(check_expr(checker, value)?, element, expr.line)?);
        }
        return Ok(TExpr {
            kind: TExprKind::ArrayLiteral {
                elem: (**element).clone(),
                values,
            },
            ty: expected.clone(),
        });
    }
    if let (ast::ExprKind::Table(fields), Type::Map(key, value)) = (&expr.kind, expected) {
        let mut entries = Vec::with_capacity(fields.len());
        for field in fields {
            let ast::TableField::Key(entry_key, entry_value) = field else {
                return Err(format!(
                    "line {}: a typed map literal requires [key] = value entries",
                    expr.line
                ));
            };
            let entry_key = coerce(check_expr(checker, entry_key)?, key, expr.line)?;
            let entry_value = coerce(check_expr(checker, entry_value)?, value, expr.line)?;
            entries.push((entry_key, entry_value));
        }
        return Ok(TExpr {
            kind: TExprKind::MapLiteral {
                key: (**key).clone(),
                value: (**value).clone(),
                entries,
            },
            ty: expected.clone(),
        });
    }
    if let (ast::ExprKind::Table(fields), Type::Struct(name)) = (&expr.kind, expected) {
        let mut named = Vec::with_capacity(fields.len());
        for field in fields {
            match field {
                ast::TableField::Named(field, value) => named.push((field.clone(), value.clone())),
                _ => {
                    return Err(format!(
                        "line {}: a typed record literal accepts only named fields",
                        expr.line
                    ))
                }
            }
        }
        return check_expr(
            checker,
            &ast::Expr {
                kind: ast::ExprKind::StructLiteral(name.clone(), named),
                line: expr.line,
            },
        );
    }
    check_expr(checker, expr)
}

/// Like `check_block`, but reuses the caller's scope (for a `for` body,
/// sharing the scope its loop variable was already declared in).
fn block_in_current_scope(checker: &mut Checker, block: &ast::Block) -> Result<TBlock, String> {
    block
        .iter()
        .map(|s| Ok((ast_stmt_line(s), check_stmt(checker, s)?)))
        .collect()
}

fn check_expr(checker: &mut Checker, expr: &ast::Expr) -> Result<TExpr, String> {
    let line = expr.line;
    match &expr.kind {
        ast::ExprKind::Table(_)
        | ast::ExprKind::Function(_)
        | ast::ExprKind::Vararg
        | ast::ExprKind::CallExpr(_, _)
        | ast::ExprKind::MethodCall(_, _, _) => Err(format!(
            "line {line}: dynamic Lua table/function syntax is parsed, but requires the M13 dynamic runtime [{DYNAMIC_RUNTIME_CODE}]"
        )),
        ast::ExprKind::StringLit(bytes) => Ok(TExpr {kind:TExprKind::StringLit(bytes.clone()),ty:Type::String}),
        ast::ExprKind::NilLit => Ok(TExpr { kind: TExprKind::NilLit, ty: Type::Nil }),
        ast::ExprKind::IntLit(n) => Ok(TExpr { kind: TExprKind::IntLit(*n), ty: Type::I64 }),
        ast::ExprKind::FloatLit(n) => Ok(TExpr { kind: TExprKind::FloatLit(*n), ty: Type::F64 }),
        ast::ExprKind::BoolLit(b) => Ok(TExpr { kind: TExprKind::BoolLit(*b), ty: Type::Bool }),
        ast::ExprKind::Name(name) => {
            if let Some((id, ty)) = checker.resolve(name) {
                return Ok(TExpr { kind: TExprKind::Local(id), ty });
            }
            let (params, return_type) = checker
                .sigs
                .get(name)
                .cloned()
                .ok_or_else(|| {
                    // Same reasoning as the assignment-target case above:
                    // reading a name that's neither a local nor a known
                    // top-level function is how ordinary Lua reads a
                    // builtin (`tonumber`, `print`, ...) or any other
                    // implicit global - genuinely dynamic, not an error.
                    if checker.lua_mode {
                        format!("line {line}: undefined variable '{name}' [{DYNAMIC_RUNTIME_CODE}]")
                    } else {
                        format!("line {line}: undefined variable '{name}'")
                    }
                })?;
            Ok(TExpr {
                kind: TExprKind::FunctionRef(name.clone()),
                ty: Type::Function { params, return_type: Box::new(return_type) },
            })
        }
        ast::ExprKind::Unary(UnaryOp::Neg, operand) => {
            let operand = check_expr(checker, operand)?;
            match operand.ty {
                Type::I64 | Type::F64 | Type::Any => {
                    let ty = operand.ty.clone();
                    Ok(TExpr { kind: TExprKind::Neg(Box::new(operand)), ty })
                }
                other => Err(format!("line {line}: cannot negate {other}")),
            }
        }
        ast::ExprKind::Unary(UnaryOp::BitNot, operand) => {
            let operand = coerce(check_expr(checker, operand)?, &Type::I64, line)?;
            Ok(TExpr { kind: TExprKind::Arith(BinaryOp::BitXor, Box::new(operand), Box::new(TExpr { kind: TExprKind::IntLit(-1), ty: Type::I64 })), ty: Type::I64 })
        }
        ast::ExprKind::Unary(UnaryOp::Not, operand) => {
            let operand = check_expr(checker, operand)?;
            let operand = truth(operand);
            Ok(TExpr { kind: TExprKind::Not(Box::new(operand)), ty: Type::Bool })
        }
        ast::ExprKind::Len(operand) => {
            let operand = check_expr(checker, operand)?;
            match operand.ty {
                Type::Array(_) | Type::Map(_, _) | Type::String => Ok(TExpr { kind: TExprKind::Len(Box::new(operand)), ty: Type::I64 }),
                other => Err(format!("line {line}: cannot take the length of {other}")),
            }
        }
        ast::ExprKind::Index(array_expr, index_expr) => {
            let array = check_expr(checker, array_expr)?;
            let (index_ty, elem_ty) = match &array.ty {
                Type::Array(inner) => (Type::I64, (**inner).clone()),
                Type::Map(key, value) => ((**key).clone(), (**value).clone()),
                other => return Err(non_array_index_error(checker, line, other)),
            };
            let index = check_expr(checker, index_expr)?;
            let index = coerce(index, &index_ty, line)?;
            Ok(TExpr { kind: TExprKind::Index(Box::new(array), Box::new(index)), ty: elem_ty })
        }
        ast::ExprKind::Binary(op, l, r) => check_binary(checker, *op, l, r, line),
        ast::ExprKind::Call(name, args) => check_call(checker, name, args, line),
        ast::ExprKind::TypeTest(value, target) => {
            let value = check_expr(checker, value)?;
            if value.ty != Type::Any {
                return Err(format!(
                    "line {line}: 'is' requires an any value, found {}",
                    value.ty
                ));
            }
            let target = lower_type(target, &checker.struct_names, line)?;
            let tag = crate::value::tag_for(&target)
                .ok_or_else(|| format!("line {line}: cannot test for type {target} yet"))?;
            Ok(TExpr {
                kind: TExprKind::Call(
                    "__sol_any_is".into(),
                    vec![
                        value,
                        TExpr {
                            kind: TExprKind::IntLit(tag),
                            ty: Type::I64,
                        },
                    ],
                ),
                ty: Type::Bool,
            })
        }
        ast::ExprKind::Cast(value, target) => {
            let value = check_expr(checker, value)?;
            let target = lower_type(target, &checker.struct_names, line)?;
            if value.ty != Type::Any {
                return Err(format!(
                    "line {line}: checked 'as' cast requires an any value, found {}",
                    value.ty
                ));
            }
            coerce(value, &target, line)
        }
        ast::ExprKind::Field(base_expr, field_name) => {
            let base = check_expr(checker, base_expr)?;
            let (field_index, field_ty) = resolve_field(checker, &base.ty, field_name, line)?;
            Ok(TExpr { kind: TExprKind::Field { base: Box::new(base), field_index }, ty: field_ty })
        }
        ast::ExprKind::StructLiteral(name, given_fields) => {
            let layout = checker
                .structs
                .get(name)
                .ok_or_else(|| format!("line {line}: unknown struct '{name}'"))?
                .clone();
            if given_fields.len() != layout.fields.len() {
                return Err(format!(
                    "line {line}: struct '{name}' has {} field(s), but {} were given - every field must be initialized in M3",
                    layout.fields.len(),
                    given_fields.len()
                ));
            }
            // Reorder to the struct's declared field order (its byte layout).
            let mut ordered = Vec::with_capacity(layout.fields.len());
            for (fname, fty) in &layout.fields {
                let (_, given_expr) = given_fields
                    .iter()
                    .find(|(n, _)| n == fname)
                    .ok_or_else(|| format!("line {line}: struct '{name}' is missing field '{fname}'"))?;
                let checked = coerce(check_expr_expected(checker, given_expr, fty)?, fty, line)?;
                ordered.push(checked);
            }
            for (given_name, _) in given_fields {
                if layout.field_index(given_name).is_none() {
                    return Err(format!("line {line}: struct '{name}' has no field '{given_name}'"));
                }
            }
            Ok(TExpr { kind: TExprKind::StructLiteral { name: name.clone(), fields: ordered }, ty: Type::Struct(name.clone()) })
        }
        // The parser only wraps a multi-value-producing expression in
        // `Paren` (`(f())`, `(...)`) to mark truncation to one value; every
        // typed-Sol expression already produces exactly one `TExpr`, so the
        // parens themselves are a no-op here.
        ast::ExprKind::Paren(inner) => check_expr(checker, inner),
    }
}

/// Resolves `base_ty.field_name` to `(field_index, field_type)` for both reads and writes.
fn resolve_field(
    checker: &Checker,
    base_ty: &Type,
    field_name: &str,
    line: u32,
) -> Result<(usize, Type), String> {
    let Type::Struct(struct_name) = base_ty else {
        // `.lua` tables are usually `any`-typed statically, and
        // `t.field`/`t.field = v` on an `any` value is ordinary dynamic
        // table indexing, not a type error - defer to the dynamic runtime.
        // `.sol` has no dynamic fallback, so this stays a hard error there.
        if checker.lua_mode {
            return Err(format!(
                "line {line}: cannot access field '{field_name}' on non-struct type {base_ty} [{DYNAMIC_RUNTIME_CODE}]"
            ));
        }
        return Err(format!(
            "line {line}: cannot access field '{field_name}' on non-struct type {base_ty}"
        ));
    };
    let layout = checker
        .structs
        .get(struct_name)
        .expect("typeck only ever produces Type::Struct for a known struct");
    let field_index = layout.field_index(field_name).ok_or_else(|| {
        format!("line {line}: struct '{struct_name}' has no field '{field_name}'")
    })?;
    Ok((field_index, layout.fields[field_index].1.clone()))
}

/// `t[k]`/`t[k] = v` on a non-`Array`/`Map` type. In `.lua`, this base is
/// usually `any` (a table whose real shape is only known dynamically), so
/// it's ordinary dynamic table indexing, not a type error - defer to the
/// dynamic runtime. `.sol`'s typed `Array<T>`/`Map<K, V>` have no such
/// fallback, so this stays a hard error there.
fn non_array_index_error(checker: &Checker, line: u32, other: &Type) -> String {
    if checker.lua_mode {
        format!("line {line}: cannot index into non-array type {other} [{DYNAMIC_RUNTIME_CODE}]")
    } else {
        format!("line {line}: cannot index into non-array type {other}")
    }
}

fn truth(expr: TExpr) -> TExpr {
    if expr.ty == Type::Bool {
        expr
    } else {
        TExpr {
            kind: TExprKind::Truth(Box::new(expr)),
            ty: Type::Bool,
        }
    }
}

fn numeric_join(a: &Type, b: &Type) -> Option<Type> {
    match (a, b) {
        (Type::I64, Type::I64) => Some(Type::I64),
        (Type::F64, Type::F64) | (Type::I64, Type::F64) | (Type::F64, Type::I64) => Some(Type::F64),
        _ => None,
    }
}

fn check_binary(
    checker: &mut Checker,
    op: BinaryOp,
    l: &ast::Expr,
    r: &ast::Expr,
    line: u32,
) -> Result<TExpr, String> {
    use BinaryOp::*;
    let left = check_expr(checker, l)?;
    let right = check_expr(checker, r)?;
    if (left.ty == Type::Any || right.ty == Type::Any) && !matches!(op, And | Or) {
        let left = coerce(left, &Type::Any, line)?;
        let right = coerce(right, &Type::Any, line)?;
        return Ok(if matches!(op, Eq | NotEq | Lt | Le | Gt | Ge) {
            TExpr {
                kind: TExprKind::Compare(op, Box::new(left), Box::new(right)),
                ty: Type::Bool,
            }
        } else {
            TExpr {
                kind: TExprKind::Arith(op, Box::new(left), Box::new(right)),
                ty: Type::Any,
            }
        });
    }
    match op {
        Concat => {
            fn string(e: TExpr, line: u32) -> Result<TExpr, String> {
                if e.ty == Type::String {
                    return Ok(e);
                }
                let name = match e.ty {
                    Type::I64 => "__sol_string_i64",
                    Type::F64 => "__sol_string_f64",
                    _ => return Err(format!("line {line}: cannot concatenate {}", e.ty)),
                };
                Ok(TExpr {
                    kind: TExprKind::Call(name.into(), vec![e]),
                    ty: Type::String,
                })
            }
            Ok(TExpr {
                kind: TExprKind::Call(
                    "__sol_string_concat".into(),
                    vec![string(left, line)?, string(right, line)?],
                ),
                ty: Type::String,
            })
        }
        BitAnd | BitOr | BitXor | Shl | Shr => {
            let left = coerce(left, &Type::I64, line)?;
            let right = coerce(right, &Type::I64, line)?;
            Ok(TExpr {
                kind: TExprKind::Arith(op, Box::new(left), Box::new(right)),
                ty: Type::I64,
            })
        }
        Add | Sub | Mul | Div | FloorDiv | Mod | Pow => {
            let joined = numeric_join(&left.ty, &right.ty).ok_or_else(|| {
                format!(
                    "line {line}: cannot apply {op:?} to {} and {}",
                    left.ty, right.ty
                )
            })?;
            let joined = if matches!(op, Div | Pow) {
                Type::F64
            } else {
                joined
            };
            let left = coerce(left, &joined, line)?;
            let right = coerce(right, &joined, line)?;
            Ok(TExpr {
                kind: TExprKind::Arith(op, Box::new(left), Box::new(right)),
                ty: joined,
            })
        }
        Eq | NotEq => {
            let joined = if left.ty == right.ty
                && matches!(left.ty, Type::Bool | Type::Nil | Type::String)
            {
                left.ty.clone()
            } else {
                numeric_join(&left.ty, &right.ty).unwrap_or(Type::Any)
            };
            let left = coerce(left, &joined, line)?;
            let right = coerce(right, &joined, line)?;
            Ok(TExpr {
                kind: TExprKind::Compare(op, Box::new(left), Box::new(right)),
                ty: Type::Bool,
            })
        }
        Lt | Le | Gt | Ge => {
            let joined = if left.ty == Type::String && right.ty == Type::String {
                Type::String
            } else {
                numeric_join(&left.ty, &right.ty).ok_or_else(|| {
                    format!(
                        "line {line}: cannot order-compare {} and {}",
                        left.ty, right.ty
                    )
                })?
            };
            let left = coerce(left, &joined, line)?;
            let right = coerce(right, &joined, line)?;
            Ok(TExpr {
                kind: TExprKind::Compare(op, Box::new(left), Box::new(right)),
                ty: Type::Bool,
            })
        }
        And | Or => {
            let ty = if left.ty == right.ty {
                left.ty.clone()
            } else {
                Type::Any
            };
            let left = coerce(left, &ty, line)?;
            let right = coerce(right, &ty, line)?;
            Ok(TExpr {
                kind: TExprKind::Logical(op, Box::new(left), Box::new(right)),
                ty,
            })
        }
    }
}

fn check_call(
    checker: &mut Checker,
    name: &str,
    args: &[ast::Expr],
    line: u32,
) -> Result<TExpr, String> {
    if name == "map" {
        if args.len() != 2 {
            return Err(format!(
                "line {line}: generic map expects an array and a callback"
            ));
        }
        let array = check_expr(checker, &args[0])?;
        let callback = check_expr(checker, &args[1])?;
        // Monomorphize at this call site: `map` is generic over any scalar
        // `T` the runtime has a dedicated unboxed array representation for
        // (`I64`/`F64`, same set `new_array_i64`/`new_array_f64` cover),
        // instead of being hard-coded to `T = I64`.
        let elem = match &array.ty {
            Type::Array(elem) if matches!(**elem, Type::I64 | Type::F64) => (**elem).clone(),
            _ => {
                return Err(format!(
                    "line {line}: map expects an Array<i64> or Array<f64>, found {}",
                    array.ty
                ));
            }
        };
        let expected_callback = Type::Function {
            params: vec![elem.clone()],
            return_type: Box::new(elem.clone()),
        };
        if callback.ty != expected_callback {
            return Err(format!(
                "line {line}: this map call expects fn({elem}) -> {elem}, found {}",
                callback.ty
            ));
        }
        return Ok(TExpr {
            kind: TExprKind::ArrayMap {
                array: Box::new(array),
                callback: Box::new(callback),
                elem: elem.clone(),
            },
            ty: Type::Array(Box::new(elem)),
        });
    }
    if let Some((
        id,
        Type::Function {
            params,
            return_type,
        },
    )) = checker.resolve(name)
    {
        if args.len() != params.len() {
            return Err(format!(
                "line {line}: function value '{name}' expects {} argument(s), found {}",
                params.len(),
                args.len()
            ));
        }
        let mut checked_args = Vec::with_capacity(args.len());
        for (arg, expected) in args.iter().zip(params.iter()) {
            checked_args.push(coerce(check_expr(checker, arg)?, expected, line)?);
        }
        return Ok(TExpr {
            kind: TExprKind::CallIndirect {
                callee: Box::new(TExpr {
                    kind: TExprKind::Local(id),
                    ty: Type::Function {
                        params,
                        return_type: return_type.clone(),
                    },
                }),
                args: checked_args,
            },
            ty: *return_type,
        });
    }
    if !checker.sigs.contains_key(name) {
        if let Some((symbol, params, ret)) = crate::strings::signature(name) {
            let mut args = args.to_vec();
            let int = |n| ast::Expr {
                kind: ast::ExprKind::IntLit(n),
                line,
            };
            if name == "string.sub" && args.len() == 2 {
                args.push(int(-1));
            }
            if name == "string.rep" && args.len() == 2 {
                args.push(ast::Expr {
                    kind: ast::ExprKind::StringLit(vec![]),
                    line,
                });
            }
            if args.len() != params.len() {
                return Err(format!("line {line}: wrong argument count for '{name}'"));
            }
            let args = args
                .iter()
                .zip(params)
                .map(|(a, t)| coerce(check_expr(checker, a)?, &t, line))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(TExpr {
                kind: TExprKind::Call(symbol.into(), args),
                ty: ret,
            });
        }
    }
    if name == "new_array_i64" || name == "new_array_f64" {
        if args.len() != 1 {
            return Err(format!(
                "line {line}: '{name}' takes exactly one argument (the length)"
            ));
        }
        let len = coerce(check_expr(checker, &args[0])?, &Type::I64, line)?;
        let elem = if name == "new_array_i64" {
            Type::I64
        } else {
            Type::F64
        };
        return Ok(TExpr {
            ty: Type::Array(Box::new(elem.clone())),
            kind: TExprKind::NewArray {
                elem,
                len: Box::new(len),
            },
        });
    }

    let (param_types, return_type) = checker.sigs.get(name).cloned().ok_or_else(|| {
        if checker.lua_mode {
            format!("line {line}: unknown function '{name}' [{DYNAMIC_RUNTIME_CODE}]")
        } else {
            format!("line {line}: unknown function '{name}'")
        }
    })?;
    if args.len() != param_types.len() {
        return Err(format!(
            "line {line}: '{name}' expects {} argument(s), found {}",
            param_types.len(),
            args.len()
        ));
    }
    let mut checked_args = Vec::with_capacity(args.len());
    for (arg, expected) in args.iter().zip(param_types.iter()) {
        checked_args.push(coerce(check_expr(checker, arg)?, expected, line)?);
    }
    Ok(TExpr {
        kind: TExprKind::Call(name.to_string(), checked_args),
        ty: return_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn check_source(src: &str) -> Result<TProgram, String> {
        check(&parser::parse(lexer::lex(src).unwrap()).unwrap(), false)
    }

    #[test]
    fn local_without_annotation_infers_i64_for_an_integer_literal() {
        let prog = check_source("function main(): i64\n  local x = 10\n  return x\nend\n").unwrap();
        let TStmt::Local { value, .. } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert_eq!(value.ty, Type::I64);
    }

    #[test]
    fn local_without_annotation_infers_f64_for_a_float_literal() {
        let prog =
            check_source("function main(): f64\n  local y = 20.0\n  return y\nend\n").unwrap();
        let TStmt::Local { value, .. } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert_eq!(value.ty, Type::F64);
    }

    #[test]
    fn mixed_i64_f64_arithmetic_promotes_to_f64() {
        let prog = check_source(
            "function main(): f64\n  local x: i64 = 1\n  local y: f64 = 2.0\n  return x + y\nend\n",
        )
        .unwrap();
        let TStmt::Return { value: Some(v) } = &prog.functions[0].body.last().unwrap().1 else {
            panic!()
        };
        assert_eq!(v.ty, Type::F64);
    }

    #[test]
    fn undefined_variable_is_a_clear_error() {
        let err = check_source("function main(): i64\n  return nope\nend\n").unwrap_err();
        assert!(err.contains("undefined variable 'nope'"), "{err}");
    }

    #[test]
    fn a_function_without_a_return_type_infers_its_result() {
        let p = check_source("function main() return 42 end").unwrap();
        assert_eq!(p.functions[0].return_type, Type::I64);
    }

    #[test]
    fn recursive_calls_resolve_via_the_first_pass_signature_table() {
        // `fib` calling itself before its own body finishes checking must work.
        let prog = check_source(
            "function fib(n: i64): i64\n  if n < 2 then\n    return n\n  else\n    return fib(n - 1) + fib(n - 2)\n  end\nend\nfunction main(): i64\n  return fib(10)\nend\n",
        )
        .unwrap();
        assert_eq!(prog.functions.len(), 2);
    }

    #[test]
    fn calling_an_unknown_function_is_a_clear_error() {
        let err = check_source("function main(): i64\n  return nope(1)\nend\n").unwrap_err();
        assert!(err.contains("unknown function 'nope'"), "{err}");
    }

    #[test]
    fn indexing_a_non_array_is_a_clear_error() {
        let err = check_source("function main(): i64\n  local x: i64 = 1\n  return x[0]\nend\n")
            .unwrap_err();
        assert!(err.contains("cannot index into non-array type"), "{err}");
    }

    #[test]
    fn struct_literal_fields_are_reordered_to_the_declared_layout() {
        let prog = check_source("struct P { x: i64, y: i64 }\nfunction main(): i64\n  local p = P { y = 2, x = 1 }\n  return p.x\nend\n").unwrap();
        let TStmt::Local { value, .. } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        let TExprKind::StructLiteral { fields, .. } = &value.kind else {
            panic!()
        };
        // Declared order is x, y - regardless of the literal's y-then-x order.
        assert!(
            matches!(fields[0].kind, TExprKind::IntLit(1)),
            "{:?}",
            fields[0].kind
        );
        assert!(
            matches!(fields[1].kind, TExprKind::IntLit(2)),
            "{:?}",
            fields[1].kind
        );
    }

    #[test]
    fn a_struct_literal_missing_a_field_is_a_clear_error() {
        let err = check_source("struct P { x: i64, y: i64 }\nfunction main(): i64\n  local p = P { x = 1 }\n  return p.x\nend\n").unwrap_err();
        assert!(err.contains("every field must be initialized"), "{err}");
    }

    #[test]
    fn accessing_an_unknown_field_is_a_clear_error() {
        let err = check_source("struct P { x: i64 }\nfunction main(): i64\n  local p = P { x = 1 }\n  return p.z\nend\n").unwrap_err();
        assert!(err.contains("no field 'z'"), "{err}");
    }

    // -- Gradual typing (docs/features/gradual-typing.md) --

    #[test]
    fn a_scalar_boxed_into_any_and_unboxed_back_type_checks() {
        let prog = check_source(
            "function main(): i64\n  local y: any = 42\n  local z: i64 = y\n  return z\nend\n",
        )
        .unwrap();
        let TStmt::Local { value, .. } = &prog.functions[0].body[0].1 else {
            panic!()
        };
        assert!(matches!(value.kind, TExprKind::Box(_)), "{:?}", value.kind);
        assert_eq!(value.ty, Type::Any);
        let TStmt::Local { value, .. } = &prog.functions[0].body[1].1 else {
            panic!()
        };
        assert!(
            matches!(value.kind, TExprKind::Unbox(_, Type::I64)),
            "{:?}",
            value.kind
        );
        assert_eq!(value.ty, Type::I64);
    }

    #[test]
    fn boxing_a_struct_into_any_preserves_a_reference_payload() {
        let program = check_source(
            "struct P { x: i64 }\nfunction main(): i64\n  local p = P { x = 1 }\n  local a: any = p\n  return 0\nend\n",
        )
        .unwrap();
        let TStmt::Local { value, .. } = &program.functions[0].body[1].1 else {
            panic!()
        };
        assert!(matches!(value.kind, TExprKind::Box(_)), "{:?}", value.kind);
        assert_eq!(value.ty, Type::Any);
    }

    #[test]
    fn function_annotations_accept_top_level_functions_and_type_indirect_calls() {
        let prog = check_source(
            "function inc(x: i64): i64 return x + 1 end\nfunction apply(f: fn(i64) -> i64, x: i64): i64 return f(x) end\nfunction main(): i64 local f: fn(i64): i64 = inc return apply(f, 41) end",
        )
        .unwrap();
        let TStmt::Return { value: Some(value) } = &prog.functions[1].body[0].1 else {
            panic!()
        };
        assert!(
            matches!(value.kind, TExprKind::CallIndirect { .. }),
            "{:?}",
            value.kind
        );
    }

    #[test]
    fn function_annotations_reject_a_signature_mismatch() {
        let err = check_source(
            "function inc(x: i64): i64 return x + 1 end\nfunction main(): i64 local f: fn(f64) -> i64 = inc return 0 end",
        )
        .unwrap_err();
        assert!(
            err.contains("expected type fn(f64) -> i64, found fn(i64) -> i64"),
            "{err}"
        );
    }

    #[test]
    fn function_references_are_native_compilation_dependencies() {
        let prog = check_source(
            "function inc(x: i64): i64 return x + 1 end\nfunction use(f: fn(i64) -> i64): i64 return f(1) end\nfunction main(): i64 local f: fn(i64) -> i64 = inc return use(f) end",
        )
        .unwrap();
        let called = called_functions(&prog.functions[2]);
        assert!(called.contains("inc"), "{called:?}");
        assert!(called.contains("use"), "{called:?}");
    }

    #[test]
    fn do_end_blocks_are_lexically_scoped() {
        let err = check_source(
            "function main(): i64\n  do local hidden: i64 = 1 end\n  return hidden\nend",
        )
        .unwrap_err();
        assert!(err.contains("undefined variable 'hidden'"), "{err}");
    }

    #[test]
    fn arithmetic_on_any_uses_a_dynamic_operation() {
        let p =
            check_source("function add(x, y) return x+y end function main() return add(1,2) end")
                .unwrap();
        assert_eq!(p.functions[0].return_type, Type::Any);
    }

    #[test]
    fn a_dynamic_function_boundary_boxes_arguments_and_unboxes_the_return_value() {
        let prog = check_source(
            "function identity(x: any): any\n  return x\nend\nfunction main(): i64\n  local y: any = 42\n  local z: i64 = identity(y)\n  return z\nend\n",
        )
        .unwrap();
        // `x` is already `any`, so returning it needs no box/unbox.
        let identity = &prog.functions[0];
        let TStmt::Return { value: Some(v) } = &identity.body[0].1 else {
            panic!()
        };
        assert!(matches!(v.kind, TExprKind::Local(_)), "{:?}", v.kind);

        let main = &prog.functions[1];
        let TStmt::Local { value, .. } = &main.body[1].1 else {
            panic!()
        };
        // `identity(y)` is `any`, unboxed by the `i64` annotation.
        assert!(
            matches!(value.kind, TExprKind::Unbox(_, Type::I64)),
            "{:?}",
            value.kind
        );
        let TExprKind::Unbox(call, _) = &value.kind else {
            unreachable!()
        };
        assert!(
            matches!(call.kind, TExprKind::Call(_, _)),
            "{:?}",
            call.kind
        );
    }

    #[test]
    fn typed_map_literals_require_an_expected_type_and_keyed_entries() {
        let untyped = check_source("function main(): i64 local x = {} return 0 end").unwrap_err();
        assert!(
            untyped.contains("dynamic Lua table/function syntax"),
            "{untyped}"
        );

        let positional =
            check_source("function main(): i64 local x: Map<i64, i64> = { 1 } return 0 end")
                .unwrap_err();
        assert!(
            positional.contains("requires [key] = value"),
            "{positional}"
        );
    }

    #[test]
    fn pointer_bearing_maps_are_rejected_until_gc_layouts_exist() {
        let error =
            check_source("function main(): i64 local x: Map<string, i64> = {} return 0 end")
                .unwrap_err();
        assert!(error.contains("pointer-bearing map entries"), "{error}");
    }

    #[test]
    fn typed_generic_for_rejects_the_wrong_collection_kind() {
        let error = check_source(
            "function main(): i64 local x = new_array_i64(1) for k in pairs(x) do end return 0 end",
        )
        .unwrap_err();
        assert!(error.contains("pairs expects Map<K, V>"), "{error}");
    }
}
