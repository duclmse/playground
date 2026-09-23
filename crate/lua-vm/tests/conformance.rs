//! Runs conformance.md's fixture corpus against the real piccolo-backed
//! `lua_vm::run_named`. Ground truth for `.expected` files was captured from
//! the system `lua` interpreter (5.5.1) invoked as `lua <fixture>.lua`, so
//! each fixture is run here with its filename as the chunk name to match
//! error-message position strings exactly. Reports per-fixture pass/fail,
//! not just a suite-level result, per conformance.md's harness design.
//!
//! Fixture code and expected output live in sibling directories
//! (`conformance/fixtures/*.lua`, `conformance/expected/*.expected`), paired
//! by matching basename - not co-located by extension - so a directory
//! listing of one never mixes inputs with outputs.

use std::fs;
use std::path::Path;

#[test]
fn fixtures_match_reference_lua_output() {
    let conformance_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance");
    let fixtures_dir = conformance_dir.join("fixtures");
    let expected_dir = conformance_dir.join("expected");

    let mut lua_files: Vec<_> = fs::read_dir(&fixtures_dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", fixtures_dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
        .collect();
    lua_files.sort();

    assert!(
        !lua_files.is_empty(),
        "no .lua fixtures found in {}",
        fixtures_dir.display()
    );

    let mut failures = Vec::new();

    for lua_path in &lua_files {
        let chunk_name = lua_path.file_name().unwrap().to_str().unwrap();
        let expected_path = expected_dir
            .join(lua_path.file_stem().unwrap())
            .with_extension("expected");

        let source = fs::read_to_string(lua_path).unwrap();
        let expected = fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!(
                "fixture {chunk_name} has no matching .expected file at {}: {e}",
                expected_path.display()
            )
        });

        let (output, error) = lua_vm::run_named(&source, chunk_name);

        if let Some(err) = error {
            failures.push(format!("{chunk_name}: unexpected error: {err}"));
        } else if output != expected {
            failures.push(format!(
                "{chunk_name}: output mismatch\n--- expected ---\n{expected}--- actual ---\n{output}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed:\n\n{}",
        failures.len(),
        lua_files.len(),
        failures.join("\n\n")
    );
}
