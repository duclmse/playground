// M7 §22: AOT compilation to a standalone executable. No tiering - every
// function is compiled eagerly via `cranelift-object`'s `ObjectModule`
// (there's no interpreter/JIT shipped with the binary to promote into).
// Produces a `.o`, generates a small hand-written C-ABI `main` that calls
// the compiled sol `main` and prints its result, then shells out to
// the system `cc` to link the object against this crate's own `staticlib`
// (runtime.rs's `#[no_mangle]` alloc/GC/print functions).
//
// Only tested on macOS ARM64 this session - Linux linking may need extra
// system libs (pthread/dl/m) not yet verified.

use std::collections::HashMap;
use std::process::Command;

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, Signature};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::Configurable;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::codegen;
use crate::types::{TFunction, TProgram, Type};

/// sol's own `main` function can't share the object-file symbol "main" with the
/// generated C entry point below.
const SOL_MAIN_SYMBOL: &str = "__sol_main";

pub fn build(program: TProgram, return_type: Type, output_path: &str) -> Result<(), String> {
    let isa_builder = cranelift_native::builder().map_err(|e| e.to_string())?;
    let mut flag_builder = cranelift_codegen::settings::builder();
    // A real executable must be position-independent (macOS's linker
    // rejects text relocations in a PIE main executable) - unlike the JIT
    // path, which maps its own memory and doesn't need this.
    flag_builder
        .set("is_pic", "true")
        .map_err(|e| e.to_string())?;
    flag_builder
        .set("opt_level", "speed")
        .map_err(|e| e.to_string())?;
    let flags = cranelift_codegen::settings::Flags::new(flag_builder);
    let isa = isa_builder.finish(flags).map_err(|e| e.to_string())?;
    let call_conv = isa.default_call_conv();

    let obj_builder = ObjectBuilder::new(isa, "sol", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut module = ObjectModule::new(obj_builder);

    let runtime_funcs = codegen::declare_runtime(&mut module)?;
    let gc_init_id = declare_void_import(&mut module, call_conv, "sol_gc_init_stack_base")?;
    let print_i64_id = declare_unary_import(&mut module, call_conv, "sol_print_i64", types::I64)?;
    let print_f64_id = declare_unary_import(&mut module, call_conv, "sol_print_f64", types::F64)?;
    let print_bool_id = declare_unary_import(&mut module, call_conv, "sol_print_bool", types::I64)?;

    let mut func_ids: HashMap<String, FuncId> = HashMap::new();
    for f in &program.functions {
        let param_types: Vec<Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
        let sig = codegen::signature_of(call_conv, &param_types, &f.return_type);
        let symbol = if f.name == "main" {
            SOL_MAIN_SYMBOL
        } else {
            f.name.as_str()
        };
        let id = module
            .declare_function(symbol, Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        func_ids.insert(f.name.clone(), id);
    }
    for e in &program.externs {
        let sig = codegen::signature_of(call_conv, &e.params, &e.return_type);
        let id = module
            .declare_function(&e.name, Linkage::Import, &sig)
            .map_err(|e| e.to_string())?;
        func_ids.insert(e.name.clone(), id);
    }

    let print_extra_id = declare_unary_import(
        &mut module,
        call_conv,
        match return_type {
            Type::Any => "sol_print_any",
            Type::String => "sol_print_string",
            _ => "sol_print_nil",
        },
        types::I64,
    )?;
    let inlinable = codegen::compute_inlinable(&program);
    let functions: HashMap<String, &TFunction> = program
        .functions
        .iter()
        .map(|f| (f.name.clone(), f))
        .collect();
    let prog_ctx = codegen::ProgramCtx {
        func_ids: &func_ids,
        functions: &functions,
        inlinable: &inlinable,
        runtime: &runtime_funcs,
    };

    let mut builder_ctx = FunctionBuilderContext::new();
    for f in &program.functions {
        let mut ctx = module.make_context();
        codegen::compile_function(&mut module, &mut builder_ctx, &mut ctx.func, f, &prog_ctx)?;
        let id = func_ids[&f.name];
        module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        module.clear_context(&mut ctx);
    }

    compile_entry(
        &mut module,
        &mut builder_ctx,
        func_ids["main"],
        gc_init_id,
        print_i64_id,
        print_f64_id,
        print_bool_id,
        print_extra_id,
        call_conv,
        &return_type,
    )?;

    let bytes = module.finish().emit().map_err(|e| e.to_string())?;
    link(&bytes, output_path)
}

fn declare_void_import(
    module: &mut ObjectModule,
    call_conv: CallConv,
    name: &str,
) -> Result<FuncId, String> {
    let sig = Signature::new(call_conv);
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(|e| e.to_string())
}

fn declare_unary_import(
    module: &mut ObjectModule,
    call_conv: CallConv,
    name: &str,
    param: cranelift_codegen::ir::Type,
) -> Result<FuncId, String> {
    let mut sig = Signature::new(call_conv);
    sig.params.push(AbiParam::new(param));
    module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(|e| e.to_string())
}

/// A hand-written C-ABI `main() -> i32`: init the GC's stack base, call sol's
/// `main`, print its result via the matching `sol_print_*` helper, return 0.
/// Small and fixed enough not to need `codegen.rs`'s general expression translation.
#[allow(clippy::too_many_arguments)]
fn compile_entry(
    module: &mut ObjectModule,
    builder_ctx: &mut FunctionBuilderContext,
    sol_main_id: FuncId,
    gc_init_id: FuncId,
    print_i64_id: FuncId,
    print_f64_id: FuncId,
    print_bool_id: FuncId,
    print_extra_id: FuncId,
    call_conv: CallConv,
    return_type: &Type,
) -> Result<(), String> {
    let mut sig = Signature::new(call_conv);
    sig.returns.push(AbiParam::new(types::I32));
    let entry_id = module
        .declare_function("main", Linkage::Export, &sig)
        .map_err(|e| e.to_string())?;

    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    let frontend_config = module.target_config();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, builder_ctx);
        let block = builder.create_block();
        builder.switch_to_block(block);
        builder.seal_block(block);

        let gc_init_ref = module.declare_func_in_func(gc_init_id, builder.func);
        builder.ins().call(gc_init_ref, &[]);

        let main_ref = module.declare_func_in_func(sol_main_id, builder.func);
        let call = builder.ins().call(main_ref, &[]);
        let result = builder.inst_results(call)[0];

        let (print_id, arg) = match return_type {
            Type::I64 => (print_i64_id, result),
            Type::F64 => (print_f64_id, result),
            Type::Bool => (print_bool_id, builder.ins().uextend(types::I64, result)),
            Type::Any | Type::Nil | Type::String => (print_extra_id, result),
            Type::Array(_) | Type::Map(_, _) | Type::Struct(_) | Type::Function { .. } => {
                unreachable!("rejected by sol::compile")
            }
        };
        let print_ref = module.declare_func_in_func(print_id, builder.func);
        builder.ins().call(print_ref, &[arg]);

        let zero = builder.ins().iconst(types::I32, 0);
        builder.ins().return_(&[zero]);
        builder.finalize(frontend_config);
    }
    module
        .define_function(entry_id, &mut ctx)
        .map_err(|e| e.to_string())?;
    module.clear_context(&mut ctx);
    Ok(())
}

/// Writes `object_bytes` to a temp `.o` and shells out to `cc` to link it
/// against this crate's own staticlib (built alongside the running
/// `sol` binary itself - see `Cargo.toml`'s `[lib]` `crate-type`).
fn link(object_bytes: &[u8], output_path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("sol binary has no parent directory")?;
    let staticlib = dir.join("libsol.a");
    if !staticlib.exists() {
        return Err(format!(
            "runtime staticlib not found at {} - `sol build` requires it built alongside the `sol` binary (`cargo build` builds both by default)",
            staticlib.display()
        ));
    }

    let obj_path = std::env::temp_dir().join(format!("sol_aot_{}.o", std::process::id()));
    std::fs::write(&obj_path, object_bytes).map_err(|e| e.to_string())?;

    let status = Command::new("cc")
        .arg(&obj_path)
        .arg(&staticlib)
        .arg("-o")
        .arg(output_path)
        .status()
        .map_err(|e| format!("failed to invoke 'cc': {e}"))?;
    std::fs::remove_file(&obj_path).ok();

    if !status.success() {
        return Err(format!("linking failed (cc exit status: {status})"));
    }
    Ok(())
}
