use std::path::Path;
use std::process::Command;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/inference");

#[test]
fn u4_inference_fixtures_elide_the_expected_dynamic_checks_without_cli_return_echoes() {
    let cases = [
        ("literal.lua", "operator Add"),
        ("dominated_type_test.lua", "dominated type test"),
        ("loop_index.lua", "numeric-for index"),
        ("local_function.lua", "local function signature"),
        ("table_shape.lua", "field answer"),
    ];

    for (fixture, proof) in cases {
        let path = Path::new(FIXTURES).join(fixture);
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", "--type-policy", "infer", "--explain-types"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "{fixture}: unexpected CLI stdout");
        let report = String::from_utf8_lossy(&output.stderr);
        assert!(
            report.contains(proof),
            "{fixture}: missing {proof:?}\n{report}"
        );
        if fixture == "local_function.lua" {
            assert!(report.contains("elided call add as integer"), "{report}");
        }
        assert!(report.contains("checks-elided="), "{fixture}: {report}");
    }
}

#[test]
fn every_u4_fixture_is_semantically_identical_with_inference_off() {
    for fixture in [
        "literal.lua",
        "dominated_type_test.lua",
        "loop_index.lua",
        "local_function.lua",
        "table_shape.lua",
    ] {
        let path = Path::new(FIXTURES).join(fixture);
        let run = |policy: &str| {
            Command::new(env!("CARGO_BIN_EXE_sol"))
                .args(["run", "--type-policy", policy])
                .arg(&path)
                .output()
                .unwrap()
        };
        let off = run("off");
        let infer = run("infer");
        assert!(
            off.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&off.stderr)
        );
        assert!(
            infer.status.success(),
            "{fixture}: {}",
            String::from_utf8_lossy(&infer.stderr)
        );
        assert_eq!(off.stdout, infer.stdout, "{fixture}");
    }
}
