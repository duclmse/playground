// U12 item 3, deliverable 6: differential tests for the native Tier-0
// debugger engine spike (`crate/sol/src/debugger.rs`). See
// `docs/features/milestones/u12-wasm-playground.md`'s Work item 3 section
// for the full design and its explicitly-scoped-out parts (most importantly:
// this engine answers breakpoint/step queries by indexing a full recorded
// instruction trace from one completed run, not by truly pausing/resuming a
// live interpreter - see `debugger.rs`'s module doc comment).
//
// U12 item 4 adds the tests below the three original ones: `debugEvaluate`
// frame-scoped expression evaluation, `Map`-valued local expansion, and
// memory-stats/force-GC. See `docs/features/milestones/u12-wasm-playground.md`'s
// Work item 4 section and `debugger.rs`'s own "deliverable 1"/`memory_stats`/
// `force_gc` doc comments for the full design and scope cuts.

use sol::debugger::DebugSession;

fn compile(source: &str) -> sol::types::TProgram {
    let (program, _return_type) = sol::compile(source).expect("fixture compiles");
    program
}

/// Fixture 1: a simple multi-statement function - distinct lines, no
/// control flow.
const MULTI_STATEMENT: &str = "function main(): i64
    local a: i64 = 1
    local b: i64 = 2
    local c: i64 = a + b
    return c
end";

/// Fixture 2: a local plus a loop.
const LOCAL_AND_LOOP: &str = "function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 5 do
        total = total + i
        i = i + 1
    end
    return total
end";

/// Fixture 3: a nested call - `main` calls `helper`, which has its own
/// multi-statement body.
const NESTED_CALL: &str = "function helper(x: i64): i64
    local doubled: i64 = x * 2
    local result: i64 = doubled + 1
    return result
end

function main(): i64
    local before: i64 = 10
    local after: i64 = helper(before)
    return after
end";

/// Fixture 4 (U12 item 4): a function with two scalar locals in scope at the
/// same point, for the eval differential tests.
const TWO_LOCALS: &str = "function main(): i64
    local a: i64 = 7
    local b: i64 = 6
    local c: i64 = a * b
    return c
end";

/// Fixture 5 (U12 item 4): a `Map<i64, i64>` local, fully populated in one
/// statement, for the Map-expansion test.
const MAP_LOCAL: &str = "function main(): i64
    local m: Map<i64, i64> = { [1] = 10, [2] = 20 }
    local total: i64 = m[1] + m[2]
    return total
end";

/// Fixture 6 (U12 item 4): allocation-heavy (many `Map` insertions in a
/// loop), for the memory-stats test.
const ALLOCATION_HEAVY: &str = "function main(): i64
    local m: Map<i64, i64> = {}
    local i: i64 = 0
    while i < 2000 do
        m[i] = i * 2
        i = i + 1
    end
    return m[1999]
end";

#[test]
fn breakpoint_hit_locals_match_the_non_paused_interpreted_run() {
    let program = compile(MULTI_STATEMENT);
    // Independent, non-paused run: what the interpreter actually computes.
    let plain_engine: sol::tier0::Engine = sol::tier0::Engine::new(program.clone(), ())
        .expect("plain engine builds");
    let expected_c = match plain_engine.call_outcome("main", &[]) {
        sol_core::CallOutcome::Returned(values) => values[0] as i64,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(expected_c, 3, "sanity: a + b with a=1, b=2");

    // Paused run: breakpoint at line 4 (`local c: i64 = a + b`), just before
    // `c` is assigned - `a` and `b` must already be visible with their final
    // values, `c` must still read as its as-yet-unassigned zero value.
    let session = DebugSession::new(program).expect("session builds");
    let breakpoint = session.set_breakpoint("main", 4);
    assert!(breakpoint.verified, "line 4 must verify: {breakpoint:?}");

    session.run("main", &[]);
    let hit = session
        .first_breakpoint_hit()
        .expect("the breakpoint must be hit during this run");
    let locals = session.locals_at(hit).expect("locals available at the hit");

    let render = |name_index: usize| -> String {
        match &locals[name_index].value {
            sol::debugger::DisplayValue::Scalar(s) => s.clone(),
            other => panic!("expected a scalar local, got {other:?}"),
        }
    };
    assert_eq!(render(0), "1", "local a must already be assigned: {locals:?}");
    assert_eq!(render(1), "2", "local b must already be assigned: {locals:?}");

    // Now let the run finish and confirm the breakpoint's own snapshot was
    // consistent with the final, non-paused result by continuing to the end
    // of the trace and checking the last instruction's register for `c`.
    let trace = session.trace();
    let last = trace.last().expect("trace is non-empty");
    let last_locals = session.locals_at(trace.len() - 1).unwrap();
    assert_eq!(
        last_locals[2].value,
        sol::debugger::DisplayValue::Scalar(expected_c.to_string()),
        "the final recorded state must match the plain run's result; last step: {last:?}"
    );
}

#[test]
fn stepping_over_a_loop_body_produces_the_expected_line_sequence() {
    let program = compile(LOCAL_AND_LOOP);
    let session = DebugSession::new(program).expect("session builds");
    session.run("main", &[]);
    let trace = session.trace();
    assert!(!trace.is_empty());

    // Step-into from the very first instruction must walk every line in
    // source order: line 2 (`local total`) and line 3 (`local i`) each once,
    // then line 4 (the `while` condition check) once per iteration plus one
    // final falsifying check, then lines 5/6 (the loop body) once per
    // iteration, then line 8 (`return total` - line 7 is the loop's own
    // `end`, which maps to no instruction) once. Collect the full line
    // sequence via repeated step_into and check these counts, which also
    // proves the loop actually iterated 5 times in the trace.
    let mut index = 0usize;
    let mut lines = vec![trace[0].line];
    while let Some(next) = session.step_into(index) {
        lines.push(trace[next].line);
        index = next;
    }
    let count = |line: u32| lines.iter().filter(|&&l| l == line).count();
    assert_eq!(count(2), 1, "line 2 (`local total`) runs once: {lines:?}");
    assert_eq!(count(3), 1, "line 3 (`local i`) runs once: {lines:?}");
    assert_eq!(count(4), 6, "line 4 (while condition) runs once per iteration plus the final false check: {lines:?}");
    assert_eq!(count(5), 5, "line 5 (`total = total + i`) runs once per iteration: {lines:?}");
    assert_eq!(count(6), 5, "line 6 (`i = i + 1`) runs once per iteration: {lines:?}");
    assert_eq!(count(8), 1, "line 8 (`return total`) runs once: {lines:?}");
}

#[test]
fn stepping_over_into_and_out_of_a_nested_call_behave_differently() {
    let program = compile(NESTED_CALL);
    let session = DebugSession::new(program).expect("session builds");
    session.run("main", &[]);
    let trace = session.trace();

    // Find the trace index of `main`'s `local after: i64 = helper(before)`
    // (line 9: line 6 is blank between the two functions, line 7 is `main`'s
    // own signature, line 8 is `local before`) - the call site.
    let main_id = session.function_id("main").expect("main has a func_id");
    let call_site = trace
        .iter()
        .position(|s| s.func_id == main_id && s.line == 9)
        .expect("main's call-site line must appear in the trace");

    // step_over from the call site must land back in `main` (same depth),
    // never inside `helper`.
    let over = session
        .step_over(call_site)
        .expect("step_over must land somewhere");
    assert_eq!(
        trace[over].func_id, main_id,
        "step_over must skip entirely over helper's body: landed in func_id {}",
        trace[over].func_id
    );

    // step_into from the call site must land inside `helper` (a different
    // func_id, deeper depth) - the first line of its body.
    let into = session
        .step_into(call_site)
        .expect("step_into must land somewhere");
    assert_ne!(
        trace[into].func_id, main_id,
        "step_into must enter helper's body, not skip it"
    );
    assert!(
        trace[into].depth > trace[call_site].depth,
        "stepping into a call must increase depth"
    );

    // step_out from inside helper must return to main at the original depth.
    let out = session
        .step_out(into)
        .expect("step_out must land somewhere");
    assert_eq!(
        trace[out].func_id, main_id,
        "step_out of helper must return to main"
    );
    assert_eq!(
        trace[out].depth, trace[call_site].depth,
        "step_out must restore main's original call depth"
    );
}

// -----------------------------------------------------------------------
// U12 item 4, deliverable 1: `evaluate` differential tests.
// -----------------------------------------------------------------------

#[test]
fn eval_of_a_local_read_matches_locals_at_reported_value() {
    let program = compile(TWO_LOCALS);
    let session = DebugSession::new(program).expect("session builds");
    // Breakpoint at line 4 (`local c: i64 = a * b`): `a` and `b` are already
    // assigned, `c` is not yet.
    let breakpoint = session.set_breakpoint("main", 4);
    assert!(breakpoint.verified, "line 4 must verify: {breakpoint:?}");
    session.run("main", &[]);
    let hit = session
        .first_breakpoint_hit()
        .expect("the breakpoint must be hit during this run");

    let locals = session.locals_at(hit).expect("locals available at the hit");
    let expected_a = match &locals[0].value {
        sol::debugger::DisplayValue::Scalar(s) => s.clone(),
        other => panic!("expected a scalar local for `a`, got {other:?}"),
    };
    assert_eq!(expected_a, "7", "sanity: local a is 7 at this point: {locals:?}");

    let evaluated = session
        .evaluate(hit, "local0")
        .expect("eval of a plain local read must succeed");
    assert_eq!(
        evaluated,
        sol::debugger::DisplayValue::Scalar(expected_a),
        "eval of `local0` must match locals_at's own reported value for local id 0 (`a`)"
    );
}

#[test]
fn eval_of_arithmetic_over_two_locals_matches_the_independently_computed_result() {
    let program = compile(TWO_LOCALS);
    let session = DebugSession::new(program).expect("session builds");
    let breakpoint = session.set_breakpoint("main", 4);
    assert!(breakpoint.verified, "line 4 must verify: {breakpoint:?}");
    session.run("main", &[]);
    let hit = session
        .first_breakpoint_hit()
        .expect("the breakpoint must be hit during this run");

    // Independently computable: local0 (a) = 7, local1 (b) = 6.
    let evaluated = session
        .evaluate(hit, "local0 * local1 + 1")
        .expect("eval of arithmetic over two locals must succeed");
    assert_eq!(
        evaluated,
        sol::debugger::DisplayValue::Scalar((7 * 6 + 1).to_string()),
        "eval must independently recompute the same arithmetic the source expresses"
    );
}

#[test]
fn eval_rejects_a_reference_to_an_undefined_or_out_of_scope_name() {
    let program = compile(TWO_LOCALS);
    let session = DebugSession::new(program).expect("session builds");
    session.run("main", &[]);
    let trace = session.trace();
    assert!(!trace.is_empty());

    let error = session
        .evaluate(0, "not_a_real_local")
        .expect_err("an undefined name must fail to typecheck, not silently resolve");
    assert!(
        error.contains("eval:"),
        "error should be tagged as an eval failure: {error}"
    );
}

// -----------------------------------------------------------------------
// U12 item 4, deliverable 2: `Map` local expansion.
// -----------------------------------------------------------------------

#[test]
fn map_valued_local_expands_to_its_occupied_entries() {
    let program = compile(MAP_LOCAL);
    let session = DebugSession::new(program).expect("session builds");
    // Breakpoint at line 3 (`local total: i64 = m[1] + m[2]`): `m` is fully
    // populated by line 2's single map-literal statement by this point.
    let breakpoint = session.set_breakpoint("main", 3);
    assert!(breakpoint.verified, "line 3 must verify: {breakpoint:?}");
    session.run("main", &[]);
    let hit = session
        .first_breakpoint_hit()
        .expect("the breakpoint must be hit during this run");
    let locals = session.locals_at(hit).expect("locals available at the hit");

    let map_local = &locals[0];
    let (reference, summary) = match &map_local.value {
        sol::debugger::DisplayValue::Reference { reference, summary } => {
            (*reference, summary.clone())
        }
        other => panic!("expected local 0 (`m`) to render as a Map reference, got {other:?}"),
    };
    assert_eq!(summary, "Map<i64, i64>", "local 0 must be the Map<i64, i64> local `m`");

    let mut entries = session
        .expand(&map_local.ty, reference)
        .expect("a populated Map reference must expand");
    entries.sort_by(|(a, _, _), (b, _, _)| a.cmp(b));
    let rendered: Vec<(String, i64)> = entries
        .into_iter()
        .map(|(key, ty, raw)| {
            assert_eq!(ty, sol::types::Type::I64, "Map<i64, i64>'s value type is i64");
            (key, raw as i64)
        })
        .collect();
    assert_eq!(
        rendered,
        vec![("1".to_string(), 10), ("2".to_string(), 20)],
        "expand must report exactly the two occupied entries the literal inserted"
    );
}

// -----------------------------------------------------------------------
// U12 item 4, deliverable 3: memory stats + force-GC.
// -----------------------------------------------------------------------

#[test]
fn memory_stats_report_plausible_values_and_force_gc_does_not_crash() {
    let program = compile(ALLOCATION_HEAVY);
    let session = DebugSession::new(program).expect("session builds");

    let before = session.memory_stats();
    session.run("main", &[]);
    let after = session.memory_stats();

    // Non-panicking, plausible values: the allocation-heavy fixture (a
    // growing Map<i64, i64> plus its internal resizes) must have allocated
    // something, and byte/block counts must stay mutually consistent (any
    // allocation implies at least one live byte).
    assert!(
        after.live_bytes >= before.live_bytes,
        "running an allocation-heavy fixture must not decrease live_bytes: before={before:?} after={after:?}"
    );
    assert!(
        after.live_blocks >= before.live_blocks,
        "running an allocation-heavy fixture must not decrease live_blocks: before={before:?} after={after:?}"
    );
    assert!(
        after.live_bytes > 0 && after.live_blocks > 0,
        "the fixture's own Map allocations must be reflected: after={after:?}"
    );

    // `force_gc` must be invocable through `DebugSession` without crashing.
    // Per `DebugSession::force_gc`'s doc comment (verified against gc.rs
    // directly): it is a documented no-op here, since `STACK_BASE` is never
    // initialized outside the native JIT/AOT entry path, so it must not
    // reclaim anything either - assert the stats are unchanged, not just
    // that the call returned.
    session.force_gc();
    let after_gc = session.memory_stats();
    assert_eq!(
        after_gc, after,
        "force_gc must be a safe no-op in a jit-free DebugSession context, not reclaim anything"
    );
}
