// U12 item 2: proves the jit-free `tier0::Engine` (no promotion, pure
// bytecode interpretation - what a wasm32/`--no-default-features` build
// actually runs) produces the exact same output as the tiered-JIT/AOT path
// for every fixture in the existing `tests/fixtures/sol-conformance/` corpus
// (see `tests/sol_conformance.rs`, which runs the same fixtures through the
// `sol` CLI binary instead). This is the "safe replacement candidate"
// differential evidence the U12 plan asked for - there is no separate
// `conformance/fixtures`/`conformance/expected` directory on disk (that
// referenced piccolo's retired browser corpus and never existed for this
// crate); this corpus is the real, already-existing asset that serves the
// same purpose for the typed tier.
//
// CASES is intentionally a duplicate of `tests/sol_conformance.rs`'s own
// list, not a shared import (these are two independent integration test
// binaries) - keep the two in sync when either changes.

const CASES: &[(&str, &str)] = &[
    ("all.sol", "0"),
    ("api.sol", "0"),
    ("attrib.sol", "5050"),
    ("big.sol", "599950010"),
    ("bitwise.sol", "533"),
    ("bwcoercion.sol", "1"),
    ("calls.sol", "125250"),
    ("closure.sol", "44"),
    ("code.sol", "0"),
    ("constructs.sol", "188"),
    ("coroutine.sol", "0"),
    ("cstack.sol", "0"),
    ("db.sol", "0"),
    ("errors.sol", "54"),
    ("events.sol", "26"),
    ("files.sol", "0"),
    ("gc.sol", "7"),
    ("gengc.sol", "100"),
    ("goto.sol", "10"),
    ("heavy.sol", "899997"),
    ("literals.sol", "3"),
    ("locals.sol", "9"),
    ("main.sol", "0"),
    ("math.sol", "81"),
    ("memerr.sol", "0"),
    ("nextvar.sol", "332834600"),
    ("pm.sol", "0"),
    ("sort.sol", "119"),
    ("strings.sol", "8"),
    ("tpack.sol", "0"),
    ("tracegc.sol", "0"),
    ("utf8.sol", "3"),
    ("vararg.sol", "104"),
    ("verybig.sol", "999899867"),
];

fn run_on_tier0(name: &str) -> Result<String, String> {
    let path = format!(
        "{}/tests/fixtures/sol-conformance/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let source = std::fs::read(&path).map_err(|e| format!("reading {path}: {e}"))?;
    let (program, return_type) =
        sol::compile_bytes(&source, sol::parser::SourceMode::Sol).map_err(|e| e.to_string())?;
    let engine: sol::tier0::Engine =
        sol::tier0::Engine::new_with_budget(program, (), u64::MAX)?;
    match engine.call_outcome("main", &[]) {
        sol_core::CallOutcome::Returned(values) => {
            let result = values.first().copied().unwrap_or(0);
            Ok(match return_type {
                sol::types::Type::I64 => (result as i64).to_string(),
                sol::types::Type::F64 => f64::from_bits(result).to_string(),
                sol::types::Type::Bool => (result != 0).to_string(),
                other => format!("<unsupported return type for this differential test: {other:?}>"),
            })
        }
        sol_core::CallOutcome::Raised(error) => Err(error),
        other => Err(format!("unexpected call outcome: {other:?}")),
    }
}

/// `math.sol` declares `extern function sqrt`/`pow` linking real native libm
/// symbols (arbitrary user FFI, not a compiler-injected runtime helper) -
/// see `tier0.rs::known_runtime_extern_shim`'s doc comment for why this is a
/// genuine architectural limit of Tier-0-only execution, not a gap this
/// milestone item closes. Every other fixture in the corpus is expected to
/// match exactly.
const KNOWN_UNSUPPORTED: &[&str] = &["math.sol"];

#[test]
fn tier0_bytecode_interpretation_matches_the_tiered_jit_path_on_every_fixture() {
    // `calls.sol` recurses only ~1000 levels deep (trivial for native/JIT
    // code, or for the interpreter in an optimized build), but a debug build
    // of `interp.rs`'s recursive, large-match-per-frame `dispatch`/
    // `interpret` pair is heavy enough per Rust stack frame to overflow the
    // platform's default 8MB thread stack at that depth - spawning on a
    // thread with a much larger stack is the standard fix for a debug-build
    // recursive interpreter test, not a sign of a real per-Sol-call-depth
    // regression (`cargo test --release` never needed this). The same
    // per-frame cost is relevant to item 3+'s wasm-bindgen work: the wasm
    // module's own linear-memory stack size will likely need raising past
    // its default too.
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let mut failures = Vec::new();
            for (fixture, expected) in CASES {
                if KNOWN_UNSUPPORTED.contains(fixture) {
                    continue;
                }
                match run_on_tier0(fixture) {
                    Ok(actual) if actual == *expected => {}
                    Ok(actual) => failures.push(format!(
                        "{fixture}: expected {expected:?}, tier0 produced {actual:?}"
                    )),
                    Err(error) => failures.push(format!("{fixture}: tier0 failed: {error}")),
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
        })
        .expect("spawning the larger-stack test thread")
        .join()
        .expect("tier0 differential test thread panicked");
}
