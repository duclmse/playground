// Owns the `cranelift-jit` `JITModule`: declares every function's (and its
// uniform wrapper's, see `codegen::compile_wrapper`) signature up front,
// but only compiles a body on demand via `promote`, called from `tier.rs`
// once a function's hot-counter crosses its threshold.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{types, AbiParam, Signature};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Module};

use crate::codegen;
use crate::runtime;
use crate::types::{LocalId, TExpr, TExprKind, TFunction, TProgram, TStmt, Type};

/// `promote`'s result: the primary function's wrapper pointer, plus
/// `(name, wrapper_pointer)` for every dependency also compiled.
pub type PromoteResult = (*const u8, Vec<(String, *const u8)>);

pub struct Jit {
    module: JITModule,
    program: TProgram,
    runtime_funcs: codegen::RuntimeFuncs,
    func_ids: HashMap<String, FuncId>,
    wrapper_ids: HashMap<String, FuncId>,
    inlinable: HashSet<String>,
    /// Functions already `define_function`'d - guards against compiling (and
    /// re-`define_function`ing) the same body twice.
    compiled: HashSet<String>,
    builder_ctx: FunctionBuilderContext,
    dump_clif: bool,
    /// `--dump-asm`: prints Cranelift's `VCode` text (its pre-emission
    /// pseudo-assembly, not a byte-level disassembly - no capstone dep).
    dump_asm: bool,
    /// traces promotion/OSR/speculative-specialization events to stderr as they happen.
    jit_log: bool,
    /// Per-function speculative-specialization target, computed once (see
    /// `speculative_candidate`) rather than re-walked on every promotion.
    speculative_candidates: HashMap<String, (usize, LocalId, Type)>,
    /// U11 item 2: names in `speculative_candidates` whose candidate is
    /// provably exhaustive across the whole program (see
    /// `speculative_exhaustive`) - `tier.rs` wires these into
    /// `interp::Runtime` with the per-call runtime tag re-check skipped.
    speculative_exhaustive: HashSet<String>,
}

impl Jit {
    /// Declares every function's (and wrapper's) signature; no code generated yet.
    pub fn new(program: TProgram) -> Result<Self, String> {
        let mut builder = JITBuilder::with_flags(
            &[("opt_level", "speed")], //
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| e.to_string())?;
        builder.symbol("sol_new_array_i64", runtime::sol_new_array_i64 as *const u8);
        builder.symbol("sol_new_array_f64", runtime::sol_new_array_f64 as *const u8);
        builder.symbol("sol_new_array_ptr", runtime::sol_new_array_ptr as *const u8);
        builder.symbol("sol_array_map_i64", runtime::sol_array_map_i64 as *const u8);
        builder.symbol("sol_array_map_f64", runtime::sol_array_map_f64 as *const u8);
        builder.symbol("sol_new_map_i64", runtime::sol_new_map_i64 as *const u8);
        builder.symbol("sol_map_get_i64", runtime::sol_map_get_i64 as *const u8);
        builder.symbol("sol_map_set_i64", runtime::sol_map_set_i64 as *const u8);
        builder.symbol("sol_map_next_i64", runtime::sol_map_next_i64 as *const u8);
        builder.symbol(
            "sol_map_key_at_i64",
            runtime::sol_map_key_at_i64 as *const u8,
        );
        builder.symbol(
            "sol_map_value_at_i64",
            runtime::sol_map_value_at_i64 as *const u8,
        );
        builder.symbol(
            "sol_dynamic_binary",
            crate::dynamic::sol_dynamic_binary as *const u8,
        );
        builder.symbol(
            "sol_dynamic_compare",
            crate::dynamic::sol_dynamic_compare as *const u8,
        );
        builder.symbol(
            "sol_dynamic_neg",
            crate::dynamic::sol_dynamic_neg as *const u8,
        );
        builder.symbol("sol_truth", crate::dynamic::sol_truth as *const u8);
        builder.symbol("__sol_any_is", crate::dynamic::__sol_any_is as *const u8);
        crate::strings::register(&mut builder);
        builder.symbol("sol_alloc", runtime::sol_alloc as *const u8);
        builder.symbol("sol_alloc_layout", runtime::sol_alloc_layout as *const u8);
        builder.symbol("sol_pow", crate::numeric::sol_pow as *const u8);
        builder.symbol("sol_mod_float", crate::numeric::sol_mod_float as *const u8);
        builder.symbol(
            "sol_gc_write_barrier",
            crate::gc::sol_gc_write_barrier as *const u8,
        );
        let mut module = JITModule::new(builder);

        // `--target-info`: confirms which host ISA features Cranelift
        // actually detected (e.g. has_avx2 on x86-64).
        if std::env::var_os("SOL_TARGET_INFO")
            .or_else(|| std::env::var_os("SOL_TARGET_INFO"))
            .is_some()
        {
            let isa = module.isa();
            eprintln!(
                "[target] isa={} triple={} pointer_bits={}",
                isa.name(),
                isa.triple(),
                isa.pointer_bits()
            );
            for flag in isa.isa_flags() {
                eprintln!("[target]   {flag}");
            }
        }

        let runtime_funcs = codegen::declare_runtime(&mut module)?;
        let func_ids = codegen::declare_functions(&mut module, &program)?;
        let wrapper_ids = declare_wrappers(&mut module, &program)?;
        let inlinable = codegen::compute_inlinable(&program, &HashSet::new());
        let dump_clif = std::env::var_os("SOL_DUMP_CLIF")
            .or_else(|| std::env::var_os("SOL_DUMP_CLIF"))
            .is_some();
        let dump_asm = std::env::var_os("SOL_DUMP_ASM")
            .or_else(|| std::env::var_os("SOL_DUMP_ASM"))
            .is_some();
        let jit_log = std::env::var_os("SOL_JIT_LOG")
            .or_else(|| std::env::var_os("SOL_JIT_LOG"))
            .is_some();
        let speculative_candidates: HashMap<String, (usize, LocalId, Type)> = program
            .functions
            .iter()
            .filter_map(|f| speculative_candidate(f).map(|c| (f.name.clone(), c)))
            .collect();
        let speculative_exhaustive: HashSet<String> = speculative_candidates
            .iter()
            .filter(|(name, (param_index, _, target))| {
                is_speculative_exhaustive(&program, name, *param_index, target)
            })
            .map(|(name, _)| name.clone())
            .collect();
        if jit_log {
            for name in &speculative_exhaustive {
                eprintln!(
                    "[jit] '{name}' speculative candidate proven exhaustive across the whole program - runtime tag guard skipped"
                );
            }
        }

        // Externs are linked separately (`link_externs`), never via
        // `promote`'s dependency walk - seeding `compiled` with their
        // names makes the walk skip them as callees instead of trying to
        // find a body that doesn't exist.
        let compiled = program.externs.iter().map(|e| e.name.clone()).collect();

        Ok(Jit {
            module,
            program,
            runtime_funcs,
            func_ids,
            wrapper_ids,
            inlinable,
            compiled,
            builder_ctx: FunctionBuilderContext::new(),
            dump_clif,
            dump_asm,
            jit_log,
            speculative_candidates,
            speculative_exhaustive,
        })
    }

    /// `(param_index, target_type)` if `name` has a speculative candidate
    /// (see `speculative_candidate`) - tells `tier.rs` which functions'
    /// `any` parameters are worth tag-counting.
    pub fn speculative_candidate(&self, name: &str) -> Option<(usize, Type)> {
        self.speculative_candidates
            .get(name)
            .map(|(idx, _, ty)| (*idx, ty.clone()))
    }

    /// U11 item 2: whether `name`'s speculative candidate is provably
    /// exhaustive (see `is_speculative_exhaustive`) - `tier.rs` reads this to
    /// tell `interp::Runtime` it can skip the per-call runtime tag re-check
    /// for `name` once specialized, since every call site in the whole
    /// program is statically guaranteed to pass the candidate's target type.
    pub fn speculative_exhaustive(&self, name: &str) -> bool {
        self.speculative_exhaustive.contains(name)
    }

    /// M7 §25 FFI: compiles every extern's uniform-ABI wrapper (the real
    /// symbol itself has no body - it's an `Import`, resolved via `dlsym`
    /// at `finalize_definitions` time) and returns `(name, wrapper_ptr)`
    /// for each. Called once, up front - externs are always native, never
    /// interpreted, so `tier.rs` seeds their slot directly instead of
    /// waiting for a promotion threshold that would never fire (there's no
    /// bytecode for them to run in the meantime).
    pub fn link_externs(&mut self) -> Result<Vec<(String, *const u8)>, String> {
        for e in self.program.externs.clone() {
            let real_id = *self.func_ids.get(&e.name).expect("declared in new()");
            let wrapper_id = *self.wrapper_ids.get(&e.name).expect("declared in new()");
            let mut ctx = self.module.make_context();
            codegen::compile_wrapper(
                &mut self.module,
                &mut self.builder_ctx,
                &mut ctx.func,
                &e.params,
                &e.return_type,
                real_id,
            )?;
            self.module
                .define_function(wrapper_id, &mut ctx)
                .map_err(|e| e.to_string())?;
            self.module.clear_context(&mut ctx);
        }
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        Ok(self
            .program
            .externs
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    self.module
                        .get_finalized_function(self.wrapper_ids[&e.name]),
                )
            })
            .collect())
    }

    /// Compiles `name`'s speculative variant with its candidate `any`
    /// parameter narrowed to its target type (see `specialize`). The
    /// call-site tag guard in `interp::Runtime`, not this compile, is what
    /// makes calling it sound. Side entry point only, like `promote_osr` -
    /// doesn't touch `name`'s normal slot.
    pub fn promote_speculative(&mut self, name: &str) -> Result<PromoteResult, String> {
        let &(_, id, ref target) = self
            .speculative_candidates
            .get(name)
            .ok_or_else(|| format!("'{name}' has no speculative candidate"))?;
        let target = target.clone();
        if self.jit_log {
            eprintln!("[jit] speculatively specializing '{name}' (param narrowed to {target})");
        }
        let (_, mut deps) = self.promote(name)?;
        let f = self
            .program
            .functions
            .iter()
            .find(|f| f.name == name)
            .expect("tier.rs only promotes real functions")
            .clone();
        let spec_fn = specialize(&f, id, &target);
        let functions: HashMap<String, &TFunction> = self
            .program
            .functions
            .iter()
            .map(|g| (g.name.clone(), g))
            .collect();
        let prog_ctx = codegen::ProgramCtx {
            func_ids: &self.func_ids,
            functions: &functions,
            inlinable: &self.inlinable,
            runtime: &self.runtime_funcs,
        };
        let call_conv = self.module.target_config().default_call_conv;
        let param_types: Vec<Type> = spec_fn.params.iter().map(|(_, t)| t.clone()).collect();

        let spec_sig = codegen::signature_of(call_conv, &param_types, &spec_fn.return_type);
        let tag =
            crate::value::tag_for(&target).expect("candidate types are always boxable scalars");
        let spec_id = self
            .module
            .declare_function(
                &format!("{name}__spec{tag}"),
                cranelift_module::Linkage::Local,
                &spec_sig,
            )
            .map_err(|e| e.to_string())?;
        let mut ctx = self.module.make_context();
        codegen::compile_function(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            &spec_fn,
            &prog_ctx,
        )?;
        if self.dump_clif {
            dump_clif_with_legend(&self.module, &format!("{name}__spec{tag}"), &ctx.func);
        }
        if self.dump_asm {
            ctx.set_disasm(true);
        }
        self.module
            .define_function(spec_id, &mut ctx)
            .map_err(|e| e.to_string())?;
        dump_asm_if_present(self.dump_asm, &format!("{name}__spec{tag}"), &ctx);
        self.module.clear_context(&mut ctx);

        let mut wrapper_sig = Signature::new(call_conv);
        wrapper_sig.params.push(AbiParam::new(types::I64));
        wrapper_sig.returns.push(AbiParam::new(types::I64));
        let spec_wrapper_id = self
            .module
            .declare_function(
                &format!("{name}__spec{tag}__wrapper"),
                cranelift_module::Linkage::Local,
                &wrapper_sig,
            )
            .map_err(|e| e.to_string())?;
        let mut ctx = self.module.make_context();
        codegen::compile_wrapper(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            &param_types,
            &spec_fn.return_type,
            spec_id,
        )?;
        self.module
            .define_function(spec_wrapper_id, &mut ctx)
            .map_err(|e| e.to_string())?;
        self.module.clear_context(&mut ctx);

        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        let ptr = self.module.get_finalized_function(spec_wrapper_id);
        deps.retain(|(n, _)| n != name);
        Ok((ptr, deps))
    }

    /// Compiles `name`'s real body and wrapper, returning the wrapper's
    /// entry point plus every other function compiled as a dependency (as
    /// `(name, wrapper_ptr)` pairs) so `tier.rs` can promote those too.
    ///
    /// Compiling only `name` isn't enough: it may call a non-inlined
    /// callee, and `JITModule::finalize_definitions` fails if a referenced
    /// function was only declared, never defined. So this does a full
    /// transitive dependency walk first. `self.compiled` makes it safe to
    /// call repeatedly without double-`define_function`ing a body.
    pub fn promote(&mut self, name: &str) -> Result<PromoteResult, String> {
        if self.jit_log && !self.compiled.contains(name) {
            eprintln!("[jit] promoting '{name}'");
        }
        let mut worklist = vec![name.to_string()];
        let mut newly_compiled = Vec::new();
        while let Some(n) = worklist.pop() {
            if self.compiled.contains(&n) {
                continue;
            }
            if self.jit_log && n != name {
                eprintln!("[jit]   + dependency '{n}'");
            }
            let f = self
                .program
                .functions
                .iter()
                .find(|f| f.name == n)
                .expect("only ever pushed real function names")
                .clone();
            self.compile_real_body(&f)?;
            self.compile_wrapper_body(&f)?;
            self.compiled.insert(n.clone());
            newly_compiled.push(n.clone());
            // Every real callee needs a compiled body here, even one that's
            // `inlinable`: `inline_call`'s `MAX_INLINE_DEPTH` safety net
            // (codegen.rs) falls back to a genuine out-of-line `call` once
            // compile-time inline-unrolling gets that deep, which happens
            // for any mutual-recursion cycle `is_directly_recursive` didn't
            // catch (it only detects direct self-recursion). Skipping
            // inlinable callees here left their `FuncId` declared but never
            // defined, so `finalize_definitions` failed to resolve the
            // fallback call's target symbol.
            for callee in crate::typeck::called_functions(&f) {
                if !self.compiled.contains(&callee) {
                    worklist.push(callee);
                }
            }
        }
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;

        let ptr_of = |n: &str| {
            self.module.get_finalized_function(
                *self
                    .wrapper_ids
                    .get(n)
                    .expect("declared for every function in new()"),
            )
        };
        let primary = ptr_of(name);
        let others = newly_compiled
            .iter()
            .filter(|n| n.as_str() != name)
            .map(|n| (n.clone(), ptr_of(n)))
            .collect();
        Ok((primary, others))
    }

    fn compile_real_body(&mut self, f: &TFunction) -> Result<(), String> {
        let functions: HashMap<String, &TFunction> = self
            .program
            .functions
            .iter()
            .map(|g| (g.name.clone(), g))
            .collect();
        let prog_ctx = codegen::ProgramCtx {
            func_ids: &self.func_ids,
            functions: &functions,
            inlinable: &self.inlinable,
            runtime: &self.runtime_funcs,
        };
        let mut ctx = self.module.make_context();
        codegen::compile_function(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            f,
            &prog_ctx,
        )?;
        if self.dump_clif {
            dump_clif_with_legend(&self.module, &f.name, &ctx.func);
        }
        if self.dump_asm {
            ctx.set_disasm(true);
        }
        let id = *self.func_ids.get(&f.name).expect("declared in new()");
        self.module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        dump_asm_if_present(self.dump_asm, &f.name, &ctx);
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    fn compile_wrapper_body(&mut self, f: &TFunction) -> Result<(), String> {
        let real_id = *self.func_ids.get(&f.name).expect("declared in new()");
        let wrapper_id = *self.wrapper_ids.get(&f.name).expect("declared in new()");
        let params: Vec<crate::types::Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
        let mut ctx = self.module.make_context();
        codegen::compile_wrapper(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            &params,
            &f.return_type,
            real_id,
        )?;
        self.module
            .define_function(wrapper_id, &mut ctx)
            .map_err(|e| e.to_string())?;
        self.module.clear_context(&mut ctx);
        Ok(())
    }

    /// On-stack replacement: compiles `name`'s OSR entry starting at its
    /// `from_stmt`-th top-level statement, plus that entry's wrapper.
    /// Unlike `promote`, doesn't replace `name`'s own interpreter slot -
    /// `tier.rs` calls this pointer directly from an already-running
    /// interpreted call, as a one-off jump into native code for the rest
    /// of that call.
    pub fn promote_osr(&mut self, name: &str, from_stmt: usize) -> Result<PromoteResult, String> {
        if self.jit_log {
            eprintln!("[jit] on-stack replacement for '{name}' at statement {from_stmt}");
        }
        // OSR entry can call other functions too - same dependency walk.
        let (_, mut deps) = self.promote(name)?;
        let f = self
            .program
            .functions
            .iter()
            .find(|f| f.name == name)
            .expect("tier.rs only promotes real functions")
            .clone();
        let functions: HashMap<String, &TFunction> = self
            .program
            .functions
            .iter()
            .map(|g| (g.name.clone(), g))
            .collect();
        let prog_ctx = codegen::ProgramCtx {
            func_ids: &self.func_ids,
            functions: &functions,
            inlinable: &self.inlinable,
            runtime: &self.runtime_funcs,
        };
        let call_conv = self.module.target_config().default_call_conv;
        let local_types = codegen::collect_local_types(&f);

        let osr_sig = codegen::signature_of(call_conv, &local_types, &f.return_type);
        let osr_id = self
            .module
            .declare_function(
                &format!("{name}__osr{from_stmt}"),
                cranelift_module::Linkage::Local,
                &osr_sig,
            )
            .map_err(|e| e.to_string())?;
        let mut ctx = self.module.make_context();
        codegen::compile_osr_entry(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            &f,
            from_stmt,
            &prog_ctx,
        )?;
        if self.dump_clif {
            dump_clif_with_legend(&self.module, &format!("{name}__osr{from_stmt}"), &ctx.func);
        }
        if self.dump_asm {
            ctx.set_disasm(true);
        }
        self.module
            .define_function(osr_id, &mut ctx)
            .map_err(|e| e.to_string())?;
        dump_asm_if_present(self.dump_asm, &format!("{name}__osr{from_stmt}"), &ctx);
        self.module.clear_context(&mut ctx);

        let mut wrapper_sig = Signature::new(call_conv);
        wrapper_sig.params.push(AbiParam::new(types::I64));
        wrapper_sig.returns.push(AbiParam::new(types::I64));
        let osr_wrapper_id = self
            .module
            .declare_function(
                &format!("{name}__osr{from_stmt}__wrapper"),
                cranelift_module::Linkage::Local,
                &wrapper_sig,
            )
            .map_err(|e| e.to_string())?;
        let mut ctx = self.module.make_context();
        codegen::compile_wrapper(
            &mut self.module,
            &mut self.builder_ctx,
            &mut ctx.func,
            &local_types,
            &f.return_type,
            osr_id,
        )?;
        self.module
            .define_function(osr_wrapper_id, &mut ctx)
            .map_err(|e| e.to_string())?;
        self.module.clear_context(&mut ctx);

        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        let ptr = self.module.get_finalized_function(osr_wrapper_id);
        deps.retain(|(n, _)| n != name);
        Ok((ptr, deps))
    }
}

/// A function's `any`-typed parameter is a speculative candidate if every
/// use in the body immediately narrows it to one concrete type via
/// `Unbox` (see `infer_unbox_type`) - anything looser (returned as `any`,
/// reassigned, narrowed to >1 type) is left un-specialized. Only the first
/// such parameter is considered (scope cut, not a fundamental limit).
fn speculative_candidate(f: &TFunction) -> Option<(usize, LocalId, Type)> {
    let (index, &(id, _)) = f
        .params
        .iter()
        .enumerate()
        .find(|(_, (_, t))| *t == Type::Any)?;
    let target = infer_unbox_type(f, id)?;
    Some((index, id, target))
}

/// The single concrete type every `Unbox(Local(id), T)` in `f`'s body
/// agrees on, or `None` if disqualified (mixed types, a bare use, a
/// reassignment, or never unboxed at all).
fn infer_unbox_type(f: &TFunction, id: LocalId) -> Option<Type> {
    fn walk_expr(e: &TExpr, id: LocalId, found: &mut Option<Type>, ok: &mut bool) {
        if !*ok {
            return;
        }
        match &e.kind {
            TExprKind::Unbox(inner, t) => {
                if let TExprKind::Local(lid) = &inner.kind {
                    if *lid == id {
                        match found {
                            Some(existing) if existing != t => *ok = false,
                            Some(_) => {}
                            None => *found = Some(t.clone()),
                        }
                        return; // classified - don't also treat as a bare use below
                    }
                }
                walk_expr(inner, id, found, ok);
            }
            TExprKind::Local(lid) => {
                if *lid == id {
                    *ok = false; // used somewhere that isn't an immediate Unbox - can't specialize
                }
            }
            TExprKind::Box(inner)
            | TExprKind::Truth(inner)
            | TExprKind::Neg(inner)
            | TExprKind::Not(inner)
            | TExprKind::IntToFloat(inner)
            | TExprKind::Len(inner)
            | TExprKind::Field { base: inner, .. } => walk_expr(inner, id, found, ok),
            TExprKind::Arith(_, l, r)
            | TExprKind::Compare(_, l, r)
            | TExprKind::Logical(_, l, r)
            | TExprKind::Index(l, r) => {
                walk_expr(l, id, found, ok);
                walk_expr(r, id, found, ok);
            }
            TExprKind::Call(_, args) => args.iter().for_each(|a| walk_expr(a, id, found, ok)),
            TExprKind::CallIndirect { callee, args } => {
                walk_expr(callee, id, found, ok);
                args.iter().for_each(|a| walk_expr(a, id, found, ok));
            }
            TExprKind::NewArray { len, .. } => walk_expr(len, id, found, ok),
            TExprKind::ArrayLiteral { values, .. } => values
                .iter()
                .for_each(|value| walk_expr(value, id, found, ok)),
            TExprKind::ArrayMap { array, callback, .. } => {
                walk_expr(array, id, found, ok);
                walk_expr(callback, id, found, ok);
            }
            TExprKind::NewMap { .. } => {}
            TExprKind::MapLiteral { entries, .. } => {
                for (key, value) in entries {
                    walk_expr(key, id, found, ok);
                    walk_expr(value, id, found, ok);
                }
            }
            TExprKind::MapNext { map, cursor }
            | TExprKind::MapKey { map, cursor }
            | TExprKind::MapValue { map, cursor } => {
                walk_expr(map, id, found, ok);
                walk_expr(cursor, id, found, ok);
            }
            TExprKind::StructLiteral { fields, .. } => {
                fields.iter().for_each(|f| walk_expr(f, id, found, ok))
            }
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::FunctionRef(_) => {}
        }
    }
    fn walk_stmts(stmts: &[TStmt], id: LocalId, found: &mut Option<Type>, ok: &mut bool) {
        for s in stmts {
            if !*ok {
                return;
            }
            match s {
                TStmt::Break => {}
                TStmt::Local { value, .. } => walk_expr(value, id, found, ok),
                TStmt::Assign { id: aid, value } => {
                    if *aid == id {
                        *ok = false; // reassigning the param would rewrite it back to a box - disqualifying
                        return;
                    }
                    walk_expr(value, id, found, ok);
                }
                TStmt::AssignIndex {
                    array,
                    index,
                    value,
                } => {
                    walk_expr(array, id, found, ok);
                    walk_expr(index, id, found, ok);
                    walk_expr(value, id, found, ok);
                }
                TStmt::AssignField { base, value, .. } => {
                    walk_expr(base, id, found, ok);
                    walk_expr(value, id, found, ok);
                }
                TStmt::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    walk_expr(cond, id, found, ok);
                    walk_stmts(then_block, id, found, ok);
                    walk_stmts(else_block, id, found, ok);
                }
                TStmt::While { cond, body } => {
                    walk_expr(cond, id, found, ok);
                    walk_stmts(body, id, found, ok);
                }
                TStmt::NumericFor {
                    start,
                    stop,
                    step,
                    body,
                    ..
                } => {
                    walk_expr(start, id, found, ok);
                    walk_expr(stop, id, found, ok);
                    walk_expr(step, id, found, ok);
                    walk_stmts(body, id, found, ok);
                }
                TStmt::Return { value } => {
                    if let Some(v) = value {
                        walk_expr(v, id, found, ok);
                    }
                }
            }
        }
    }
    let mut found = None;
    let mut ok = true;
    walk_stmts(&f.body, id, &mut found, &mut ok);
    if ok {
        found
    } else {
        None
    }
}

/// U11 item 2: whole-program static proof that `name`'s speculative
/// candidate (`param_index`/`target`, from `speculative_candidate`) needs no
/// runtime tag guard - every call to `name` anywhere in `program` passes a
/// value just boxed from exactly `target` at that call site (via `coerce`;
/// see `typeck.rs`), never a value forwarded through an already-`any`
/// local/field/param, never a different concrete type, and `name` is never
/// taken as a first-class value (an indirect call through a stored function
/// reference would make call sites uncountable). A function proven this way
/// can never actually observe a type mismatch at this parameter, so skipping
/// the check cannot turn a would-be trap into silent bit-misinterpretation -
/// it can only ever remove a check that would always have passed.
///
/// Requires at least one direct call site: an unreachable/never-called
/// function has no evidence to prove exhaustiveness from.
///
/// Scope cut, not a fundamental limit: a value boxed into an `any`-typed
/// local before being passed on (`local b: any = x; f(b)`) defeats this
/// syntactic check even when every write to that local agrees on the type -
/// proving that needs per-local reaching-definitions dataflow, which this
/// pass does not attempt. Such calls simply don't count as evidence, so the
/// guard is conservatively kept (never unsoundly dropped).
fn is_speculative_exhaustive(program: &TProgram, name: &str, param_index: usize, target: &Type) -> bool {
    let mut call_sites = 0usize;
    let mut sound = true;
    for f in &program.functions {
        walk_stmts_for_exhaustiveness(&f.body, name, param_index, target, &mut call_sites, &mut sound);
        if !sound {
            return false;
        }
    }
    sound && call_sites > 0
}

fn walk_stmts_for_exhaustiveness(
    stmts: &[TStmt],
    name: &str,
    param_index: usize,
    target: &Type,
    call_sites: &mut usize,
    sound: &mut bool,
) {
    for s in stmts {
        if !*sound {
            return;
        }
        match s {
            TStmt::Break => {}
            TStmt::Local { value, .. } | TStmt::Assign { value, .. } => {
                walk_expr_for_exhaustiveness(value, name, param_index, target, call_sites, sound)
            }
            TStmt::AssignIndex { array, index, value } => {
                walk_expr_for_exhaustiveness(array, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(index, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(value, name, param_index, target, call_sites, sound);
            }
            TStmt::AssignField { base, value, .. } => {
                walk_expr_for_exhaustiveness(base, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(value, name, param_index, target, call_sites, sound);
            }
            TStmt::If { cond, then_block, else_block } => {
                walk_expr_for_exhaustiveness(cond, name, param_index, target, call_sites, sound);
                walk_stmts_for_exhaustiveness(then_block, name, param_index, target, call_sites, sound);
                walk_stmts_for_exhaustiveness(else_block, name, param_index, target, call_sites, sound);
            }
            TStmt::While { cond, body } => {
                walk_expr_for_exhaustiveness(cond, name, param_index, target, call_sites, sound);
                walk_stmts_for_exhaustiveness(body, name, param_index, target, call_sites, sound);
            }
            TStmt::NumericFor { start, stop, step, body, .. } => {
                walk_expr_for_exhaustiveness(start, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(stop, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(step, name, param_index, target, call_sites, sound);
                walk_stmts_for_exhaustiveness(body, name, param_index, target, call_sites, sound);
            }
            TStmt::Return { value } => {
                if let Some(v) = value {
                    walk_expr_for_exhaustiveness(v, name, param_index, target, call_sites, sound);
                }
            }
        }
    }
}

fn walk_expr_for_exhaustiveness(
    e: &TExpr,
    name: &str,
    param_index: usize,
    target: &Type,
    call_sites: &mut usize,
    sound: &mut bool,
) {
    if !*sound {
        return;
    }
    match &e.kind {
        TExprKind::FunctionRef(n) => {
            if n.as_str() == name {
                // Taken as a first-class value - indirect call sites through
                // it aren't enumerable, so exhaustiveness isn't provable.
                *sound = false;
            }
        }
        TExprKind::Call(callee, args) => {
            if callee.as_str() == name {
                match args.get(param_index) {
                    Some(TExpr {
                        kind: TExprKind::Box(inner),
                        ..
                    }) if inner.ty == *target => {
                        *call_sites += 1;
                    }
                    _ => *sound = false,
                }
            }
            args.iter()
                .for_each(|a| walk_expr_for_exhaustiveness(a, name, param_index, target, call_sites, sound));
        }
        TExprKind::CallIndirect { callee, args } => {
            walk_expr_for_exhaustiveness(callee, name, param_index, target, call_sites, sound);
            args.iter()
                .for_each(|a| walk_expr_for_exhaustiveness(a, name, param_index, target, call_sites, sound));
        }
        TExprKind::Truth(inner)
        | TExprKind::Neg(inner)
        | TExprKind::Not(inner)
        | TExprKind::IntToFloat(inner)
        | TExprKind::Len(inner)
        | TExprKind::Field { base: inner, .. }
        | TExprKind::Box(inner)
        | TExprKind::Unbox(inner, _) => {
            walk_expr_for_exhaustiveness(inner, name, param_index, target, call_sites, sound)
        }
        TExprKind::Arith(_, l, r)
        | TExprKind::Compare(_, l, r)
        | TExprKind::Logical(_, l, r)
        | TExprKind::Index(l, r) => {
            walk_expr_for_exhaustiveness(l, name, param_index, target, call_sites, sound);
            walk_expr_for_exhaustiveness(r, name, param_index, target, call_sites, sound);
        }
        TExprKind::NewArray { len, .. } => {
            walk_expr_for_exhaustiveness(len, name, param_index, target, call_sites, sound)
        }
        TExprKind::ArrayLiteral { values, .. } => values
            .iter()
            .for_each(|v| walk_expr_for_exhaustiveness(v, name, param_index, target, call_sites, sound)),
        TExprKind::ArrayMap { array, callback, .. } => {
            walk_expr_for_exhaustiveness(array, name, param_index, target, call_sites, sound);
            walk_expr_for_exhaustiveness(callback, name, param_index, target, call_sites, sound);
        }
        TExprKind::NewMap { .. } => {}
        TExprKind::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                walk_expr_for_exhaustiveness(k, name, param_index, target, call_sites, sound);
                walk_expr_for_exhaustiveness(v, name, param_index, target, call_sites, sound);
            }
        }
        TExprKind::MapNext { map, cursor }
        | TExprKind::MapKey { map, cursor }
        | TExprKind::MapValue { map, cursor } => {
            walk_expr_for_exhaustiveness(map, name, param_index, target, call_sites, sound);
            walk_expr_for_exhaustiveness(cursor, name, param_index, target, call_sites, sound);
        }
        TExprKind::StructLiteral { fields, .. } => fields
            .iter()
            .for_each(|f| walk_expr_for_exhaustiveness(f, name, param_index, target, call_sites, sound)),
        TExprKind::StringLit(_)
        | TExprKind::NilLit
        | TExprKind::IntLit(_)
        | TExprKind::FloatLit(_)
        | TExprKind::BoolLit(_)
        | TExprKind::Local(_) => {}
    }
}

/// Rewrites `f` with `id` (a speculative-candidate `any` parameter)
/// narrowed to `target`: its declared type becomes `target`, and every
/// `Unbox(Local(id), target)` node is replaced by a plain `Local(id)` -
/// the call-site guard already checked the tag, so the `Unbox` check
/// would be redundant.
fn specialize(f: &TFunction, id: LocalId, target: &Type) -> TFunction {
    fn rewrite_expr(e: &TExpr, id: LocalId, target: &Type) -> TExpr {
        let ty = e.ty.clone();
        match &e.kind {
            TExprKind::Unbox(inner, t) => {
                if let TExprKind::Local(lid) = &inner.kind {
                    if *lid == id && t == target {
                        return TExpr {
                            kind: TExprKind::Local(id),
                            ty: target.clone(),
                        };
                    }
                }
                TExpr {
                    kind: TExprKind::Unbox(Box::new(rewrite_expr(inner, id, target)), t.clone()),
                    ty,
                }
            }
            TExprKind::Box(inner) => TExpr {
                kind: TExprKind::Box(Box::new(rewrite_expr(inner, id, target))),
                ty,
            },
            TExprKind::Truth(inner) => TExpr {
                ty,
                kind: TExprKind::Truth(Box::new(rewrite_expr(inner, id, target))),
            },
            TExprKind::Neg(inner) => TExpr {
                kind: TExprKind::Neg(Box::new(rewrite_expr(inner, id, target))),
                ty,
            },
            TExprKind::Not(inner) => TExpr {
                kind: TExprKind::Not(Box::new(rewrite_expr(inner, id, target))),
                ty,
            },
            TExprKind::IntToFloat(inner) => TExpr {
                kind: TExprKind::IntToFloat(Box::new(rewrite_expr(inner, id, target))),
                ty,
            },
            TExprKind::Len(inner) => TExpr {
                kind: TExprKind::Len(Box::new(rewrite_expr(inner, id, target))),
                ty,
            },
            TExprKind::Arith(op, l, r) => TExpr {
                kind: TExprKind::Arith(
                    *op,
                    Box::new(rewrite_expr(l, id, target)),
                    Box::new(rewrite_expr(r, id, target)),
                ),
                ty,
            },
            TExprKind::Compare(op, l, r) => TExpr {
                kind: TExprKind::Compare(
                    *op,
                    Box::new(rewrite_expr(l, id, target)),
                    Box::new(rewrite_expr(r, id, target)),
                ),
                ty,
            },
            TExprKind::Logical(op, l, r) => TExpr {
                kind: TExprKind::Logical(
                    *op,
                    Box::new(rewrite_expr(l, id, target)),
                    Box::new(rewrite_expr(r, id, target)),
                ),
                ty,
            },
            TExprKind::Index(l, r) => TExpr {
                kind: TExprKind::Index(
                    Box::new(rewrite_expr(l, id, target)),
                    Box::new(rewrite_expr(r, id, target)),
                ),
                ty,
            },
            TExprKind::Call(name, args) => TExpr {
                kind: TExprKind::Call(
                    name.clone(),
                    args.iter().map(|a| rewrite_expr(a, id, target)).collect(),
                ),
                ty,
            },
            TExprKind::CallIndirect { callee, args } => TExpr {
                kind: TExprKind::CallIndirect {
                    callee: Box::new(rewrite_expr(callee, id, target)),
                    args: args.iter().map(|a| rewrite_expr(a, id, target)).collect(),
                },
                ty,
            },
            TExprKind::NewArray { elem, len } => TExpr {
                kind: TExprKind::NewArray {
                    elem: elem.clone(),
                    len: Box::new(rewrite_expr(len, id, target)),
                },
                ty,
            },
            TExprKind::ArrayLiteral { elem, values } => TExpr {
                kind: TExprKind::ArrayLiteral {
                    elem: elem.clone(),
                    values: values
                        .iter()
                        .map(|value| rewrite_expr(value, id, target))
                        .collect(),
                },
                ty,
            },
            TExprKind::ArrayMap {
                array,
                callback,
                elem,
            } => TExpr {
                kind: TExprKind::ArrayMap {
                    array: Box::new(rewrite_expr(array, id, target)),
                    callback: Box::new(rewrite_expr(callback, id, target)),
                    elem: elem.clone(),
                },
                ty,
            },
            TExprKind::NewMap { key, value } => TExpr {
                kind: TExprKind::NewMap {
                    key: key.clone(),
                    value: value.clone(),
                },
                ty,
            },
            TExprKind::MapLiteral {
                key,
                value,
                entries,
            } => TExpr {
                kind: TExprKind::MapLiteral {
                    key: key.clone(),
                    value: value.clone(),
                    entries: entries
                        .iter()
                        .map(|(key, value)| {
                            (
                                rewrite_expr(key, id, target),
                                rewrite_expr(value, id, target),
                            )
                        })
                        .collect(),
                },
                ty,
            },
            TExprKind::MapNext { map, cursor } => TExpr {
                kind: TExprKind::MapNext {
                    map: Box::new(rewrite_expr(map, id, target)),
                    cursor: Box::new(rewrite_expr(cursor, id, target)),
                },
                ty,
            },
            TExprKind::MapKey { map, cursor } => TExpr {
                kind: TExprKind::MapKey {
                    map: Box::new(rewrite_expr(map, id, target)),
                    cursor: Box::new(rewrite_expr(cursor, id, target)),
                },
                ty,
            },
            TExprKind::MapValue { map, cursor } => TExpr {
                kind: TExprKind::MapValue {
                    map: Box::new(rewrite_expr(map, id, target)),
                    cursor: Box::new(rewrite_expr(cursor, id, target)),
                },
                ty,
            },
            TExprKind::StructLiteral { name, fields } => TExpr {
                kind: TExprKind::StructLiteral {
                    name: name.clone(),
                    fields: fields.iter().map(|f| rewrite_expr(f, id, target)).collect(),
                },
                ty,
            },
            TExprKind::Field { base, field_index } => TExpr {
                kind: TExprKind::Field {
                    base: Box::new(rewrite_expr(base, id, target)),
                    field_index: *field_index,
                },
                ty,
            },
            TExprKind::StringLit(_)
            | TExprKind::NilLit
            | TExprKind::IntLit(_)
            | TExprKind::FloatLit(_)
            | TExprKind::BoolLit(_)
            | TExprKind::Local(_)
            | TExprKind::FunctionRef(_) => e.clone(),
        }
    }
    fn rewrite_stmts(stmts: &[TStmt], id: LocalId, target: &Type) -> Vec<TStmt> {
        stmts
            .iter()
            .map(|s| match s {
                TStmt::Break => TStmt::Break,
                TStmt::Local { id: lid, value } => TStmt::Local {
                    id: *lid,
                    value: rewrite_expr(value, id, target),
                },
                TStmt::Assign { id: aid, value } => TStmt::Assign {
                    id: *aid,
                    value: rewrite_expr(value, id, target),
                },
                TStmt::AssignIndex {
                    array,
                    index,
                    value,
                } => TStmt::AssignIndex {
                    array: rewrite_expr(array, id, target),
                    index: rewrite_expr(index, id, target),
                    value: rewrite_expr(value, id, target),
                },
                TStmt::AssignField {
                    base,
                    field_index,
                    value,
                } => TStmt::AssignField {
                    base: rewrite_expr(base, id, target),
                    field_index: *field_index,
                    value: rewrite_expr(value, id, target),
                },
                TStmt::If {
                    cond,
                    then_block,
                    else_block,
                } => TStmt::If {
                    cond: rewrite_expr(cond, id, target),
                    then_block: rewrite_stmts(then_block, id, target),
                    else_block: rewrite_stmts(else_block, id, target),
                },
                TStmt::While { cond, body } => TStmt::While {
                    cond: rewrite_expr(cond, id, target),
                    body: rewrite_stmts(body, id, target),
                },
                TStmt::NumericFor {
                    id: lid,
                    stop_id,
                    step_id,
                    start,
                    stop,
                    step,
                    body,
                } => TStmt::NumericFor {
                    id: *lid,
                    stop_id: *stop_id,
                    step_id: *step_id,
                    start: rewrite_expr(start, id, target),
                    stop: rewrite_expr(stop, id, target),
                    step: rewrite_expr(step, id, target),
                    body: rewrite_stmts(body, id, target),
                },
                TStmt::Return { value } => TStmt::Return {
                    value: value.as_ref().map(|v| rewrite_expr(v, id, target)),
                },
            })
            .collect()
    }
    let mut params = f.params.clone();
    if let Some(p) = params.iter_mut().find(|(pid, _)| *pid == id) {
        p.1 = target.clone();
    }
    TFunction {
        name: f.name.clone(),
        source_file: f.source_file.clone(),
        source_line: f.source_line,
        source_span: f.source_span,
        params,
        return_type: f.return_type.clone(),
        body: rewrite_stmts(&f.body, id, target),
        local_count: f.local_count,
    }
}

/// `--dump-ir`: prints `func`'s CLIF text plus a legend resolving every
/// callee it references back to its real name. Cranelift's `Function`
/// `Display` only ever prints callees as opaque `u0:N` module-function-id
/// references (never the linkage name), so without this legend a dumped
/// call to e.g. `sol_dynamic_binary` is indistinguishable by eye (or by
/// grep) from a call to any other runtime helper or user function that
/// happens to share the same `u0:N` numbering scheme.
fn dump_clif_with_legend(module: &JITModule, label: &str, func: &cranelift_codegen::ir::Function) {
    eprintln!("; ==== {label} ====\n{func}");
    for (funcref, data) in func.dfg.ext_funcs.iter() {
        if let cranelift_codegen::ir::ExternalName::User(user_ref) = data.name {
            let user_name = &func.params.user_named_funcs()[user_ref];
            if user_name.namespace == 0 {
                let id = FuncId::from_u32(user_name.index);
                let name = module.declarations().get_function_decl(id).linkage_name(id);
                eprintln!("; {funcref} = {name}");
            }
        }
    }
}

/// `--dump-asm`: prints `ctx`'s `VCode` text, if `set_disasm(true)` was
/// called before `define_function` compiled it.
fn dump_asm_if_present(enabled: bool, name: &str, ctx: &cranelift_codegen::Context) {
    if !enabled {
        return;
    }
    if let Some(vcode) = ctx.compiled_code().and_then(|code| code.vcode.as_deref()) {
        eprintln!("; ==== {name} (asm) ====\n{vcode}");
    }
}

fn declare_wrappers(
    module: &mut dyn Module,
    program: &TProgram,
) -> Result<HashMap<String, FuncId>, String> {
    let call_conv = module.target_config().default_call_conv;
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let names = program
        .functions
        .iter()
        .map(|f| &f.name)
        .chain(program.externs.iter().map(|e| &e.name));
    names
        .map(|name| {
            let id = module
                .declare_function(
                    &format!("{name}__wrapper"),
                    cranelift_module::Linkage::Local,
                    &sig,
                )
                .map_err(|e| e.to_string())?;
            Ok((name.clone(), id))
        })
        .collect()
}
