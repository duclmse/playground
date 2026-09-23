use super::*;

#[test]
fn breakpoint_stops_at_the_right_line_and_locals_are_named() {
    let mut session =
        DebugSession::launch("local x = 1\nlocal y = 2\nlocal z = x + y\nprint(z)", "bp");
    session.set_breakpoint("bp", 3);
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(stop.line, Some(3));

    let locals = session.get_locals(0, 0);
    let x = locals.iter().find(|v| v.name == "x").expect("x in scope");
    assert_eq!(x.display, "1");
    let y = locals.iter().find(|v| v.name == "y").expect("y in scope");
    assert_eq!(y.display, "2");
}

#[test]
fn continue_after_breakpoint_runs_to_completion() {
    let mut session = DebugSession::launch("local x = 1\nprint(x)\nprint('done')", "cont");
    session.set_breakpoint("cont", 2);
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    let stop2 = session.continue_();
    assert_eq!(stop2.reason, "terminated");
    assert_eq!(session.take_output(), "1\ndone\n");
}

#[test]
fn continue_burst_reports_budget_exhausted_before_a_real_stop() {
    let mut session = DebugSession::launch(
        "local sum = 0\nfor i = 1, 100 do\n  sum = sum + i\nend\nprint(sum)",
        "burst",
    );
    // No breakpoints: a 3-opcode budget can't possibly reach "terminated"
    // in this program, so this must report "still running", not a stop.
    let burst = session.continue_burst(3);
    assert!(!burst.stopped);
    assert!(burst.stop.is_none());
}

#[test]
fn continue_burst_reports_a_real_stop_once_the_program_finishes() {
    let mut session = DebugSession::launch("print('done')", "burst-finish");
    let burst = session.continue_burst(1_000_000);
    assert!(burst.stopped);
    assert_eq!(burst.stop.unwrap().reason, "terminated");
    assert_eq!(session.take_output(), "done\n");
}

#[test]
fn repeated_continue_bursts_eventually_reach_the_same_breakpoint_as_continue() {
    let mut session = DebugSession::launch("local x = 1\nprint(x)\nprint('done')", "burst-bp");
    session.set_breakpoint("burst-bp", 2);
    let mut burst = session.continue_burst(1);
    let mut guard = 0;
    while !burst.stopped {
        guard += 1;
        assert!(
            guard < 10_000,
            "never reached a stop across many tiny bursts"
        );
        burst = session.continue_burst(1);
    }
    assert_eq!(burst.stop.unwrap().reason, "breakpoint");
    // Resuming past the breakpoint (still via one-instruction bursts)
    // must reach "terminated" without re-triggering the same breakpoint
    // it just resumed from.
    let mut burst = session.continue_burst(1);
    guard = 0;
    while !burst.stopped {
        guard += 1;
        assert!(
            guard < 10_000,
            "never reached a stop across many tiny bursts"
        );
        burst = session.continue_burst(1);
    }
    assert_eq!(burst.stop.unwrap().reason, "terminated");
    assert_eq!(session.take_output(), "1\ndone\n");
}

#[test]
fn step_over_does_not_descend_into_calls() {
    let mut session = DebugSession::launch(
        "local function f() return 1 end\nlocal a = f()\nlocal b = 2",
        "over",
    );
    session.set_breakpoint("over", 2);
    let stop = session.continue_();
    assert_eq!(stop.line, Some(2));
    let stop = session.step_over();
    assert_eq!(stop.reason, "step");
    assert_eq!(stop.line, Some(3));
}

#[test]
fn step_into_descends_into_the_callee() {
    let mut session =
        DebugSession::launch("local function f()\n  return 1\nend\nlocal a = f()", "into");
    session.set_breakpoint("into", 4);
    let stop = session.continue_();
    assert_eq!(stop.line, Some(4));
    let stop = session.step_into();
    assert_eq!(stop.reason, "step");
    assert_eq!(stop.line, Some(2));
    let stack = session.get_stack_trace(0);
    assert!(
        stack.len() >= 2,
        "expected at least callee+main frames, got {stack:?}",
    );
}

#[test]
fn step_out_returns_to_the_caller() {
    let mut session = DebugSession::launch(
        "local function f()\n  return 1\nend\nlocal a = f()\nlocal b = 2",
        "out",
    );
    session.set_breakpoint("out", 2);
    let stop = session.continue_();
    assert_eq!(stop.line, Some(2));
    let stop = session.step_out();
    assert_eq!(stop.reason, "step");
    // Not line 4 ("local a = f()", the call site): piccolo's `Call`
    // opcode places the return value directly into `a`'s register as
    // part of returning, so there is no separate opcode left on line 4
    // once the call completes - the very next position is already line
    // 5. Confirmed by reading the compiled prototype's own
    // opcode/line table (see docs/phase-4-8-implementation.md).
    assert_eq!(stop.line, Some(5));
}

#[test]
fn call_stack_reports_frame_names_and_lines() {
    let mut session = DebugSession::launch(
        // `return inner()` deliberately avoided in `outer` - it's a
        // proper tail call, which piccolo (correctly, matching real
        // Lua) *replaces* the caller's frame rather than pushing a new
        // one, so `outer` wouldn't appear in the stack at all. `local r
        // = inner(); return r` forces a real, non-tail call instead.
        "local function inner()\n  return 1\nend\nlocal function outer()\n  local r = inner()\n  return r\nend\nprint(outer())",
        "stack",
    );
    session.set_breakpoint("stack", 2);
    let stop = session.continue_();
    assert_eq!(stop.line, Some(2));
    let stack = session.get_stack_trace(0);
    assert_eq!(stack.len(), 3, "inner, outer, main: {stack:?}");
    assert_eq!(stack[0].line, Some(2));
    assert_eq!(stack[2].name, "main");
}

#[test]
fn get_upvalues_reports_a_captured_local_by_name() {
    let mut session = DebugSession::launch(
        "local function make_counter()\n  local count = 0\n  local function increment()\n    count = count + 1\n    return count\n  end\n  return increment\nend\nlocal inc = make_counter()\ninc()\n",
        "upvalues",
    );
    session.set_breakpoint("upvalues", 5); // "return count", inside increment()
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    let upvalues = session.get_upvalues(0, 0);
    let count = upvalues
        .iter()
        .find(|v| v.name == "count")
        .unwrap_or_else(|| panic!("upvalue 'count' not found in {upvalues:?}"));
    assert_eq!(count.display, "1");
}

#[test]
fn get_upvalues_is_empty_for_a_function_with_no_captures() {
    // `f` only ever touches its own parameter/locals and globals (via
    // the implicit `_ENV` upvalue every top-level function gets) - no
    // *named* local from an enclosing scope, so this exercises the
    // `_ENV`-only case rather than an empty list outright.
    let mut session = DebugSession::launch(
        "local function f(x)\n  return x + 1\nend\nprint(f(1))",
        "no-upvalues",
    );
    session.set_breakpoint("no-upvalues", 2);
    session.continue_();
    let upvalues = session.get_upvalues(0, 0);
    assert!(
        upvalues.iter().all(|v| v.name == "_ENV"),
        "expected only _ENV (if anything): {upvalues:?}"
    );
}

#[test]
fn globals_are_visible_and_tables_expand_lazily() {
    let mut session = DebugSession::launch("t = { a = 1, b = 2 }\nlocal x = 1", "globals");
    session.set_breakpoint("globals", 2);
    session.continue_();
    let globals = session.get_globals();
    let t = globals.iter().find(|v| v.name == "t").expect("global t");
    assert!(t.expandable);
    let reference = t.reference.expect("table has a reference");
    let entries = session.get_table_entries(reference, 0, 100);
    assert_eq!(entries.len(), 2);
}

#[test]
fn self_referential_table_reuses_the_same_reference() {
    let mut session = DebugSession::launch("t = {}\nt.self = t\nlocal x = 1", "selfref");
    session.set_breakpoint("selfref", 3);
    session.continue_();
    let globals = session.get_globals();
    let t = globals.iter().find(|v| v.name == "t").unwrap();
    let t_ref = t.reference.unwrap();
    let entries = session.get_table_entries(t_ref, 0, 10);
    let self_entry = entries.iter().find(|v| v.name == "self").unwrap();
    assert_eq!(
        self_entry.reference,
        Some(t_ref),
        "t.self must alias t's own reference"
    );
}

#[test]
fn evaluate_sees_the_paused_frames_locals() {
    let mut session = DebugSession::launch("local secret = 42\nprint(secret)", "eval");
    session.set_breakpoint("eval", 2);
    session.continue_();
    let result = session.evaluate(0, "secret + 1", 0);
    assert!(result.ok, "{}", result.display);
    assert_eq!(result.display, "43");
}

#[test]
fn evaluate_falls_back_to_globals() {
    let mut session = DebugSession::launch("g = 10\nlocal x = 1\nprint(x)", "evalglobal");
    session.set_breakpoint("evalglobal", 3);
    session.continue_();
    let result = session.evaluate(0, "g * 2", 0);
    assert!(result.ok, "{}", result.display);
    assert_eq!(result.display, "20");
}

#[test]
fn set_variable_writes_back_into_the_paused_frame() {
    let mut session = DebugSession::launch("local x = 1\nprint(x)", "setvar");
    session.set_breakpoint("setvar", 2);
    session.continue_();
    let result = session.set_variable(0, 0, "x", "99");
    assert!(result.ok, "{}", result.display);
    let locals = session.get_locals(0, 0);
    let x = locals.iter().find(|v| v.name == "x").unwrap();
    assert_eq!(x.display, "99");
}

#[test]
fn conditional_breakpoint_only_stops_when_condition_is_truthy() {
    // Breakpoint deliberately on the `print` line, not the `local
    // dummy = i` line right above it: piccolo reports "current
    // position" as the *next* instruction about to run (see
    // docs/phase-4-8-implementation.md), so at the assignment line
    // itself `dummy`'s live range hasn't started yet and it wouldn't
    // resolve as a named local - a real, if narrow, consequence of that
    // pc convention, not something this test should paper over.
    let mut session = DebugSession::launch(
        "for i = 1, 5 do\n  local dummy = i\n  print(dummy)\nend",
        "condbp",
    );
    let bp = session.set_breakpoint("condbp", 3);
    session.set_breakpoint_condition(bp.id, Some("dummy == 3".to_string()));
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    let locals = session.get_locals(0, 0);
    let dummy = locals.iter().find(|v| v.name == "dummy").unwrap();
    assert_eq!(dummy.display, "3");
}

#[test]
fn hit_condition_only_stops_after_the_nth_hit() {
    let mut session = DebugSession::launch(
        "for i = 1, 5 do\n  local dummy = i\n  print(dummy)\nend",
        "hitcount",
    );
    let bp = session.set_breakpoint("hitcount", 3);
    session.set_breakpoint_hit_condition(bp.id, Some(3));
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    let locals = session.get_locals(0, 0);
    let dummy = locals.iter().find(|v| v.name == "dummy").unwrap();
    assert_eq!(dummy.display, "3");
}

#[test]
fn logpoint_logs_without_stopping() {
    let mut session = DebugSession::launch(
        "for i = 1, 3 do\n  local dummy = i\n  print(dummy)\nend\nprint('done')",
        "logpoint",
    );
    let bp = session.set_breakpoint("logpoint", 3);
    session.set_breakpoint_log_message(bp.id, Some("'i='..dummy".to_string()));
    let stop = session.continue_();
    assert_eq!(stop.reason, "terminated");
    let output = session.take_output();
    assert_eq!(output, "i=1\n1\ni=2\n2\ni=3\n3\ndone\n");
}

#[test]
fn uncaught_error_stops_as_an_exception() {
    let mut session = DebugSession::launch("local x = 1\nerror('boom')", "err");
    let stop = session.continue_();
    assert_eq!(stop.reason, "exception");
    assert!(stop.message.unwrap().contains("boom"));
    assert!(session.is_terminated());
}

#[test]
fn remove_breakpoint_stops_it_from_triggering() {
    let mut session = DebugSession::launch("local x = 1\nprint(x)\nprint('done')", "remove");
    let bp = session.set_breakpoint("remove", 2);
    session.remove_breakpoint(bp.id);
    let stop = session.continue_();
    assert_eq!(stop.reason, "terminated");
}

#[test]
fn breakpoints_are_scoped_to_their_own_file_in_a_multi_file_project() {
    // Regression test: `set_breakpoint` used to take only a line number,
    // so a breakpoint on line 2 would fire the first time *any* file
    // reached line 2, regardless of which file the UI actually meant.
    // Both files below have their `print` call on line 2 - a breakpoint
    // on "lib.lua" line 2 must not stop main.lua's own line 2 first.
    let names = vec!["main.lua".to_string(), "lib.lua".to_string()];
    let contents = vec![
        "local lib = require('lib')\nprint('main line 2')\nlib.go()".to_string(),
        "local M = {}\nfunction M.go()\n  print('lib line 3')\nend\nreturn M".to_string(),
    ];
    let mut session = DebugSession::launch_project(names, contents, "main.lua");
    session.set_breakpoint("lib", 3);
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(stop.line, Some(3));
    let stack = session.get_stack_trace(0);
    assert_eq!(
        stack[0].source, "lib",
        "stopped in the wrong file: {stack:?}"
    );
    assert_eq!(session.take_output(), "main line 2\n");
}

#[test]
fn get_threads_lists_the_main_thread_and_a_resumed_coroutine() {
    let mut session = DebugSession::launch(
        "local co = coroutine.create(function()\n  local x = 42\n  print(x)\nend)\ncoroutine.resume(co)",
        "coro",
    );
    session.set_breakpoint("coro", 3); // print(x), inside the coroutine body
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");

    let threads = session.get_threads();
    assert_eq!(
        threads.len(),
        2,
        "expected main + the resumed coroutine: {threads:?}"
    );
    assert_eq!(threads[0].id, 0);
    assert_eq!(
        threads[0].status, "normal",
        "main is waiting on the coroutine.resume() call"
    );
    assert_eq!(threads[1].id, 1);
    assert_eq!(threads[1].status, "running");
}

#[test]
fn a_coroutines_own_stack_and_locals_are_inspectable_independent_of_the_main_thread() {
    let mut session = DebugSession::launch(
        "local co = coroutine.create(function()\n  local x = 42\n  print(x)\nend)\ncoroutine.resume(co)",
        "coro2",
    );
    session.set_breakpoint("coro2", 3);
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    assert_eq!(stop.line, Some(3));

    // Thread 1 (the coroutine) is paused inside its own anonymous
    // function, about to run `print(x)`, with `x` already assigned.
    let coro_stack = session.get_stack_trace(1);
    assert_eq!(coro_stack[0].line, Some(3));
    let coro_locals = session.get_locals(1, 0);
    let x = coro_locals
        .iter()
        .find(|v| v.name == "x")
        .expect("x in the coroutine's frame");
    assert_eq!(x.display, "42");

    // Thread 0 (main) is paused at its own line - the coroutine.resume
    // call - and has no `x` of its own.
    let main_stack = session.get_stack_trace(0);
    assert_eq!(main_stack[0].line, Some(5));
    let main_locals = session.get_locals(0, 0);
    assert!(
        main_locals.iter().all(|v| v.name != "x"),
        "main's own frame must not see the coroutine's local: {main_locals:?}"
    );

    // evaluate() against the coroutine's frame sees its local; against
    // main's frame it doesn't (falls through to a nonexistent global).
    let eval_in_coro = session.evaluate(1, "x", 0);
    assert!(eval_in_coro.ok, "{}", eval_in_coro.display);
    assert_eq!(eval_in_coro.display, "42");
    let eval_in_main = session.evaluate(0, "x", 0);
    assert!(eval_in_main.ok, "{}", eval_in_main.display);
    assert_eq!(eval_in_main.display, "nil");
}

#[test]
fn set_variable_writes_an_open_upvalue_back_into_the_capturing_frames_stack_slot() {
    // `outer` hasn't returned by the time `inner` is paused, so `count`'s
    // upvalue is still `Open` - it points straight into `outer`'s own
    // (still-live) stack slot. Writing it should be visible to `outer`
    // itself once `inner` returns, not just to a later `get_upvalues` call.
    let mut session = DebugSession::launch(
        "local function outer()\n  local count = 0\n  local function inner()\n    local x = count\n  end\n  inner()\n  print(count)\nend\nouter()\n",
        "openupvalue",
    );
    session.set_breakpoint("openupvalue", 4); // "local x = count", inside inner()
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");

    // Not a named local of inner's own frame (the assignment hasn't run
    // yet) - must resolve via the upvalue fallback.
    let locals = session.get_locals(0, 0);
    assert!(locals.iter().all(|v| v.name != "count"), "{locals:?}");
    let upvalues = session.get_upvalues(0, 0);
    let count = upvalues
        .iter()
        .find(|v| v.name == "count")
        .expect("count captured by inner");
    assert_eq!(count.display, "0");

    let result = session.set_variable(0, 0, "count", "42");
    assert!(result.ok, "{}", result.display);

    let stop2 = session.continue_();
    assert_eq!(stop2.reason, "terminated");
    assert_eq!(session.take_output(), "42\n");
}

#[test]
fn set_variable_writes_a_closed_upvalue() {
    // By the time `inc()` runs, `make_counter` has already returned, so
    // `count`'s upvalue is `Closed` - a different storage representation
    // (the value lives inline in the `UpValueState`, not in any thread's
    // stack) than the open-upvalue case above, exercising the other branch
    // of `set_variable`'s upvalue write path.
    let mut session = DebugSession::launch(
        "local function make_counter()\n  local count = 0\n  local function increment()\n    count = count + 1\n    return count\n  end\n  return increment\nend\nlocal inc = make_counter()\nprint(inc())\n",
        "closedupvalue",
    );
    session.set_breakpoint("closedupvalue", 5); // "return count", count already incremented to 1
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");
    let upvalues = session.get_upvalues(0, 0);
    let count = upvalues
        .iter()
        .find(|v| v.name == "count")
        .expect("count captured by increment");
    assert_eq!(count.display, "1");

    let result = session.set_variable(0, 0, "count", "555");
    assert!(result.ok, "{}", result.display);

    let stop2 = session.continue_();
    assert_eq!(stop2.reason, "terminated");
    assert_eq!(session.take_output(), "555\n");
}

#[test]
fn set_variable_reports_an_error_for_an_unknown_name() {
    let mut session = DebugSession::launch("local x = 1\nprint(x)", "unknownvar");
    session.set_breakpoint("unknownvar", 2);
    session.continue_();
    let result = session.set_variable(0, 0, "does_not_exist", "1");
    assert!(!result.ok);
    assert_eq!(
        result.display,
        "no local or upvalue named 'does_not_exist' in this frame"
    );
}

#[test]
fn memory_stats_report_consistent_nonzero_allocation() {
    // `gc-arena`'s incremental collector runs opportunistically during
    // ordinary stepping (paced by its own allocation-debt heuristic, not
    // under this crate's control), so asserting an exact byte delta across
    // two points in a running program is inherently timing-dependent and
    // not something this test should rely on. What *is* guaranteed
    // regardless of collector timing: there is some live allocation to
    // report, and `total_allocation` is always exactly the sum of its two
    // parts (arithmetic, not GC-timing dependent - see `Metrics::total_allocation`).
    let mut session = DebugSession::launch(
        "local t = {}\nfor i = 1, 500 do\n  t[i] = { value = i }\nend\nprint('done')",
        "memstats",
    );
    session.set_breakpoint("memstats", 5); // "print('done')" - t and its entries are all still live
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");

    let stats = session.get_memory_stats();
    assert!(stats.total_allocation > 0.0, "{stats:?}");
    assert!(stats.gc_allocation > 0.0, "{stats:?}");
    assert!(stats.allocation_debt >= 0.0, "{stats:?}");
    assert_eq!(
        stats.total_allocation,
        stats.gc_allocation + stats.external_allocation,
        "{stats:?}"
    );
}

#[test]
fn force_gc_drains_allocation_debt_without_losing_reachable_data() {
    // The main safety property a debugger's "Force GC" action needs: it
    // must not corrupt or lose data the program can still see, even though
    // it's forcing a full collection cycle mid-execution. `allocation_debt`
    // resetting to zero is `gc_collect`'s own deterministic signature of a
    // completed cycle (see its doc comment / `arena.collect_all()`'s
    // postcondition) - unlike `total_allocation`, which piccolo's
    // incremental collector may have already reduced opportunistically
    // during ordinary stepping before this call, so it isn't a reliable
    // signal to assert on here.
    let mut session = DebugSession::launch(
        "local t = {}\nfor i = 1, 1000 do\n  t[i] = { value = i }\nend\nprint('done')",
        "forcegc",
    );
    session.set_breakpoint("forcegc", 5); // "print('done')" - t and its 1000 entries are all still live
    let stop = session.continue_();
    assert_eq!(stop.reason, "breakpoint");

    session.force_gc();
    let after = session.get_memory_stats();
    assert_eq!(after.allocation_debt, 0.0, "{after:?}");

    // `t` (still referenced by the paused frame) and its contents must
    // still be there, correctly, after a forced collection. Checked by
    // index rather than `#t` - the length operator isn't reliable through
    // `evaluate()`'s wrapper-chunk snapshot regardless of `force_gc`, since
    // it depends on the table's internal array-part border, not on GC state.
    let first = session.evaluate(0, "t[1].value", 0);
    assert!(first.ok, "{}", first.display);
    assert_eq!(first.display, "1");
    let middle = session.evaluate(0, "t[500].value", 0);
    assert!(middle.ok, "{}", middle.display);
    assert_eq!(middle.display, "500");
    let last = session.evaluate(0, "t[1000].value", 0);
    assert!(last.ok, "{}", last.display);
    assert_eq!(last.display, "1000");
}
