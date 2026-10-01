//! Regression coverage for the U9 dynamic baseline JIT
//! (`sol::lua_runtime::dynjit`, not part of this crate's public API, so
//! exercised here through the `sol` CLI binary rather than in-process -
//! mirrors `programs.rs`'s own `dump_ir_dump_asm_and_jit_log_cli_flags_all_produce_output`
//! test, which does the same for the typed tier's `SOL_PROMOTE_THRESHOLD`/
//! `--jit-log`).

use std::process::Command;

/// Item 2 (safepoint helper + hot-counter wiring): forcing
/// `SOL_LUA_PROMOTE_THRESHOLD=1` must drive `LuaRuntime::try_promote` on a
/// `Proto`'s very first activation (observable via `SOL_LUA_JIT_LOG=1`'s
/// trace), with **no behavior change** yet - `DynJit::promote` always fails
/// until item 3 lands real lowering, so the function must still run through
/// the interpreter and produce the same result as an unpromoted run.
#[test]
fn forcing_the_promotion_threshold_to_one_triggers_a_promotion_attempt_with_no_behavior_change() {
    let path = format!(
        "{}/tests/fixtures/dynjit_promote.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    // "42" from the script's own `print`, "nil" from `sol run`'s own
    // trailing print of the chunk's (absent) return value.
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "42\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("[dynjit] promotion requested for 'add_one'"),
        "expected a dynjit promotion-attempt trace for 'add_one':\n{stderr}"
    );

    // Same source, no forced threshold (the default is far higher than this
    // script's handful of calls, so no promotion attempt happens at all) -
    // must produce byte-identical stdout.
    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 3 (leaf-instruction lowering): a call-free, capture-free numeric
/// loop (`ForPrep`/`ForLoop`/`Binary`/`Return`, no table/global/upvalue
/// access) is eligible for real promotion - forcing
/// `SOL_LUA_PROMOTE_THRESHOLD=1` must actually compile it to native code
/// (not just attempt-and-reject, as `dynjit_promote.lua`'s `Binary`-only
/// body still does in the previous test above) and produce byte-identical
/// output to the fully interpreted run.
#[test]
fn a_call_free_numeric_loop_is_promoted_to_native_code_with_no_behavior_change() {
    let path = format!(
        "{}/tests/fixtures/dynjit_loop_sum.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "5050\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'loop_sum' promoted to native code"),
        "expected 'loop_sum' to actually compile under item 3's eligible \
         instruction set, not just attempt-and-reject:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 4 (field/global access + allocation): a function that allocates a
/// table (`NewTable`), writes/reads it through both the constant-key IC path
/// (`GetField`/`SetField`) and the runtime-key path (`GetIndex`/`SetIndex`),
/// and reads/writes a global (`GetGlobal`/`SetGlobal`) is now eligible for
/// promotion - forcing `SOL_LUA_PROMOTE_THRESHOLD=1` must actually compile it
/// (not just attempt-and-reject, as item 3's `dynjit_promote.lua` still
/// does) and produce byte-identical output to the fully interpreted run,
/// across two calls (the first call triggers promotion and already runs
/// natively; the second call exercises an already-`Native` `Proto`).
#[test]
fn table_field_global_and_index_access_is_promoted_to_native_code_with_no_behavior_change() {
    let path = format!(
        "{}/tests/fixtures/dynjit_table_global.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "61\n62\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'touch' promoted to native code"),
        "expected 'touch' to actually compile under item 4's extended \
         eligible instruction set, not just attempt-and-reject:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Same fixture as above, but with `SOL_LUA_GC_STRESS=1`: every allocation
/// (`NewTable` here) runs a full collection pass first - this exercises the
/// `dynjit_new_table` stub's dual-write GC-safety contract (`stubs.rs`'s own
/// doc comment on that stub) under the highest allocation-triggered
/// collection pressure this suite can produce, not just the register-sync
/// contract at backward-branch safepoints that item 3's own GC-stress
/// coverage already exercises.
#[test]
fn table_field_global_and_index_access_survives_gc_stress_mode() {
    let path = format!(
        "{}/tests/fixtures/dynjit_table_global.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let stressed = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .env("SOL_LUA_GC_STRESS", "1")
        .output()
        .unwrap();
    assert!(stressed.status.success());
    assert_eq!(String::from_utf8_lossy(&stressed.stdout).trim(), "61\n62\nnil");
    let stderr = String::from_utf8_lossy(&stressed.stderr);
    assert!(
        stderr.contains("'touch' promoted to native code"),
        "expected 'touch' to actually compile even under gc_stress:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_GC_STRESS", "1")
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, stressed.stdout);
}

/// Item 5 (call/return protocol): a function containing ordinary nested
/// calls (`Instr::Call`) to other Lua functions is now eligible for
/// promotion - the call instruction itself always deopts to the interpreter
/// (`lower.rs`'s own doc on this unconditional-deopt design), but everything
/// around it (the surrounding arithmetic, the `Return`) still runs natively,
/// and the whole function must still produce byte-identical output.
#[test]
fn a_function_with_nested_calls_is_promoted_to_native_code_with_no_behavior_change() {
    let path = format!(
        "{}/tests/fixtures/dynjit_nested_call.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "21\n23\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'compute' promoted to native code"),
        "expected 'compute' to promote despite its nested Call instructions:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 5: `pcall` wrapping a call made from a promoted function must behave
/// identically whether the wrapped call succeeds or raises - the `Call` to
/// `pcall` itself deopts (per the unconditional-deopt design), and the
/// interpreter's own existing `pcall`/error-unwind machinery handles the
/// rest untouched.
#[test]
fn pcall_around_a_call_from_a_promoted_function_behaves_identically() {
    let path = format!(
        "{}/tests/fixtures/dynjit_pcall_call.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "10\n-1\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'safe_call' promoted to native code"),
        "expected 'safe_call' (the pcall wrapper) to promote:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 5: a coroutine resumed (and yielding) from inside a call made by a
/// promoted function - `driver`'s own `Call` to `coroutine.resume` deopts,
/// and the interpreter's existing coroutine-yield machinery (untouched by
/// this increment) drives `yielder` across three resumes exactly as it would
/// with no promotion at all.
#[test]
fn coroutine_yield_from_a_call_inside_a_promoted_function_behaves_identically() {
    let path = format!(
        "{}/tests/fixtures/dynjit_coroutine_yield.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "6\n6\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'driver' promoted to native code"),
        "expected 'driver' to promote:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 5: a generic `for` loop (`ipairs`) now promotes - `TForCall` always
/// deopts (it invokes the iterator function), but `TForLoop`'s back-edge
/// check runs natively, so the loop body (`total = total + v`) stays
/// compiled across iterations rather than falling back to the interpreter
/// every time.
#[test]
fn a_generic_for_loop_is_promoted_to_native_code_with_no_behavior_change() {
    let path = format!(
        "{}/tests/fixtures/dynjit_generic_for.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "100\n100\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'sum_table' promoted to native code"),
        "expected 'sum_table' to promote with its TForCall/TForLoop generic for:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}

/// Item 5: a to-be-closed (`<close>`) variable inside a promoted function -
/// `MarkClose` runs as an ordinary native leaf stub (it never invokes `Lua`
/// itself), while `CloseSlots` (which does invoke the `__close` metamethods)
/// always deopts, letting the interpreter's existing blocking `__close` call
/// path run unchanged.
#[test]
fn a_to_be_closed_variable_in_a_promoted_function_behaves_identically() {
    let path = format!(
        "{}/tests/fixtures/dynjit_close_slots.lua",
        env!("CARGO_MANIFEST_DIR")
    );

    let promoted = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .env("SOL_LUA_PROMOTE_THRESHOLD", "1")
        .env("SOL_LUA_JIT_LOG", "1")
        .output()
        .unwrap();
    assert!(promoted.status.success());
    assert_eq!(String::from_utf8_lossy(&promoted.stdout).trim(), "2\n4\nnil");
    let stderr = String::from_utf8_lossy(&promoted.stderr);
    assert!(
        stderr.contains("'use_closers' promoted to native code"),
        "expected 'use_closers' to promote with its MarkClose/CloseSlots <close> variables:\n{stderr}"
    );

    let baseline = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", &path])
        .output()
        .unwrap();
    assert!(baseline.status.success());
    assert_eq!(baseline.stdout, promoted.stdout);
}
