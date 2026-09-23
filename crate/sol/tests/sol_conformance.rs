// Typed Sol capability regression suite (legacy sol-conformance paths): runs every fixture under
// tests/fixtures/sol-conformance/ (one hand-written typed `.sol` counterpart
// per top-level file in the pinned lua-5.5.1-tests corpus) and checks its
// `sol run` stdout against the expected value recorded in
// tests/sol-conformance/manifest.toml (repo root). See that manifest's
// header and tests/sol-conformance/README.md for what "ported" vs
// "not-applicable" means here, and scripts/test-sol-conformance-suite.sh for the
// shell-level runner this test mirrors. These are not Lua compatibility passes.

use std::process::Command;
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
    let path = format!(
        "{}/tests/fixtures/sol-conformance/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
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

// One (fixture, expected stdout) pair per case in
// tests/sol-conformance/manifest.toml (repo root) - kept in the same order
// as that manifest so the two stay easy to diff against each other by eye.
// "Not applicable" stubs are included too: they must still compile, run,
// and return their documented sentinel (0), the same as any other fixture.
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

#[test]
fn every_typed_capability_fixture_matches_its_manifest_expectation() {
    ensure_staticlib();
    let mut failures = Vec::new();
    for (fixture, expected) in CASES {
        let (stdout, stderr, ok) = run_fixture(fixture);
        if !ok {
            failures.push(format!(
                "{fixture}: did not run successfully (stderr: {stderr})"
            ));
        } else if stdout != *expected {
            failures.push(format!(
                "{fixture}: expected stdout {expected:?}, got {stdout:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn typed_capability_manifest_lists_every_upstream_lua_file_exactly_once() {
    let manifest_path = format!(
        "{}/../../tests/sol-conformance/manifest.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("failed to read {manifest_path}: {e}"));
    let manifest_case_count = manifest
        .lines()
        .filter(|line| line.trim() == "[[case]]")
        .count();
    assert_eq!(
        manifest_case_count,
        CASES.len(),
        "tests/sol-conformance/manifest.toml has {manifest_case_count} cases but this test tracks {}",
        CASES.len()
    );

    let upstream_dir = format!("{}/../../lua-5.5.1-tests", env!("CARGO_MANIFEST_DIR"));
    let Ok(entries) = std::fs::read_dir(&upstream_dir) else {
        return; // upstream corpus not checked out in this environment; skip the cross-check
    };
    let mut upstream_files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".lua"))
        .collect();
    upstream_files.sort();
    assert_eq!(
        upstream_files.len(),
        CASES.len(),
        "lua-5.5.1-tests has {} top-level .lua files but this suite tracks {}",
        upstream_files.len(),
        CASES.len()
    );
    for upstream in &upstream_files {
        let stem = upstream.strip_suffix(".lua").unwrap();
        assert!(
            manifest.contains(&format!("sol = \"{stem}.sol\"")),
            "no sol-conformance case maps to {upstream}"
        );
    }
}
