//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of
//! lua55_dynamic_runtime_core.rs, grouped by theme: calls, pcall/xpcall, goto/labels, metatables, tail calls, depth budgets).

#[test]
fn dynamic_lua_runtime_supports_iterator_triples_goto_and_numeric_key_canonicalization() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local values = { [1] = 20, [2] = 22 }
        assert(values[1.0] == 20)
        local iterator, state, key = pairs(values)
        local total = 0
        while true do
            local next_key, value = iterator(state, key)
            if next_key == nil then break end
            key = next_key
            total = total + value
        end
        local count = 0
        ::again::
        count = count + 1
        if count < 3 then goto again end
        return total == 42 and count == 3
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_rejects_a_duplicate_label_in_the_same_or_a_nested_open_scope() {
    use sol::lua_runtime::run_source;

    let same_scope = run_source(b"::l1:: ::l1::").unwrap_err();
    assert!(same_scope.message.contains("label 'l1'"), "{same_scope}");

    let nested_scope = run_source(b"::l1:: do ::l1:: end").unwrap_err();
    assert!(
        nested_scope.message.contains("label 'l1'"),
        "{nested_scope}"
    );
}

#[test]
fn dynamic_lua_runtime_rejects_a_goto_that_jumps_into_the_scope_of_a_local() {
    use sol::lua_runtime::run_source;

    // Forward goto skips straight over `local aa`'s declaration.
    let flat = run_source(b"goto l1; local aa ::l1:: print(3)").unwrap_err();
    assert!(flat.message.contains("scope of 'aa'"), "{flat}");

    // The goto is inside a nested block that closes before the label, but
    // the label still lands after a later-declared outer local - real Lua
    // (and this check) must catch this even though the naive per-scope
    // local count would look fine (bb/cc go out of scope first).
    let bubbled = run_source(
        br#"
        do local bb, cc; goto l1; end
        local aa
        ::l1:: print(3)
        "#,
    )
    .unwrap_err();
    assert!(bubbled.message.contains("scope of 'aa'"), "{bubbled}");

    // A `repeat...until` body shares its scope with the `until` condition,
    // so real Lua disallows "continuing" past a body-local via the usual
    // last-statement-of-block exception.
    let repeat_until = run_source(
        br#"
        local x = 1
        repeat
            if x then goto cont end
            local xuxu = 10
            ::cont::
        until xuxu < x
        "#,
    )
    .unwrap_err();
    assert!(
        repeat_until.message.contains("scope of 'xuxu'"),
        "{repeat_until}"
    );
}

#[test]
fn dynamic_lua_runtime_rejects_a_goto_that_jumps_into_the_scope_of_a_global_star_declaration() {
    use sol::lua_runtime::run_source;

    // `global *`/`global none` are block-scoped pseudo-locals for goto-scope
    // purposes, exactly like a real `local` - a forward goto may not skip
    // over one to reach a label. Matches lua-5.5.1-tests/goto.lua's
    // `errmsg([[ goto l2; global *; ::l1:: ::l2:: print(3) ]], "scope of '*'")`.
    let err = run_source(b"goto l2; global *; ::l1:: ::l2:: print(3)").unwrap_err();
    assert!(err.message.contains("scope of '*'"), "{err}");
}

#[test]
fn dynamic_lua_runtime_allows_a_goto_to_skip_a_blocks_own_locals_to_reach_its_last_label() {
    use sol::lua_runtime::{run_source, LuaValue};

    // The "goto continue" idiom: a forward `goto` inside a loop body may
    // skip over locals the body itself declares, as long as the label is
    // the last statement of that body (just before the loop's own `end`).
    let while_loop = br#"
        local total = 0
        local i = 0
        while i < 3 do
            i = i + 1
            if i == 2 then goto continue end
            local skipped = i * 100
            total = total + skipped
            ::continue::
        end
        return total
    "#;
    assert_eq!(
        run_source(while_loop).unwrap().value,
        LuaValue::Integer(100 + 300)
    );

    let numeric_for = br#"
        local total = 0
        for i = 1, 3 do
            if i == 2 then goto continue end
            local skipped = i * 100
            total = total + skipped
            ::continue::
        end
        return total
    "#;
    assert_eq!(
        run_source(numeric_for).unwrap().value,
        LuaValue::Integer(100 + 300)
    );

    let generic_for = br#"
        local total = 0
        for _, v in ipairs({1, 2, 3}) do
            if v == 2 then goto continue end
            local skipped = v * 100
            total = total + skipped
            ::continue::
        end
        return total
    "#;
    assert_eq!(
        run_source(generic_for).unwrap().value,
        LuaValue::Integer(100 + 300)
    );

    // A goto out of a loop entirely, to a label declared after the loop,
    // must not be confused with jumping into the scope of a local the loop
    // itself declared (that local's scope is long gone by the time the
    // label after the loop is reached).
    let jump_past_loop = br#"
        while true do
            goto done
            local unused = 1
        end
        ::done::
        return 42
    "#;
    assert_eq!(
        run_source(jump_past_loop).unwrap().value,
        LuaValue::Integer(42)
    );
}

#[test]
fn dynamic_lua_runtime_dispatches_core_metatable_operations() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local fallback = { answer = 40 }
        local writes = {}
        local meta = {
            __index = fallback,
            __newindex = function(_, key, value) rawset(writes, key, value) end,
            __call = function(_, value) return value + 1 end,
            __tostring = function(_) return "object" end,
            __len = function(_) return 7 end,
            __add = function(_, value) return value + 2 end,
            __pairs = function(_)
                return next, { left = 20, right = 22 }, nil
            end
        }
        local object = setmetatable({}, meta)
        local ordering = {
            __lt = function(left, right) return rawget(left, "rank") < rawget(right, "rank") end,
            __eq = function(left, right) return rawget(left, "rank") == rawget(right, "rank") end
        }
        local one = setmetatable({ rank = 1 }, ordering)
        local same = setmetatable({ rank = 1 }, ordering)
        local two = setmetatable({ rank = 2 }, ordering)
        object.created = 9
        local total = 0
        for _, value in pairs(object) do total = total + value end
        print(object)
        return object.answer == 40 and writes.created == 9 and object(41) == 42 and
            #object == 7 and object + 40 == 42 and total == 42 and rawget(object, "answer") == nil and
            one == same and one <= two and two > one
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
    assert_eq!(run.output, b"object\n");
}

#[test]
fn dynamic_lua_runtime_enforces_instruction_and_call_depth_budgets() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut instruction_limited = LuaRuntime::with_limits(5, 100);
    let error = instruction_limited
        .run(&parse(b"while true do end"))
        .unwrap_err();
    assert!(error.message.contains("instruction budget"), "{error}");

    let mut recursion_limited = LuaRuntime::with_limits(10_000, 8);
    let error = recursion_limited
        .run(&parse(
            b"local function recurse() return 1 + recurse() end return recurse()",
        ))
        .unwrap_err();
    assert!(error.message.contains("call-depth budget"), "{error}");

    let mut allocation_limited = LuaRuntime::with_budgets(10_000, 100, 512);
    let error = allocation_limited
        .run(&parse(
            b"for index = 1, 100 do local value = {} end return true",
        ))
        .unwrap_err();
    assert!(error.message.contains("allocation budget"), "{error}");
}

#[test]
fn dynamic_lua_runtime_drives_deep_call_chains_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Ordinary (non-tail) Lua-to-Lua calls nested 100,000 deep: each level
    // adds a stack frame that must survive until its callee returns, so this
    // would overflow the native Rust stack under the old recursive
    // `run_proto`/`call` dispatch. The frame-stack trampoline drives this
    // iteratively instead, bounded only by the configured call-depth budget.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 200_000);
    let source = br#"
        local function descend(depth)
            if depth == 0 then return 0 end
            return 1 + descend(depth - 1)
        end
        return descend(100000)
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(100000));
}

#[test]
fn dynamic_lua_runtime_drives_deep_function_valued_index_metamethod_chains_without_native_stack_overflow(
) {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Each level's `__index` is a Lua closure whose own return value is
    // itself indexed through another level's `__index` before the outer
    // call can complete, nesting 100,000 deep. `Instr::GetIndex` used to
    // resolve a function-valued `__index` via the blocking `LuaRuntime::call`
    // bridge (native Rust recursion, one frame per level); it now resolves
    // through the same frame-stack trampoline as ordinary calls whenever the
    // metamethod is a closure, so this completes without native stack growth.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            local mt = {
                __index = function(_, key)
                    if n == 0 then return 42 end
                    return make_chain(n - 1)[key]
                end
            }
            return setmetatable({}, mt)
        end
        return make_chain(100000).x
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_drives_deep_call_metamethod_chains_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Each level is a plain table whose `__call` metamethod is a Lua
    // closure that calls the next level, nesting 100,000 deep. Calling a
    // `__call`-metatable object used to resolve through the blocking
    // `LuaRuntime::call` bridge (native Rust recursion, one frame per
    // level); it now resolves through the same frame-stack trampoline as
    // ordinary calls whenever the metamethod is a closure, so this
    // completes without native stack growth.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            local mt = { __call = function(_, key)
                if n == 0 then return 42 end
                return make_chain(n - 1)(key)
            end }
            return setmetatable({}, mt)
        end
        return make_chain(100000)("x")
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_successful_pcalls_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Each level wraps its own recursive call in `pcall`, nesting 100,000
    // `Frame::Native(NativeCont::PCall)` markers deep (alternating with a
    // `Frame::Lua` per level), all of which resolve successfully. `pcall`
    // used to call back into Lua through the blocking `LuaRuntime::call`
    // bridge (native Rust recursion, one frame per level); it now pushes a
    // marker onto the same frame-stack trampoline as ordinary calls, so this
    // completes - including unwinding the 100,000 successful markers back
    // to `42` - without native stack growth.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            if n == 0 then return 42 end
            local ok, result = pcall(make_chain, n - 1)
            if not ok then error(result) end
            return result
        end
        return make_chain(100000)
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_pcall_catches_an_error_raised_deep_below_it_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // A single outer `pcall` wraps 100,000 levels of ordinary (not
    // individually protected) recursive calls, the innermost of which
    // raises an error. `LuaRuntime::unwind_error_to_marker` must search
    // across all 100,000 intervening `Frame::Lua` frames to find the single
    // `Frame::Native(NativeCont::PCall)` marker below them and correctly
    // undo their `call_depth` charges, without native stack growth either
    // during the search or during the 100,000-deep call itself.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            if n == 0 then error("bottom") end
            return make_chain(n - 1)
        end
        local ok, err = pcall(make_chain, 100000)
        if ok then error("expected pcall to observe the error") end
        return err
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    match run {
        LuaValue::String(bytes) => assert!(
            bytes.as_bytes().ends_with(b"bottom"),
            "unexpected error message: {bytes:?}"
        ),
        other => panic!("expected a string error value, got {other:?}"),
    }
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_successful_xpcalls_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Same shape as the nested-`pcall` success test above, but through
    // `xpcall`: each level wraps its own recursive call, nesting 100,000
    // `Frame::Native(NativeCont::XCall(XCallStage::Function))` markers deep,
    // all resolving successfully. `xpcall` calls its protected function with
    // no extra arguments (matching the existing blocking implementation
    // this replaces), so depth is tracked through a shared upvalue instead
    // of a call argument.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local counter = 100000
        local function handler(err) return err end
        local function make_chain()
            counter = counter - 1
            if counter == 0 then return 42 end
            local ok, result = xpcall(make_chain, handler)
            if not ok then error(result) end
            return result
        end
        return make_chain()
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_xpcall_handles_an_error_raised_deep_below_it_without_native_stack_overflow()
{
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // A single outer `xpcall` wraps 100,000 levels of ordinary recursive
    // calls, the innermost of which raises an error; `handler` must still
    // run correctly once `unwind_error_to_marker` finds the single
    // `Frame::Native(NativeCont::XCall(XCallStage::Function))` marker below
    // all 100,000 intervening frames and dispatches the transition to
    // `XCallStage::Handler`.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function handler(err) return "handled: " .. err end
        local function make_chain(n)
            n = n or 100000
            if n == 0 then error("bottom") end
            return make_chain(n - 1)
        end
        local ok, err = xpcall(make_chain, handler)
        if ok then error("expected xpcall to observe the error") end
        return err
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    match run {
        LuaValue::String(bytes) => assert!(
            bytes.as_bytes().starts_with(b"handled: "),
            "unexpected error message: {bytes:?}"
        ),
        other => panic!("expected a string error value, got {other:?}"),
    }
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_pairs_metamethod_chains_without_native_stack_overflow()
{
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Each of 100,000 nested tables' `__pairs` metamethod calls `pairs` on
    // the next table down, itself dispatched through `NativeCont::Once`.
    // The whole chain must run on the explicit frame stack, not the native
    // Rust call stack, until the innermost table's `__pairs` finally hands
    // back a real iterator over one key.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        local function make_chain(n)
            if n == 0 then
                return setmetatable({}, { __pairs = function(_) return next, { x = 1 }, nil end })
            end
            return setmetatable({}, { __pairs = function(_) return pairs(make_chain(n - 1)) end })
        end
        local sum = 0
        for k, v in pairs(make_chain(100000)) do
            sum = sum + v
        end
        return sum
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(1));
}

#[test]
fn dynamic_lua_runtime_sorts_via_default_comparator_lt_metamethod() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `table.sort` with no explicit comparator falls back to Lua's `<`
    // operator, which itself must resolve a table element's `__lt`
    // metamethod - this exercises that fallback specifically (as opposed to
    // an explicit-comparator sort, already covered elsewhere), confirming
    // the default-comparator path is just as fully steppable as the
    // explicit-comparator one through `NativeCont::Sort`/`sort_step`.
    let source = br#"
        local mt = { __lt = function(a, b) return a.rank < b.rank end }
        local items = {
            setmetatable({ rank = 3 }, mt),
            setmetatable({ rank = 1 }, mt),
            setmetatable({ rank = 2 }, mt),
        }
        table.sort(items)
        return items[1].rank == 1 and items[2].rank == 2 and items[3].rank == 3
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_reuses_frames_for_proper_tail_calls() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let program = sol::parser::parse_lua(
        sol::lexer::lex_bytes(
            br#"
                local function loop(remaining, total)
                    if remaining == 0 then return total end
                    return loop(remaining - 1, total + 1)
                end
                return loop(5000, 0)
            "#,
        )
        .unwrap(),
    )
    .unwrap();
    let mut runtime = LuaRuntime::with_limits(100_000, 8);
    assert_eq!(runtime.run(&program).unwrap(), LuaValue::Integer(5000));
}

#[test]
fn dynamic_lua_runtime_parenthesized_call_truncates_to_one_value() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function f() return 1, 2, 3 end

        local a, b, c = (f())
        assert(a == 1 and b == nil and c == nil)

        local x, y, z = f()
        assert(x == 1 and y == 2 and z == 3)

        local t = { (f()) }
        assert(#t == 1 and t[1] == 1)

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_index_error_names_the_global_or_field() {
    use sol::lua_runtime::run_source;

    let global_error = run_source(b"return UNDEFINED.member").unwrap_err();
    assert!(
        global_error
            .message
            .contains("attempt to index a nil value (global 'UNDEFINED')"),
        "unexpected message: {}",
        global_error.message
    );

    let field_error = run_source(b"local t = {}; return t.inner.member").unwrap_err();
    assert!(
        field_error
            .message
            .contains("attempt to index a nil value (field 'inner')"),
        "unexpected message: {}",
        field_error.message
    );
}

#[test]
fn dynamic_lua_runtime_custom_env_metatable_intercepts_global_access() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    let source = br#"
        local env = {string = string, assert = assert}
        local prog = "local y = {0,1,2,3}\nX = y\nassert(X[2] == 1)\nreturn 0"
        local f = assert(load(prog, nil, nil, env))

        f()
        assert(env.X[3] == 2 and env.X[4] == 3)
        for k in pairs(env) do env[k] = nil end

        setmetatable(env, {
            __index = function (t, n) coroutine.yield('g'); return _G[n] end,
            __newindex = function (t, n, v) coroutine.yield('s'); _G[n] = v end,
        })

        X = nil
        local co = coroutine.wrap(f)
        assert(co() == 's')
        assert(co() == 'g')
        assert(co() == 'g')
        assert(co() == 0)
        assert(X[3] == 2 and X[4] == 3)

        getmetatable(env).__index = function () end
        getmetatable(env).__newindex = function () end
        local e, m = pcall(f)
        assert(not e and m:find("global 'X'"))

        getmetatable(env).__newindex = function () error("hi") end
        local e2, m2 = xpcall(f, debug.traceback)
        assert(not e2 and m2:find("'newindex'"))

        return true
    "#;
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    assert_eq!(runtime.run(&parse(source)).unwrap(), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_pcall_catches_call_depth_budget_exhaustion() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua's C-stack overflow is an ordinary catchable error - `pcall`
    // must be able to catch it the same way it catches any other runtime
    // error, not have it escape as an unconditional abort.
    let source = br#"
        local function loop() return 1 + pcall(loop) end
        local ok, err = pcall(loop)
        return ok == false and type(err) == "string"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_xpcall_reports_error_in_error_handling_on_handler_double_fault() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua`'s "C-stack overflow while handling
    // C-stack overflow" case: the message handler passed to `xpcall` is the
    // same function that overflowed in the first place, so invoking it
    // overflows again. Real Lua does not retry the handler for its own
    // error - it reports the fixed "error in error handling" message
    // instead of escaping uncaught.
    let source = br#"
        local function loop()
            assert(pcall(loop))
        end
        local ok, msg = xpcall(loop, loop)
        return ok == false and msg == "error in error handling"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_tail_call_through_call_chain_does_not_grow_call_depth() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua`'s "tail calls x chain of '__call'" case
    // (line ~178): `foo` is reassigned 15 times to a table whose `__call`
    // metamethod resolves (transitively) back to the original recursive
    // `foo` closure, then `return foo()` recurses 10000 levels deep as a
    // tail call. Resolving through more than one `__call` hop used to fall
    // back to the blocking, `call_depth`-charging `LuaRuntime::call` bridge
    // instead of becoming a real `StepResult::TailClosure`, so this
    // exhausted the (default 1000) call-depth budget the same way an
    // unbounded *non*-tail recursion would. Real Lua keeps `__call`-chain
    // resolution O(1) C-stack regardless of chain depth or how many times
    // it is repeated, so this must run to completion without erroring.
    let source = br#"
        local n = 10000
        local function foo ()
            if n == 0 then return 1023
            else n = n - 1; return foo()
            end
        end
        for i = 1, 15 do
            foo = setmetatable({}, {__call = foo})
        end
        return coroutine.wrap(function() return foo() end)() == 1023
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_error_names_direct_and_tail_call_sources() {
    use sol::lua_runtime::{run_source, LuaValue};

    let dotted_error = run_source(b"dotted_root.bad_field:bad_method()").unwrap_err();
    assert!(
        dotted_error.message.contains("global 'dotted_root'"),
        "{dotted_error}"
    );
    let loaded_dotted = run_source(
        br#"local chunk = assert(load("dotted_root.bad_field:bad_method()")); local _, err = pcall(chunk); return err"#,
    )
    .unwrap();
    assert!(
        String::from_utf8_lossy(&loaded_dotted.value.display_bytes())
            .contains("global 'dotted_root'"),
        "{}",
        String::from_utf8_lossy(&loaded_dotted.value.display_bytes())
    );
    for source in [
        b"local a, b; return (function() return b + 1 end)()".as_slice(),
        b"local a, b; return (function() return b[1] end)()",
        b"local a; return (function() a.x = 1 end)()",
    ] {
        let error = run_source(source).unwrap_err();
        assert!(
            error.message.contains("upvalue 'b'") || error.message.contains("upvalue 'a'"),
            "{error}"
        );
    }
    let env_error = run_source(b"local _ENV = {}; a = a + 1").unwrap_err();
    assert!(env_error.message.contains("global 'a'"), "{env_error}");
    let right_error = run_source(b"left = 2; right = nil; result = left * right").unwrap_err();
    assert!(
        right_error.message.contains("global 'right'"),
        "{right_error}"
    );
    let unary_error = run_source(b"value = {}; result = -value").unwrap_err();
    assert!(
        unary_error.message.contains("global 'value'"),
        "{unary_error}"
    );
    for source in [
        b"global_value = {}; result = (global_value or global_value) + (global_value and global_value)"
            .as_slice(),
        b"global_value = {}; (global_value or global_value)()",
    ] {
        let error = run_source(source).unwrap_err();
        assert!(
            !error.message.contains("'global_value'"),
            "{error}"
        );
    }
    for (source, expected) in [
        (b"print(print < 10)".as_slice(), "function with number"),
        (b"print(print < print)", "two function values"),
        (b"print('10' < 10)", "string with number"),
        (b"print(10 < '23')", "number with string"),
    ] {
        let error = run_source(source).unwrap_err();
        assert!(error.message.contains(expected), "{error}");
    }
    let bitwise_error = run_source(b"return 34 >> {}").unwrap_err();
    assert!(
        bitwise_error.message.contains("table value"),
        "{bitwise_error}"
    );

    let source = br#"
        local function message(source)
            local chunk = assert(load(source))
            local _, error = pcall(chunk)
            return error
        end
        local global_message = message("bad_global = 1; bad_global()")
        local field = message("local t = {}; t.bad_field()")
        local method = message("local t = {}; t:bad_method()")
        local local_name = message("local bad_local; bad_local()")
        local tail = message("local t = {}; return t.bad_tail()")
        local concat = message("local ignored = 1 .. {}")
        local metamethod = message("local t = setmetatable({}, { __add = 1 }); local ignored = t + 1")
        return global_message:find("global 'bad_global'", 1, true) ~= nil
            and field:find("field 'bad_field'", 1, true) ~= nil
            and method:find("method 'bad_method'", 1, true) ~= nil
            and local_name:find("local 'bad_local'", 1, true) ~= nil
            and tail:find("field 'bad_tail'", 1, true) ~= nil
            and concat:find("concatenate a table value", 1, true) ~= nil
            and metamethod:find("metamethod 'add'", 1, true) ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
