//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: coroutines).

#[test]
fn dynamic_lua_runtime_coroutine_create_resume_yield_round_trip() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local co = coroutine.create(function(a, b)
            local c = coroutine.yield(a + b)
            local d = coroutine.yield(c * 2)
            return d + 1
        end)

        local ok1, first = coroutine.resume(co, 1, 2)
        local ok2, second = coroutine.resume(co, 10)
        local ok3, third = coroutine.resume(co, 100)
        local ok4, msg4 = coroutine.resume(co)

        return ok1 == true and first == 3 and
            ok2 == true and second == 20 and
            ok3 == true and third == 101 and
            ok4 == false and msg4 == "cannot resume dead coroutine" and
            coroutine.status(co) == "dead"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_yields_across_pcall_but_rejects_a_gsub_c_boundary() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function helper(x)
            return coroutine.yield(x + 1)
        end

        local co = coroutine.create(function()
            local a = helper(1)
            local ok, b = pcall(function() return coroutine.yield(a + 1) end)
            local replaced = string.gsub("xy", "%a", function(ch)
                return coroutine.yield(ch)
            end)
            return ok, b, replaced
        end)

        local _, from_helper = coroutine.resume(co)
        local _, from_pcall = coroutine.resume(co, 10)
        local ok_gsub, gsub_error = coroutine.resume(co, 20)

        return from_helper == 2 and from_pcall == 11 and
            ok_gsub == false and
            gsub_error == "attempt to yield across a C-call boundary" and
            coroutine.status(co) == "dead"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_status_transitions_and_running_isyieldable() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local outer_running, outer_main = coroutine.running()
        local outer_yieldable = coroutine.isyieldable()

        local statuses = {}
        local co
        co = coroutine.create(function()
            statuses[#statuses + 1] = coroutine.status(co)
            local inner_running, inner_main = coroutine.running()
            local inner_yieldable = coroutine.isyieldable()
            coroutine.yield(inner_running == co, inner_main == false, inner_yieldable == true)
        end)

        statuses[#statuses + 1] = coroutine.status(co)
        local ok, a, b, c = coroutine.resume(co)
        statuses[#statuses + 1] = coroutine.status(co)
        coroutine.resume(co)
        statuses[#statuses + 1] = coroutine.status(co)

        return outer_running ~= nil and coroutine.status(outer_running) == "running" and
            outer_main == true and outer_yieldable == false and
            statuses[1] == "suspended" and statuses[2] == "running" and
            statuses[3] == "suspended" and statuses[4] == "dead" and
            ok == true and a == true and b == true and c == true
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_wrap_propagates_a_real_error() {
    use sol::lua_runtime::run_source;

    let source = br#"
        local wrapped = coroutine.wrap(function()
            coroutine.yield(1)
            error("boom")
        end)
        assert(wrapped() == 1)
        wrapped()
    "#;
    let error = run_source(source).unwrap_err();
    assert_eq!(error.message, "boom");
}

#[test]
fn dynamic_lua_runtime_coroutine_rejects_resuming_dead_running_and_normal_coroutines() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local outer, inner
        outer = coroutine.create(function()
            local self_ok, self_msg = coroutine.resume(outer)
            local resumed_ok, inner_ok, inner_msg = coroutine.resume(inner)
            coroutine.yield(self_ok, self_msg, resumed_ok, inner_ok, inner_msg)
        end)
        inner = coroutine.create(function()
            return coroutine.resume(outer)
        end)

        local start_ok, self_ok, self_msg, resumed_ok, inner_ok, inner_msg = coroutine.resume(outer)
        local dead_ok, dead_msg = coroutine.resume(outer)
        local yield_ok, yield_msg = pcall(coroutine.yield)

        return start_ok == true and
            self_ok == false and self_msg == "cannot resume non-suspended coroutine" and
            resumed_ok == true and inner_ok == false and
            inner_msg == "cannot resume non-suspended coroutine" and
            dead_ok == true and dead_msg == nil and
            coroutine.status(outer) == "dead" and coroutine.status(inner) == "dead" and
            yield_ok == false and yield_msg == "attempt to yield from outside a coroutine"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_yields_from_deep_inside_a_pcall_chain_and_resumes_to_completion() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // The coroutine's body nests 50,000 `pcall`-wrapped recursive calls deep
    // (each dispatches through `Frame::Native(NativeCont::Pcall)` on the
    // coroutine's *own* frame stack, not `LuaRuntime::frames`), and the
    // innermost level calls `coroutine.yield` instead of returning directly.
    // This proves `StepResult::Yield` correctly escapes the entire `drive`
    // invocation from underneath 50,000 stacked `Pcall` markers, leaving
    // every one of them intact, and that the next `resume`'s value is then
    // threaded back up through all of them (each unwinding as a normal
    // successful `pcall`) to become the coroutine's own return value.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            if n == 0 then
                return coroutine.yield("leaf")
            end
            local ok, result = pcall(make_chain, n - 1)
            if not ok then error(result) end
            return result
        end

        local co = coroutine.create(function() return make_chain(50000) end)
        local ok1, yielded = coroutine.resume(co)
        local ok2, final_value = coroutine.resume(co, "resumed")

        return ok1 == true and yielded == "leaf" and
            ok2 == true and final_value == "resumed" and
            coroutine.status(co) == "dead"
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_rejects_yield_from_deep_inside_a_gsub_function_replacement_chain() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Lua 5.5 marks `string.gsub`'s replacement callback as a non-yieldable
    // C-call boundary. Exercise that rule after 50,000 native continuation
    // frames to ensure the error unwinds iteratively and kills the coroutine
    // without overflowing the native Rust stack.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        counter = { n = 50000 }
        local function replacer(text)
            counter.n = counter.n - 1
            if counter.n > 0 then
                return (string.gsub("x", "x", replacer))
            end
            return coroutine.yield(text)
        end

        local co = coroutine.create(function() return string.gsub("x", "x", replacer) end)
        local ok1, yielded = coroutine.resume(co)
        return ok1 == false and
            yielded == "attempt to yield across a C-call boundary" and
            counter.n == 0 and coroutine.status(co) == "dead"
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_body_recurses_deeply_between_yields_without_native_stack_overflow()
{
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // The coroutine body itself performs 50,000 levels of ordinary
    // (non-tail) recursive Lua-to-Lua calls before its single
    // `coroutine.yield`, and another 50,000 return-value additions unwind
    // after resuming - all on the coroutine's own `Vec<Frame>`, never
    // `LuaRuntime::frames` directly (that field only holds the *caller's*
    // frames for the duration of `resume`). Proves depth here is carried by
    // the coroutine's explicit frame stack, not the native Rust call stack,
    // the same property `dynamic_lua_runtime_drives_deep_call_chains_without_native_stack_overflow`
    // proves for ordinary (non-coroutine) calls.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function descend(depth)
            if depth == 0 then
                return coroutine.yield(0)
            end
            return 1 + descend(depth - 1)
        end

        local co = coroutine.create(function() return descend(50000) end)
        local ok1, yielded = coroutine.resume(co)
        local ok2, final_value = coroutine.resume(co, 0)

        return ok1 == true and yielded == 0 and
            ok2 == true and final_value == 50000 and
            coroutine.status(co) == "dead"
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_survives_several_separate_resume_yield_round_trips_each_doing_nested_work(
) {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Five separate `resume`/`yield` round trips (five distinct, separately
    // scheduled `resume_coroutine` invocations - no single Rust call stack
    // frame spans more than one of them), each performing genuinely nested
    // work (a `pcall`-wrapped recursive closure chain) between yields, and
    // each round's resume value flowing back out correctly. Complements the
    // error-unwind test above (which specifically targets `depth_charged`
    // surviving into an error path) by instead exercising the ordinary
    // repeated-suspend-and-resume path several times over.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 10_000);
    let source = br#"
        local function nest(depth)
            if depth == 0 then return 0 end
            local ok, result = pcall(nest, depth - 1)
            if not ok then error(result) end
            return 1 + result
        end

        local completed = 0
        local co = coroutine.create(function()
            for round = 1, 5 do
                local nested = nest(200)
                coroutine.yield(nested, round)
                completed = completed + 1
            end
            return "done", completed
        end)

        local rounds_ok = true
        local resume_value = nil
        for i = 1, 5 do
            local ok, nested, round = coroutine.resume(co, resume_value)
            if not (ok == true and nested == 200 and round == i) then
                rounds_ok = false
            end
            resume_value = i * 10
        end
        local ok_done, done_message, done_count = coroutine.resume(co, resume_value)

        return rounds_ok and ok_done == true and done_message == "done" and
            done_count == 5 and coroutine.status(co) == "dead"
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_resume_yield_round_trip_correctly_unwinds_depth_charge_on_a_later_error(
) {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `LuaCoroutine::depth_charged` exists specifically to survive between
    // separate top-level `resume_coroutine` invocations (no Rust stack frame
    // survives a yield/resume round trip, unlike `drive`'s own `&mut usize`
    // local). This test is the one place that actually discriminates a
    // broken/reset `depth_charged`: on the first resume, `nest` recurses
    // 500 levels deep and yields, leaving 500 charged `Frame::Lua`s on the
    // coroutine's own stack (`LuaRuntime::call_depth` stays elevated by
    // that much even while the coroutine is merely *suspended*, not
    // dropped). The second resume then raises an uncaught error with all
    // 500 of those frames still live, requiring `resume_coroutine`'s error
    // path to release the *full* accumulated charge - 500 units carried over
    // from the first resume, not just whatever (if anything) this second
    // resume itself added - back out of `call_depth`. If `depth_charged`
    // were reset to 0 at the start of the second resume instead of loaded
    // from `co.depth_charged`, those 500 units would leak permanently, and
    // the unrelated, independent 500-deep `descend` call made afterwards
    // (comfortably under the 700 call-depth budget on its own) would
    // instead push `call_depth` over budget and fail.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 700);
    let source = br#"
        local function nest(depth)
            if depth == 0 then
                coroutine.yield("leaf")
                error("boom")
            end
            local result = 1 + nest(depth - 1)
            return result
        end

        local co = coroutine.create(function() return nest(500) end)
        local ok1, leaf = coroutine.resume(co)
        local ok2, msg2 = coroutine.resume(co)

        local function descend(depth)
            if depth == 0 then return 0 end
            return 1 + descend(depth - 1)
        end
        local after = descend(500)

        return ok1 == true and leaf == "leaf" and
            ok2 == false and msg2 == "boom" and
            coroutine.status(co) == "dead" and
            after == 500
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coroutine_wrap_keeps_its_identity_through_a_table_round_trip() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Regression test for a `codec.rs` bug: `LuaValue::CoroutineWrapper` and
    // `LuaValue::Thread` both wrap the same `HeapObject::Thread`
    // representation and encode identically, so `decode_object` always
    // decoded a value read back out of canonical storage (here, a table
    // entry) as a plain `Thread` - silently turning a `coroutine.wrap`
    // closure into an uncallable thread value the moment it round-tripped
    // through a table. `type(t.co)` and calling `t.co(...)` both exercise the
    // round trip.
    let source = br#"
        local t = {}
        t.co = coroutine.wrap(function(a, b)
            local c = coroutine.yield(a + b)
            return c * 2
        end)

        local type_before_call = type(t.co)
        local first = t.co(1, 2)
        local second = t.co(10)

        return type_before_call == "function" and first == 3 and second == 20
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}
