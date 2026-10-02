// Integration tests: run the sol binary as a subprocess against each
// fixture and assert on its printed output.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Once;

macro_rules! env {
    ($name:literal) => {
        ::std::env!($name)
    };
}

fn ensure_staticlib() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let status = Command::new(::std::env!("CARGO"))
            .args([
                "build",
                "--offline",
                "--manifest-path",
                concat!(::std::env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
            ])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "failed to build Sol's AOT runtime static library"
        );
    });
}

fn run_fixture(name: &str) -> (String, String, bool) {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .expect("failed to spawn the sol binary");
    (
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
        output.status.success(),
    )
}

#[test]
fn sum_array_sums_five_elements() {
    let (stdout, stderr, ok) = run_fixture("sum_array.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "15");
}

#[test]
fn fib_computes_the_32nd_fibonacci_number() {
    let (stdout, stderr, ok) = run_fixture("fib.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "2178309");
}

#[test]
fn typed_bytecode_reuses_frames_for_proper_tail_calls() {
    let path = format!(
        "{}/tests/fixtures/typed_tail_call.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_PROMOTE_THRESHOLD", "4294967295")
        .env("SOL_OSR_THRESHOLD", "4294967295")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "100000");
}

#[test]
fn fn_alias_works_for_exported_extern_top_level_and_local_functions() {
    let path = format!(
        "{}/tests/fixtures/fn_keyword.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("fn_keyword.sol");
    assert!(status.success());
    assert_eq!(stdout, "42");
}

#[test]
fn mutually_recursive_top_level_functions_both_called_directly_from_main_link_and_run() {
    let path = format!(
        "{}/tests/fixtures/mutual_recursion.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "100",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("mutual_recursion.sol");
    assert!(status.success());
    assert_eq!(stdout, "100");
}

#[test]
fn bool_logic_short_circuits_correctly_enough_for_this_case() {
    let (stdout, stderr, ok) = run_fixture("bool_logic.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "true");
}

#[test]
fn do_end_blocks_create_a_scope() {
    let (stdout, stderr, ok) = run_fixture("do_blocks.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "42");
}

#[test]
fn structural_records_use_fixed_layout_in_bytecode_native_and_aot() {
    let path = format!(
        "{}/tests/fixtures/structural_records.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("structural_records.sol");
    assert!(status.success());
    assert_eq!(stdout, "42");
}

#[test]
fn typed_maps_handle_resize_collision_and_missing_keys_in_all_tiers() {
    let path = format!("{}/tests/fixtures/maps.sol", env!("CARGO_MANIFEST_DIR"));
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "808012",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("maps.sol");
    assert!(status.success());
    assert_eq!(stdout, "808012");
}

#[test]
fn three_typed_modules_agree_in_bytecode_native_osr_and_aot() {
    let path = format!(
        "{}/tests/fixtures/modules/main.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("modules/main.sol");
    assert!(status.success());
    assert_eq!(stdout, "42");
}

#[test]
fn typed_imports_and_dynamic_require_share_one_module_instance() {
    let path = format!(
        "{}/tests/fixtures/mixed_modules/main.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_REQUIRE_UNIFIED_MIXED", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn dynamic_module_contracts_are_explicit_and_canonicalized_once() {
    let directory =
        std::env::temp_dir().join(format!("sol_mixed_contract_test_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import left\nimport right\nfunction main(): i64 return left.value() + right.value() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("left.sol"),
        "import shared\nexport function value(): i64 return shared.answer() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("right.sol"),
        "import shared\nexport function value(): i64 return shared.answer() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("shared.lua"),
        "function answer(): i64 local box = { value = 21 } return box.value end\nfunction inferred() return 1 end\n",
    )
    .unwrap();

    let root = directory.join("main.sol");
    let source = std::fs::read(&root).unwrap();
    let project = sol::modules::load_project_program(root.to_str().unwrap(), &source).unwrap();
    assert_eq!(
        project
            .program
            .functions
            .iter()
            .filter(|function| function.name == "shared.answer")
            .count(),
        1,
        "the canonical module path must be loaded only once"
    );
    assert_eq!(
        project.dynamic_contracts,
        vec![sol::modules::DynamicModuleContract {
            name: "shared".into(),
            exports: vec!["answer".into()],
        }]
    );

    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", root.to_str().unwrap()])
        .env("SOL_REQUIRE_UNIFIED_MIXED", "1")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&directory).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn dynamic_module_contract_checks_the_return_value_at_the_boundary() {
    let directory = std::env::temp_dir().join(format!(
        "sol_mixed_contract_error_test_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import dynamic\nfunction main(): i64 return dynamic.answer() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("dynamic.lua"),
        "function answer(): i64 local box = { value = 'wrong' } return box.value end\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", directory.join("main.sol").to_str().unwrap()])
        .env("SOL_REQUIRE_UNIFIED_MIXED", "1")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&directory).ok();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("dynamic function 'dynamic.answer' returned string, expected I64"),
        "{error}"
    );
}

#[test]
fn unused_dynamic_support_does_not_change_proven_main_bytecode() {
    let compile_main = |source: &str| {
        let tokens = sol::lexer::lex(source).unwrap();
        let mut program =
            sol::parser::parse_with_config(tokens, sol::parser::LanguageConfig::SOL).unwrap();
        sol::aliases::expand(&mut program).unwrap();
        sol::closures::lower(&mut program).unwrap();
        let partition = sol::typeck::check_partitioned(&program).unwrap();
        let main = partition
            .native
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        let ids = sol::bccompile::function_index(&partition.native).unwrap();
        sol::bccompile::compile_function(main, &ids).unwrap()
    };

    let baseline = compile_main("function main(): i64 return 6 * 7 end");
    let with_unused_dynamic = compile_main(
        "function unused(value) local box = { value = value } return box.value end\n\
         function main(): i64 return 6 * 7 end",
    );
    assert_eq!(
        baseline
            .code
            .iter()
            .map(|instruction| instruction.0)
            .collect::<Vec<_>>(),
        with_unused_dynamic
            .code
            .iter()
            .map(|instruction| instruction.0)
            .collect::<Vec<_>>()
    );
    assert_eq!(baseline.consts, with_unused_dynamic.consts);
}

#[test]
fn immutable_nonescaping_closures_are_lambda_lifted_in_all_tiers() {
    let path = format!("{}/tests/fixtures/closures.sol", env!("CARGO_MANIFEST_DIR"));
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "70",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("closures.sol");
    assert!(status.success());
    assert_eq!(stdout, "70");

    let ir = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-ir", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(ir.status.success());
    let stderr = String::from_utf8_lossy(&ir.stderr);
    assert!(
        !stderr.contains("sol_alloc"),
        "closure path allocated:\n{stderr}"
    );
}

#[test]
fn aliases_aggregate_any_and_narrowing_agree_in_all_tiers() {
    let path = format!(
        "{}/tests/fixtures/m12_narrowing.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "96",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("m12_narrowing.sol");
    assert!(status.success());
    assert_eq!(stdout, "96");
}

#[test]
fn aliases_and_checked_narrowing_report_invalid_programs() {
    let directory = std::env::temp_dir();
    let cycle_path = directory.join(format!("sol_alias_cycle_{}.sol", std::process::id()));
    std::fs::write(
        &cycle_path,
        "type First = Second\ntype Second = First\nfunction main(): i64 return 0 end\n",
    )
    .unwrap();
    let cycle = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", cycle_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&cycle_path).ok();
    let cycle_error = String::from_utf8_lossy(&cycle.stderr);
    assert!(!cycle.status.success());
    assert!(cycle_error.contains("type alias cycle"), "{cycle_error}");

    let cast_path = directory.join(format!("sol_bad_cast_{}.sol", std::process::id()));
    std::fs::write(
        &cast_path,
        "function main(): i64 local value: any = true return value as i64 end\n",
    )
    .unwrap();
    let cast = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", cast_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&cast_path).ok();
    assert!(
        !cast.status.success(),
        "mismatched checked cast did not trap"
    );
}

#[test]
fn generic_map_over_i64_arrays_is_specialized_in_all_tiers() {
    let path = format!(
        "{}/tests/fixtures/generic_map.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("generic_map.sol");
    assert!(status.success());
    assert_eq!(stdout, "42");

    let ir = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-ir", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(ir.status.success());
    let stderr = String::from_utf8_lossy(&ir.stderr);
    assert!(
        !stderr.contains("sol_dynamic"),
        "map used dynamic dispatch:\n{stderr}"
    );
}

#[test]
fn generic_map_over_f64_arrays_is_specialized_in_all_tiers() {
    let path = format!(
        "{}/tests/fixtures/generic_map_f64.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "16",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("generic_map_f64.sol");
    assert!(status.success());
    assert_eq!(stdout, "16");

    let ir = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-ir", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(ir.status.success());
    let stderr = String::from_utf8_lossy(&ir.stderr);
    assert!(
        !stderr.contains("sol_dynamic"),
        "map used dynamic dispatch:\n{stderr}"
    );
    assert!(
        !stderr.contains("sol_array_map_i64"),
        "f64 map specialization called the i64 runtime helper:\n{stderr}"
    );
}

#[test]
fn generic_map_rejects_a_callback_whose_type_does_not_match_the_array_element_type() {
    let (_, stderr, ok) = run_fixture("generic_map_type_mismatch.sol");
    assert!(!ok, "expected a type-check error, program ran successfully");
    assert!(
        stderr.contains("map"),
        "expected a map-related type error, got:\n{stderr}"
    );
}

#[test]
fn maps_support_unboxed_float_and_bool_values_in_all_tiers() {
    let path = format!(
        "{}/tests/fixtures/map_scalar_values.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote, osr) in [
        ("bytecode", "4294967295", "4294967295"),
        ("native", "1", "4294967295"),
        ("OSR", "4294967295", "1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .env("SOL_OSR_THRESHOLD", osr)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
    let (stdout, status) = build_and_run("map_scalar_values.sol");
    assert!(status.success());
    assert_eq!(stdout, "42");
}

#[test]
fn typed_closures_reject_inner_capture_mutation_and_escape_before_codegen() {
    let directory = std::env::temp_dir();
    let mutable_path = directory.join(format!("sol_mutable_capture_{}.sol", std::process::id()));
    std::fs::write(
        &mutable_path,
        "function main(): i64 local value: i64 = 1 local function read(): i64 value = value + 1 return value end return read() end",
    )
    .unwrap();
    let mutable = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", mutable_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&mutable_path).ok();
    let error = String::from_utf8_lossy(&mutable.stderr);
    assert!(!mutable.status.success());
    assert!(
        error.contains("captured variable 'value' is assigned inside the closure"),
        "{error}"
    );

    let escape_path = directory.join(format!("sol_escaping_capture_{}.sol", std::process::id()));
    std::fs::write(
        &escape_path,
        "function main(): i64 local value: i64 = 1 local function read(): i64 return value end local callback: fn() -> i64 = read return callback() end",
    )
    .unwrap();
    let escaping = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", escape_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&escape_path).ok();
    let error = String::from_utf8_lossy(&escaping.stderr);
    assert!(!escaping.status.success());
    assert!(error.contains("local function 'read' escapes"), "{error}");
}

#[test]
fn module_graph_reports_missing_private_and_cyclic_imports_and_prefers_sol() {
    let directory = std::env::temp_dir().join(format!("sol_modules_test_{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let run = |name: &str| {
        Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", directory.join(name).to_str().unwrap()])
            .output()
            .unwrap()
    };

    std::fs::write(
        directory.join("main.sol"),
        "import chosen\nfunction main(): i64 return chosen.value() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("chosen.sol"),
        "export function value(): i64 return 42 end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("chosen.lua"),
        "this is deliberately not typed Sol",
    )
    .unwrap();
    let precedence = run("main.sol");
    assert!(precedence.status.success());
    assert_eq!(String::from_utf8_lossy(&precedence.stdout).trim(), "42");

    std::fs::write(
        directory.join("main.sol"),
        "import absent\nfunction main(): i64 return 0 end\n",
    )
    .unwrap();
    let missing = run("main.sol");
    let missing_error = String::from_utf8_lossy(&missing.stderr);
    assert!(!missing.status.success());
    assert!(
        missing_error.contains("module 'absent' not found"),
        "{missing_error}"
    );
    assert!(missing_error.contains("absent.sol"), "{missing_error}");
    assert!(missing_error.contains("absent.lua"), "{missing_error}");

    std::fs::write(
        directory.join("private.sol"),
        "function hidden(): i64 return 1 end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import private\nfunction main(): i64 return private.hidden() end\n",
    )
    .unwrap();
    let private = run("main.sol");
    let private_error = String::from_utf8_lossy(&private.stderr);
    assert!(!private.status.success());
    assert!(
        private_error.contains("does not export 'hidden'"),
        "{private_error}"
    );
    assert!(
        private_error.contains("private declaration"),
        "{private_error}"
    );

    std::fs::write(
        directory.join("private.sol"),
        "struct Hidden { value: i64 }\nexport function make(): Hidden return Hidden { value = 1 } end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import private\nfunction main(): i64 local value: private.Hidden = private.make() return value.value end\n",
    )
    .unwrap();
    let private_type = run("main.sol");
    let private_type_error = String::from_utf8_lossy(&private_type.stderr);
    assert!(!private_type.status.success());
    assert!(
        private_type_error.contains("does not export 'Hidden'"),
        "{private_type_error}"
    );
    assert!(
        private_type_error.contains("private declaration"),
        "{private_type_error}"
    );

    std::fs::write(
        directory.join("duplicate.sol"),
        "export function value(): i64 return 1 end\nexport function value(): i64 return 2 end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import duplicate\nfunction main(): i64 return duplicate.value() end\n",
    )
    .unwrap();
    let duplicate = run("main.sol");
    let duplicate_error = String::from_utf8_lossy(&duplicate.stderr);
    assert!(!duplicate.status.success());
    assert!(
        duplicate_error.contains("duplicate export 'value'"),
        "{duplicate_error}"
    );

    std::fs::write(
        directory.join("bad.sol"),
        "export function value(): i64 return true end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import bad\nfunction main(): i64 return bad.value() end\n",
    )
    .unwrap();
    let imported_error = run("main.sol");
    let imported_error_text = String::from_utf8_lossy(&imported_error.stderr);
    assert!(!imported_error.status.success());
    assert!(
        imported_error_text.contains("bad.sol"),
        "{imported_error_text}"
    );
    assert!(
        imported_error_text.contains("expected type i64, found bool"),
        "{imported_error_text}"
    );

    std::fs::write(
        directory.join("init.sol"),
        "export function value(): i64 return 42 end\nlocal zero: i64 = 0\nlocal broken: i64 = 1 / zero\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import init\nfunction main(): i64 return init.value() end\n",
    )
    .unwrap();
    let initializer = run("main.sol");
    assert!(
        !initializer.status.success(),
        "module initializer was not executed"
    );

    std::fs::write(
        directory.join("a.sol"),
        "import b\nexport function a(): i64 return b.b() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("b.sol"),
        "import a\nexport function b(): i64 return a.a() end\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("main.sol"),
        "import a\nfunction main(): i64 return a.a() end\n",
    )
    .unwrap();
    let cycle = run("main.sol");
    let cycle_error = String::from_utf8_lossy(&cycle.stderr);
    assert!(!cycle.status.success());
    assert!(cycle_error.contains("import cycle"), "{cycle_error}");
    assert!(cycle_error.contains("a.sol"), "{cycle_error}");
    assert!(cycle_error.contains("b.sol"), "{cycle_error}");

    std::fs::remove_dir_all(&directory).ok();
}

#[test]
fn typed_function_values_and_callbacks_work_in_bytecode_and_native_tiers() {
    let path = format!(
        "{}/tests/fixtures/function_values.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, threshold) in [("bytecode", "4294967295"), ("native", "1")] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", threshold)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "42",
            "{tier}"
        );
    }
}

#[test]
fn a_program_with_no_main_is_rejected_with_a_clear_error() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("sol_no_main_test_{}.sol", std::process::id()));
    std::fs::write(&path, "function helper(): i64\n    return 1\nend\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no 'main' function found"),
        "stderr was: {stderr}"
    );
}

#[test]
fn a_type_mismatch_is_rejected_with_a_clear_error() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("sol_type_error_test_{}.sol", std::process::id()));
    std::fs::write(&path, "function main(): i64\n    return true\nend\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let json_output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--diagnostic-format", "json", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected type i64, found bool"),
        "stderr was: {stderr}"
    );
    assert!(stderr.contains("[ETYPE001]"), "stderr was: {stderr}");
    assert!(stderr.contains(".sol:2:1"), "stderr was: {stderr}");
    assert!(stderr.contains("return true\n  ^"), "stderr was: {stderr}");
    assert!(!json_output.status.success());
    let json = String::from_utf8_lossy(&json_output.stderr);
    assert!(json.starts_with("{\"severity\":\"error\""), "{json}");
    assert!(json.contains("\"line\":2,\"column\":1"), "{json}");
    assert!(json.contains("\"code\":\"ETYPE001\""), "{json}");
    assert!(json.contains("sol_type_error_test_"), "{json}");
}

#[test]
fn function_source_ranges_survive_typed_and_bytecode_lowering() {
    let source = b"\n  fn main(): i64\n    return 42\n  end\n";
    let parsed = sol::parser::parse(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let function = &parsed.functions[0];
    assert_eq!(function.source_span.start, 3);
    assert_eq!(function.source_span.end, source.len() - 1);
    assert_eq!(
        (function.source_span.line, function.source_span.column),
        (2, 3)
    );

    let (typed, _) = sol::compile_bytes(source, sol::parser::SourceMode::Sol).unwrap();
    let function = &typed.functions[0];
    assert_eq!(function.source_span.start, 3);
    assert_eq!(function.source_span.end, source.len() - 1);
    let ids = sol::bccompile::function_index(&typed).unwrap();
    let bytecode = sol::bccompile::compile_function(function, &ids).unwrap();
    assert_eq!(bytecode.source_span, function.source_span);
}

/// Out-of-bounds array access must trap, not silently read garbage.
#[test]
fn out_of_bounds_array_access_traps_instead_of_reading_garbage() {
    let path = format!(
        "{}/tests/fixtures/out_of_bounds.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "out-of-bounds access must not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(1),
        "must be a trap/crash (no exit code, killed by a signal), not a normal exit(1) error path"
    );
}

/// A non-escaping struct local must compile to zero heap allocation - checked at the IR level.
#[test]
fn non_escaping_struct_local_has_no_allocation_in_the_emitted_ir() {
    let path = format!(
        "{}/tests/fixtures/scalar_replace_check.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_DUMP_CLIF", "1")
        .env("SOL_PROMOTE_THRESHOLD", "1") // force promotion so there's something to dump
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "25");
    let clif = String::from_utf8_lossy(&output.stderr);
    assert!(
        !clif.contains("call fn"),
        "expected no allocation call in a fully scalar-replaced function:\n{clif}"
    );
    assert!(
        !clif.contains("store"),
        "expected no stores at all once the struct's fields become plain locals:\n{clif}"
    );
}

#[test]
fn struct_that_escapes_via_a_function_call_keeps_its_allocation() {
    let path = format!("{}/tests/fixtures/structs.sol", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_DUMP_CLIF", "1")
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let clif = String::from_utf8_lossy(&output.stderr);
    assert!(
        clif.contains("call fn"),
        "expected an allocation call since `p` is passed by value to dist_squared:\n{clif}"
    );
}

#[test]
fn struct_literal_field_access_and_field_assignment_work() {
    // p = {x=3, y=4}; p.x += 1 -> {x=4, y=4}; dist_squared = 4*4+4*4 = 32
    let (stdout, stderr, ok) = run_fixture("structs.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "32");
}

#[test]
fn a_small_function_called_from_multiple_sites_inlines_correctly_at_each() {
    // 4 + 9 + 16 = 29
    let (stdout, stderr, ok) = run_fixture("inlining.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "29");
}

#[test]
fn a_loop_not_matching_the_elimination_pattern_is_still_correctly_bounds_checked() {
    let (stdout, stderr, ok) = run_fixture("bounds_check_general_case.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "20");
}

/// Cranelift's sdiv/srem already trap on a zero divisor - no extra check needed.
#[test]
fn integer_division_by_zero_traps() {
    let path = format!(
        "{}/tests/fixtures/division_by_zero.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "division by zero must not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(1),
        "must be a trap/crash, not a normal exit(1) error path"
    );
}

#[test]
fn negative_array_index_traps() {
    let path = format!(
        "{}/tests/fixtures/negative_index.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a negative index must not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(1),
        "must be a trap/crash, not a normal exit(1) error path"
    );
}

#[test]
fn negative_array_lengths_trap_in_bytecode_native_and_aot() {
    let path = format!(
        "{}/tests/fixtures/negative_array_length.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for (tier, promote) in [("bytecode", "4294967295"), ("native", "1")] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", promote)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{tier} accepted a negative length"
        );
    }
    let (_, status) = build_and_run("negative_array_length.sol");
    assert!(!status.success(), "AOT accepted a negative length");
}

/// A live array must survive many GC cycles while garbage is reclaimed.
/// Regression test for a callee-saved-register root the conservative scan once missed
/// (`gc.rs::flush_callee_saved_registers`) - run repeatedly since register allocation varies.
#[test]
fn a_live_array_survives_many_collections_while_garbage_is_reclaimed() {
    for _ in 0..5 {
        let (stdout, stderr, ok) = run_fixture("gc_stress.sol");
        assert!(ok, "stderr: {stderr}");
        assert_eq!(stdout, "1110");
    }
}

/// Same regression, for struct allocations instead of arrays.
#[test]
fn a_live_struct_survives_many_collections_while_garbage_is_reclaimed() {
    for _ in 0..5 {
        let (stdout, stderr, ok) = run_fixture("gc_stress_structs.sol");
        assert!(ok, "stderr: {stderr}");
        assert_eq!(stdout, "333");
    }
}

/// A scalar array's data buffer must never be conservatively scanned (it's atomic) - checked via SOL_GC_DEBUG's counter.
#[test]
fn allocating_a_large_array_does_not_conservatively_scan_its_contents() {
    let path = format!(
        "{}/tests/fixtures/large_array_atomic.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_GC_DEBUG", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "999999");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines().filter(|l| l.contains("words_scanned")) {
        assert!(
            line.contains("words_scanned=0"),
            "expected the large array's contents to never be scanned (it's atomic): {line}"
        );
    }
}

/// Array data must read as zero even when its chunk position is reused by a later allocation.
#[test]
fn array_data_reads_as_zero_even_after_many_collections_reuse_the_chunk() {
    for _ in 0..5 {
        let (stdout, stderr, ok) = run_fixture("array_zero_across_reuse.sol");
        assert!(ok, "stderr: {stderr}");
        assert_eq!(stdout, "0");
    }
}

/// Core generational-GC correctness property: once a struct has been
/// promoted to `Old` (by surviving a minor collection), reassigning one of
/// its fields to a brand-new `Young` array must still be recorded by
/// `sol_gc_write_barrier` - otherwise a later minor collection's
/// remembered-set scan (not a full trace) would miss that `Old -> Young`
/// edge and reclaim the array out from under the still-live struct.
#[test]
fn a_struct_field_reassigned_to_a_young_array_after_promotion_survives_minor_collections() {
    for _ in 0..5 {
        let (stdout, stderr, ok) = run_fixture("gc_struct_field_write_barrier.sol");
        assert!(ok, "stderr: {stderr}");
        assert_eq!(stdout, "1110");
    }
}

/// Array-atomicity regression: `Array<Point>`'s data buffer holds real
/// pointers and must go through `sol_new_array_ptr` (traced), not the
/// atomic scalar-array path - otherwise the collector would never scan
/// into it and the pointed-to structs would be reclaimed as garbage.
#[test]
fn an_array_of_structs_survives_many_collections_while_garbage_is_reclaimed() {
    for _ in 0..5 {
        let (stdout, stderr, ok) = run_fixture("gc_array_of_structs.sol");
        assert!(ok, "stderr: {stderr}");
        assert_eq!(stdout, "21");
    }
}

/// Combined stress test exercising both collection tiers together: many
/// minor collections (short-lived garbage every iteration) plus write-barrier
/// traffic on an already-promoted struct's field, run long enough to also
/// force at least one full major collection (`collect_heap`) given this
/// collector's coarse per-chunk promotion granularity.
#[test]
fn generational_gc_stress_survives_many_minor_and_major_collections() {
    let path = format!(
        "{}/tests/fixtures/gc_generational_stress.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_GC_DEBUG", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "199000");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().any(|l| l.contains("[gc] collect:")),
        "expected at least one major collection to fire during this stress test: {stderr}"
    );
}

/// A strict function calling a dynamic (`any` param/return) one round-trips a value through boxing correctly.
#[test]
fn any_typed_function_boundary_boxes_and_unboxes_correctly() {
    let (stdout, stderr, ok) = run_fixture("any_roundtrip.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "42");
}

/// Speculative `any`-parameter specialization: interpreted and guarded-native calls must agree on the arithmetic.
#[test]
fn speculative_any_parameter_specializes_and_stays_correct() {
    let (stdout, stderr, ok) = run_fixture("speculative_any_param.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "3540");
}

/// Forces specialization on the first call and confirms it actually compiled (via SOL_DUMP_CLIF), not just "would've been correct anyway."
#[test]
fn speculative_any_parameter_specialization_is_visible_in_dumped_ir() {
    let path = format!(
        "{}/tests/fixtures/speculative_any_param.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_DUMP_CLIF", "1")
        .env("SOL_SPECULATIVE_THRESHOLD", "1")
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .env("SOL_OSR_THRESHOLD", "100000")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3540");
    let clif = String::from_utf8_lossy(&output.stderr);
    assert!(
        clif.contains("double__spec0"),
        "expected double's speculative i64 variant (tag 0) to be compiled:\n{clif}"
    );
}

/// U11 item 2: `triple`'s only call site boxes its argument directly inline
/// (`triple(i)`), so `jit::is_speculative_exhaustive`'s whole-program proof
/// can show every call always passes `i64` - `SOL_JIT_LOG` should report the
/// candidate as proven exhaustive, and the (guard-skipped) result must still
/// be correct.
#[test]
fn speculative_exhaustive_candidate_proof_fires_and_skips_the_guard() {
    let path = format!(
        "{}/tests/fixtures/speculative_exhaustive_any_param.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_JIT_LOG", "1")
        .env("SOL_SPECULATIVE_THRESHOLD", "1")
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .env("SOL_OSR_THRESHOLD", "100000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "5310");
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        log.contains("'triple' speculative candidate proven exhaustive"),
        "expected triple's candidate to be proven exhaustive:\n{log}"
    );
}

/// U11 item 2 scope cut: `double`'s one call site passes its argument
/// through an intermediate `any`-typed local (`local boxed: any = i; ...
/// double(boxed)`), not a direct `any(i64)` boxing at the call site itself -
/// a value `jit::is_speculative_exhaustive` conservatively declines to
/// prove (see its doc comment), so the per-call guard must stay in place.
/// Existing correctness (the guarded path still works) is already covered
/// by `speculative_any_parameter_specializes_and_stays_correct`.
#[test]
fn speculative_candidate_with_indirect_any_argument_is_not_proven_exhaustive() {
    let path = format!(
        "{}/tests/fixtures/speculative_any_param.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_JIT_LOG", "1")
        .env("SOL_SPECULATIVE_THRESHOLD", "1")
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .env("SOL_OSR_THRESHOLD", "100000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3540");
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        !log.contains("'double' speculative candidate proven exhaustive"),
        "double's candidate is called through an indirect any-typed local and must not be proven exhaustive:\n{log}"
    );
}

/// U11 item 2 soundness regression: `narrow`'s candidate (unboxes to `i64`)
/// is called 40 times with `i64` (warming it past the specialization
/// threshold), then once with an `f64` boxed directly at that call site.
/// The mismatched call site must disqualify the whole-program proof, and
/// the mismatched call must still trap - confirming the proof can never
/// turn a real type mismatch into silent bit-misinterpretation.
#[test]
fn speculative_candidate_type_mismatch_is_not_proven_exhaustive_and_still_traps() {
    let path = format!(
        "{}/tests/fixtures/speculative_type_mismatch_traps.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_JIT_LOG", "1")
        .env("SOL_SPECULATIVE_THRESHOLD", "1")
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .env("SOL_OSR_THRESHOLD", "100000")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a type mismatch on narrow's speculative candidate must not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(1),
        "must be a trap/crash, not a normal exit(1) error path"
    );
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        !log.contains("'narrow' speculative candidate proven exhaustive"),
        "narrow is called with mismatched types across call sites and must not be proven exhaustive:\n{log}"
    );
}

/// `--dump-ir`'s CLIF text only ever prints callees as opaque `u0:N`
/// module-function-id references (Cranelift's `Function` `Display` never
/// prints a linkage name) - so a plain `stderr.contains("sol_dynamic_binary")`
/// style assertion can never fail regardless of whether that call is
/// actually present, unless something resolves `u0:N` back to a name.
/// `dump_clif_with_legend` (jit.rs) does this by walking each dumped
/// function's external-function table and printing `"; fnN = <real name>"`
/// for every callee. This fixture forces a genuine, non-inlined,
/// non-narrowed `any + any` (see its comment), so this test both proves the
/// legend resolves real runtime calls by name and guards against the
/// resolution silently regressing back into a vacuous check.
#[test]
fn dumped_ir_legend_resolves_runtime_calls_to_their_real_names() {
    let path = format!(
        "{}/tests/fixtures/dynamic_dispatch_probe.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-ir", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .env("SOL_OSR_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "11");
    let clif = String::from_utf8_lossy(&output.stderr);
    assert!(
        clif.contains("= sol_dynamic_binary"),
        "expected add_any's `a + b` on two any-typed values to resolve to a \
         named sol_dynamic_binary call in the dumped IR:\n{clif}"
    );
}

/// Unboxing an `any` value as the wrong type must trap, not silently read garbage.
#[test]
fn unboxing_any_as_the_wrong_type_traps() {
    let path = format!(
        "{}/tests/fixtures/any_type_mismatch_traps.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a type mismatch on unbox must not succeed"
    );
    assert_ne!(
        output.status.code(),
        Some(1),
        "must be a trap/crash, not a normal exit(1) error path"
    );
}

/// --dump-ir/--dump-asm/--jit-log all produce their expected output.
#[test]
fn dump_ir_dump_asm_and_jit_log_cli_flags_all_produce_output() {
    let path = format!("{}/tests/fixtures/fib.sol", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-ir", "--dump-asm", "--jit-log", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2178309");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("==== fib ===="),
        "expected --dump-ir's CLIF output:\n{stderr}"
    );
    assert!(
        stderr.contains("(asm)"),
        "expected --dump-asm's VCode output:\n{stderr}"
    );
    assert!(
        stderr.contains("[jit] promoting 'main'"),
        "expected --jit-log's promotion trace:\n{stderr}"
    );
}

/// --target-info reports real, architecture-appropriate detected CPU features.
#[test]
fn target_info_cli_flag_reports_detected_host_isa_features() {
    let path = format!("{}/tests/fixtures/fib.sol", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--target-info", &path])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[target] isa="),
        "expected --target-info's ISA summary line:\n{stderr}"
    );
    assert!(
        stderr.lines().filter(|l| l.contains("[target]   ")).count() > 0,
        "expected at least one detected ISA-specific setting:\n{stderr}"
    );
}

/// An unrecognized flag must be a clear usage error, not silently ignored.
#[test]
fn an_unrecognized_flag_is_a_clear_usage_error() {
    let path = format!("{}/tests/fixtures/fib.sol", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-bogus", &path])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
}

/// M7 §25 FFI: `extern function` calls a real libc/libm symbol - checked
/// through both tiers, since interpreted and promoted-native calls resolve
/// externs via different code paths (Op::Call's Slot::Native vs. a direct
/// Cranelift call).
#[test]
fn ffi_extern_function_calls_a_real_libc_symbol() {
    let (stdout, stderr, ok) = run_fixture("ffi_libm.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "1036"); // sqrt(144) + pow(2, 10) = 12 + 1024
}

#[test]
fn ffi_extern_function_works_once_the_caller_is_promoted_to_native() {
    let path = format!("{}/tests/fixtures/ffi_libm.sol", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1036");
}

/// An extern signature using a non-scalar type (only i64/f64/bool cross
/// the FFI boundary so far) must be a clear compile error.
#[test]
fn an_extern_with_a_non_scalar_type_is_a_clear_error() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "sol_extern_bad_type_test_{}.sol",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "extern function f(a: Array<i64>): i64\nfunction main(): i64\n    return 0\nend\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only i64/f64/bool cross the FFI boundary"),
        "stderr was: {stderr}"
    );
}

/// M7 §22 AOT: `sol build` produces a real standalone executable
/// (requires the `libsol.a` staticlib `cargo build` builds alongside
/// the `sol` binary - see aot.rs's `link`). Runs the produced binary
/// itself, not the sol compiler, and checks it prints the same answer
/// `run` would - covering arrays, structs+GC, `any`, and FFI, the same
/// fixtures already exercised through the tiered JIT.
fn build_and_run(fixture: &str) -> (String, std::process::ExitStatus) {
    ensure_staticlib();
    let src = format!("{}/tests/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR"));
    let safe_fixture = fixture.replace('/', "_");
    let out = std::env::temp_dir().join(format!(
        "sol_aot_test_{}_{}",
        std::process::id(),
        safe_fixture
    ));
    let build_output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["build", &src, "-o", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        build_output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );
    let run_output = Command::new(&out).output().unwrap();
    std::fs::remove_file(&out).ok();
    (
        String::from_utf8_lossy(&run_output.stdout)
            .trim()
            .to_string(),
        run_output.status,
    )
}

#[test]
fn aot_build_runs_a_standalone_executable_for_array_struct_gc_and_ffi_fixtures() {
    for (fixture, expected) in [
        ("sum_array.sol", "15"),
        ("structs.sol", "32"),
        ("gc_stress_structs.sol", "333"),
        ("ffi_libm.sol", "1036"),
        ("function_values.sol", "42"),
    ] {
        let (stdout, status) = build_and_run(fixture);
        assert!(status.success(), "{fixture} exited with {status}");
        assert_eq!(stdout, expected, "{fixture}");
    }
}

#[test]
fn aot_build_still_traps_correctly() {
    let (_, status) = build_and_run("division_by_zero.sol");
    assert!(!status.success());
    assert_ne!(
        status.code(),
        Some(1),
        "must be a trap/crash, not a normal exit(1) error path"
    );
}

/// M7 §23 PGO: `--profile-out` records what got promoted, and a later
/// `--profile-in` run preloads it - checked by setting the promotion
/// threshold impossibly high on the *second* run (so a correct answer is
/// only possible if the profile did the promoting, not the interpreter's
/// own counter).
#[test]
fn profile_out_then_profile_in_preloads_promotion_and_skips_warm_up() {
    let path = format!("{}/tests/fixtures/fib.sol", env!("CARGO_MANIFEST_DIR"));
    let prof = std::env::temp_dir().join(format!("sol_profile_test_{}.prof", std::process::id()));

    let record = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--profile-out", prof.to_str().unwrap(), &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(record.status.success());
    let profile = std::fs::read_to_string(&prof).unwrap();
    assert!(profile.contains("promoted fib"), "profile was: {profile}");
    assert!(profile.contains("promoted main"), "profile was: {profile}");

    let replay = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args([
            "run",
            "--profile-in",
            prof.to_str().unwrap(),
            "--jit-log",
            &path,
        ])
        .env("SOL_PROMOTE_THRESHOLD", "100000") // unreachable by fib.sol's single call - only the profile can promote
        .output()
        .unwrap();
    std::fs::remove_file(&prof).ok();
    assert!(replay.status.success());
    assert_eq!(String::from_utf8_lossy(&replay.stdout).trim(), "2178309");
    let stderr = String::from_utf8_lossy(&replay.stderr);
    assert!(
        stderr.contains("[jit] promoting 'fib'"),
        "expected the profile to preload fib's promotion:\n{stderr}"
    );
}

/// M7 §21: `codegen::try_vectorize_elementwise_loop`'s matched pattern
/// (`for i = 0, n-1 do c[i] = a[i] + b[i] end` over `f64` arrays) with an
/// odd element count, exercising both the vectorized main loop and its
/// scalar tail. Checked in both tiers - interpreted bytecode never goes
/// through codegen.rs at all, so agreement between the two is itself
/// evidence the vectorized path is correct, not just "happens to compile."
#[test]
fn vectorized_elementwise_loop_handles_the_scalar_tail_correctly() {
    let path = format!(
        "{}/tests/fixtures/vectorized_add.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for threshold in ["100000", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", threshold)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "84",
            "threshold={threshold}"
        );
    }
}

/// Same pattern, even element count (no scalar tail) and each of `+ - * /`
/// individually - interpreted and forced-native results must agree.
#[test]
fn vectorized_elementwise_loop_covers_all_four_arithmetic_ops() {
    let path = format!(
        "{}/tests/fixtures/vectorized_ops.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut results = Vec::new();
    for threshold in ["100000", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", threshold)
            .output()
            .unwrap();
        assert!(output.status.success());
        results.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    assert_eq!(results[0], results[1]);
}

/// The vectorized path's whole-range bounds check must still trap on a
/// genuinely out-of-bounds write, in both tiers.
#[test]
fn vectorized_elementwise_loop_still_traps_out_of_bounds() {
    let path = format!(
        "{}/tests/fixtures/vectorized_oob.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    for threshold in ["100000", "1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", &path])
            .env("SOL_PROMOTE_THRESHOLD", threshold)
            .output()
            .unwrap();
        assert!(!output.status.success(), "threshold={threshold}");
        assert_ne!(
            output.status.code(),
            Some(1),
            "must be a trap, not exit(1): threshold={threshold}"
        );
    }
}

/// The actual SIMD instruction must show up in the compiled code, not
/// just "the answer happens to be right either way."
#[test]
fn vectorized_elementwise_loop_emits_real_vector_instructions() {
    let path = format!(
        "{}/tests/fixtures/vectorized_add.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "--dump-asm", &path])
        .env("SOL_PROMOTE_THRESHOLD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let asm = String::from_utf8_lossy(&output.stderr);
    assert!(
        asm.contains(".2d") || asm.contains("xmm"),
        "expected a 2-lane f64 vector op (NEON `.2d` or SSE2 `xmm`):\n{asm}"
    );
}

/// M8: `--profile-time` writes a report with per-function call counts and
/// timings - checked with promotion disabled (`SOL_PROMOTE_THRESHOLD`
/// impossibly high) so every call routes through the interpreter's
/// dispatcher and the count is exact, not just "some subset saw hooks."
#[test]
fn profile_time_reports_accurate_call_counts_with_promotion_disabled() {
    let path = format!(
        "{}/tests/fixtures/debug_calls.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let report_path =
        std::env::temp_dir().join(format!("sol_time_report_{}.txt", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args([
            "run",
            "--profile-time",
            report_path.to_str().unwrap(),
            &path,
        ])
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "13");
    let report = std::fs::read_to_string(&report_path).unwrap();
    std::fs::remove_file(&report_path).ok();
    assert!(
        report.contains("main") && report.contains("add"),
        "report was:\n{report}"
    );
    // `add` is called exactly twice in debug_calls.sol.
    let add_line = report.lines().find(|l| l.starts_with("add ")).unwrap_or("");
    assert!(
        add_line.split_whitespace().nth(1) == Some("2"),
        "expected add's call count to be 2, report line was: {add_line}"
    );
}

/// M8's hard constraint: `sol run` with no debug/profile flags must
/// not carry any of the new instrumentation's overhead - `Runtime<()>`'s
/// hook calls are the zero-cost `()` no-op (see `interp::Hooks`), a
/// genuinely different compiled path from `Runtime<TimingHooks>`, not a
/// runtime-disabled branch. This is asserted at the type level already
/// (main.rs's `run` only ever constructs `Engine<()>` unless
/// `--profile-time` is passed); this test is the behavioral half - a
/// plain run must still work identically once `interp::Hooks`/`H` exists.
#[test]
fn plain_run_is_unaffected_by_the_new_hooks_machinery() {
    let (stdout, stderr, ok) = run_fixture("debug_calls.sol");
    assert!(ok, "stderr: {stderr}");
    assert_eq!(stdout, "13");
}

/// M8: `sol debug` pauses at the first call, supports `break <fn>` +
/// `continue` (which then re-pauses only at that breakpoint), `step`
/// (pauses at every subsequent call boundary), `backtrace`, and produces
/// the correct final answer regardless of how much stepping happened.
#[test]
fn debug_repl_breakpoints_step_and_backtrace_work() {
    let path = format!(
        "{}/tests/fixtures/debug_calls.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["debug", &path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"break add\nc\nbt\nc\nc\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-> main([])"), "stdout was:\n{stdout}");
    assert!(
        stdout.contains("debug_calls.sol:6"),
        "stdout was:\n{stdout}"
    );
    assert!(stdout.contains("-> add([1, 2])"), "stdout was:\n{stdout}");
    assert!(stdout.contains("-> add([3, 10])"), "stdout was:\n{stdout}");
    assert!(
        stdout.contains("  add at ")
            && stdout.contains("debug_calls.sol:2\n  main at ")
            && stdout.contains("debug_calls.sol:6"),
        "expected a backtrace showing add called from main:\n{stdout}"
    );
    assert!(
        stdout.trim_end().ends_with("13"),
        "expected the final answer printed after quitting the REPL:\n{stdout}"
    );
}

/// `quit` must exit immediately without running the rest of the program.
#[test]
fn debug_repl_quit_stops_immediately() {
    let path = format!(
        "{}/tests/fixtures/debug_calls.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["debug", &path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"quit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("13"),
        "quit must stop before main finishes"
    );
}

/// U11 item 4: `debug.rs`'s call-boundary hook (`Hooks::on_call_enter`) must
/// keep firing correctly - right breakpoint, right backtrace, right final
/// answer - even once item 2's whole-program proof has elided `triple`'s
/// per-call runtime tag guard and the JIT has promoted it to native code
/// mid-session. `Runtime::call` wraps the hook around the call boundary
/// itself regardless of tier (see debug.rs's module comment), so this should
/// hold; this test pins that it actually does, not just that it should.
#[test]
fn debug_repl_keeps_working_after_a_speculative_candidate_is_proven_exhaustive_and_promoted() {
    let path = format!(
        "{}/tests/fixtures/speculative_exhaustive_any_param.sol",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["debug", &path])
        .env("SOL_SPECULATIVE_THRESHOLD", "1")
        .env("SOL_PROMOTE_THRESHOLD", "100000")
        .env("SOL_OSR_THRESHOLD", "100000")
        .env("SOL_JIT_LOG", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"break triple\nc\nbt\nc\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'triple' speculative candidate proven exhaustive"),
        "expected the whole-program proof to fire before any calls happen:\n{stderr}"
    );
    assert!(
        stderr.contains("promoting 'triple'"),
        "expected triple to actually be promoted to native mid-session:\n{stderr}"
    );
    assert!(stdout.contains("-> main([])"), "stdout was:\n{stdout}");
    assert!(
        stdout.contains("speculative_exhaustive_any_param.sol:13"),
        "stdout was:\n{stdout}"
    );
    assert!(stdout.contains("breakpoint set: triple"), "stdout was:\n{stdout}");
    assert!(
        stdout.contains("-> triple(") && stdout.contains("speculative_exhaustive_any_param.sol:8"),
        "expected the breakpoint on triple to hit (both pre- and post-promotion):\n{stdout}"
    );
    assert!(
        stdout.contains("  triple at ")
            && stdout.contains("speculative_exhaustive_any_param.sol:8\n  main at ")
            && stdout.contains("speculative_exhaustive_any_param.sol:13"),
        "expected a backtrace showing triple called from main:\n{stdout}"
    );
    assert!(
        stdout.trim_end().ends_with("5310"),
        "expected the correct final answer despite the elided guard and native promotion:\n{stdout}"
    );
}
