//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of
//! lua55_dynamic_runtime_core.rs, grouped by theme: closures, locals/globals/_ENV, varargs, to-be-closed variables).

#[test]
fn dynamic_lua_runtime_keeps_values_tables_closures_and_modules_separate() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br##"
        global *
        global answer = 40
        global <const> label = "value"
        local data = { answer + 2, [label] = answer }
        local function make_adder(value)
            return function(extra) return value + extra end
        end
        local add_two = make_adder(2)
        assert(data[1] == 42 and data.value == 40)
        print(label)
        return add_two(answer)
    "##;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Integer(42));
    assert_eq!(run.output, b"value\n");

    let other_module = run_source(b"global answer = 7; return answer").unwrap();
    assert_eq!(other_module.value, LuaValue::Integer(7));
    let mutable_upvalue = run_source(b"local function make() local value = 0 return function() value = value + 1 return value end end local next = make() return next() + next()").unwrap();
    assert_eq!(mutable_upvalue.value, LuaValue::Integer(3));
    let error = run_source(b"global <const> answer = 1; answer = 2").unwrap_err();
    assert!(error.message.contains("const variable 'answer'"), "{error}");
}

#[test]
fn dynamic_lua_runtime_and_or_do_not_clobber_a_local_operand_in_place() {
    // Regression test: `x and y` / `x or y` codegen used to reuse a bare
    // local's own register as the expression's scratch destination, so
    // evaluating the expression overwrote the local itself whenever its
    // branch was taken (e.g. `local y = x and 2` used to leave `x == 2`).
    use sol::lua_runtime::{run_source, LuaValue};

    let and_case = run_source(b"local x = 1 local y = x and 2 return x == 1 and y == 2").unwrap();
    assert_eq!(and_case.value, LuaValue::Bool(true));

    let or_case =
        run_source(b"local x = false local y = x or 2 return x == false and y == 2").unwrap();
    assert_eq!(or_case.value, LuaValue::Bool(true));

    let chained = run_source(
        br#"
        local i, v = 5, "value"
        local a = { [5] = "value" }
        local ok = i and v and a[i] == v
        return i == 5 and v == "value" and ok == true
        "#,
    )
    .unwrap();
    assert_eq!(chained.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_next_resumes_after_the_current_key_is_deleted_mid_traversal() {
    // Regression test: real Lua's `next` explicitly allows setting the key
    // it just returned to nil mid-traversal ("you may set existing fields
    // to nil"). `LuaTable::set` used to remove a hash-part key outright on
    // `t[k] = nil`, so a subsequent `next(t, k)` resuming from that exact
    // key could no longer find its position and raised "invalid key to
    // 'next'" instead of continuing the traversal.
    use sol::lua_runtime::{run_source, LuaValue};

    let run = run_source(
        br#"
        local t = { [{1}] = 1, [{2}] = 2, [string.rep("x ", 4)] = 3,
                    [100.3] = 4, [4] = 5 }
        local n = 0
        for k, v in pairs(t) do
            n = n + 1
            assert(t[k] == v)
            t[k] = nil
            assert(t[k] == nil)
        end
        return n
        "#,
    )
    .unwrap();
    assert_eq!(run.value, LuaValue::Integer(5));
}

#[test]
fn dynamic_lua_runtime_next_traversal_survives_deletions_in_a_large_table() {
    // Regression test: `LuaTable::hash` used to be a plain `std::HashMap`.
    // `HashMap::insert` can trigger a capacity-growth rehash even when it
    // ends up only overwriting an already-present key's value (the growth
    // check runs before key existence is known), and such a rehash reorders
    // the map's iteration order with no change to its key set. `next`
    // resumes traversal by re-locating the last-returned key in a freshly
    // fetched snapshot and continuing from the position right after it, so a
    // reorder triggered by `t[k] = nil` (a same-key overwrite, since deleted
    // hash keys are tombstoned rather than removed) could strand
    // not-yet-visited entries before that position, silently truncating the
    // traversal. `LuaTable::hash` is now an `indexmap::IndexMap`, which never
    // repositions an existing key on overwrite. Use enough entries to cross
    // several capacity-growth boundaries during construction.
    use sol::lua_runtime::{run_source, LuaValue};

    let run = run_source(
        br#"
        local t = {}
        for i = 1, 40 do
            t[tostring(i)] = i
            t[{i}] = i
        end
        local n = 0
        for k, v in pairs(t) do
            n = n + 1
            assert(t[k] == v)
            t[k] = nil
            assert(t[k] == nil)
        end
        assert(next(t) == nil)
        return n
        "#,
    )
    .unwrap();
    assert_eq!(run.value, LuaValue::Integer(80));
}

#[test]
fn dynamic_lua_runtime_closures_survive_register_slot_reuse_after_their_scope_ends() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Once `do ... end` closes, its locals' register numbers are free for
    // later statements to reuse as plain temporaries. If a closure escaped
    // that block by capturing one of those locals, a later temporary
    // computation landing back on the same register number must not
    // corrupt the closure's captured value through a shared cell.
    let source = br#"
        local caps = {}
        do
            local x = 99
            caps[1] = function() return x end
        end
        local w = 1 + 2
        local w2 = w + 5
        local w3 = w2 * 2
        return caps[1]() == 99 and w3 == 16
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_supports_table_and_function_valued_keys() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua allows any non-nil, non-NaN value as a table key, including
    // tables and functions (compared/hashed by identity). `LuaTable`/`LuaKey`
    // used to only support bool/integer/float/string keys.
    let source = br#"
        local t = {}
        local key1 = {}
        local key2 = function() end
        t[key1] = "table key"
        t[key2] = "function key"
        local count = 0
        for _, _ in pairs(t) do count = count + 1 end
        return t[key1] == "table key" and t[key2] == "function key" and count == 2
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_applies_table_iteration_and_multi_result_rules() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        global *
        global state = true
        local function values() return 1, nil, 3 end
        local first, second, third = values()
        local table_value = { 0, values() }
        local total = 0
        for key, value in ipairs(table_value) do
            if value then total = total + value end
        end
        rawset(table_value, "name", 7)
        local a, b = 10, 20
        a, b = b, a
        return first == 1 and second == nil and third == 3 and
            table_value[2] == 1 and table_value[3] == nil and table_value[4] == 3 and
            total == 1 and rawget(table_value, "name") == 7 and rawlen(table_value) == 2 and
            a == 20 and b == 10
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_protects_errors_and_preserves_varargs() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br##"
        global *
        global enabled = true
        local function fail(value) error(value) end
        local ok, message = pcall(fail, "expected failure")
        local handled, transformed = xpcall(function() error("bad") end, function(value) return "handled:" .. value end)
        local function count(...) return select("#", ...), select(2, ...) end
        local amount, second = count(1, 2, 3)
        return not ok and message == "expected failure" and not handled and transformed == "handled:bad" and amount == 3 and second == 2
    "##;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_capabilities_and_budgets_can_be_overridden_together() {
    use sol::lua_runtime::{run_program_with_natives_and_budgets, Capabilities, LuaRuntime};
    use std::collections::{HashMap, HashSet};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let program = parse(b"local total = 0 for i = 1, 5000 do total = total + i end return total");

    // The default 1,000,000-instruction budget is generous enough for this
    // loop already; a tiny explicit override should still be enforced, and
    // a large override on the same runtime constructor should let a loop
    // that would otherwise exceed the tiny budget run to completion - this
    // is the CLI's `sol run` path (`main.rs`'s `SOL_LUA_INSTRUCTION_BUDGET`
    // and friends), exercised here without needing to spawn the binary.
    let mut too_tight = LuaRuntime::with_capabilities_and_budgets(
        Capabilities::NATIVE_CLI,
        10,
        1_000,
        64 * 1024 * 1024,
    );
    let error = too_tight.run(&program).unwrap_err();
    assert!(error.message.contains("instruction budget"), "{error}");

    let result = run_program_with_natives_and_budgets(
        &program,
        &HashSet::new(),
        HashMap::new(),
        Capabilities::NATIVE_CLI,
        1_000_000,
        1_000,
        64 * 1024 * 1024,
        false,
        None,
    )
    .unwrap();
    assert_eq!(
        result.value,
        sol::lua_runtime::LuaValue::Integer(5000 * 5001 / 2)
    );
}

#[test]
fn dynamic_lua_runtime_uses_real_lexical_environment_tables() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        assert(_G._G == _G and _ENV == _G)
        _G.root_value = 9
        local env = { x = 10, root_value = 100 }
        local read
        do
            local _ENV = env
            x = x + 1
            read = function() return x + root_value end
        end
        return root_value == 9 and env.x == 11 and read() == 111
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_named_varargs_are_packed_with_an_explicit_count() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function packed(...values)
            return values.n, values[1], values[2], values[3]
        end
        local n, first, second, third = packed(10, nil, 30)
        local function invoke(fn, arguments)
            return fn(table.unpack(arguments, 1, arguments.n))
        end
        local unpacked = invoke(function(a, b) return a + b end, { 20, 22 })
        return n == 3 and first == 10 and second == nil and third == 30 and unpacked == 42
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_coerces_complete_numeric_strings_only_where_lua_does() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local comparison_errors = not pcall(function() return "2" < 3 end)
        local nan = 0 / 0
        return "2" + 3 == 5 and "10" - 4 == 6 and
            -"2.5" == -2.5 and ("0x0f" & 7) == 7 and
            "0x1.8p1" * 2 == 6 and tonumber("  -0x1.8p1  ") == -3 and
            math.sqrt("81") == 9 and comparison_errors and
            math.mininteger < math.mininteger + 1 and
            math.maxinteger ~= math.maxinteger + 0.0 and
            not (nan < 0) and not (0 <= nan)
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));

    assert!(run_source(b"return '2 trailing' + 1").is_err());
    assert!(run_source(b"return '2 -- comment' + 1").is_err());
}

#[test]
fn dynamic_lua_runtime_applies_duplicate_global_function_declarations_in_order() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        function transform(value) return value + 1 end
        local before = transform(10)
        function transform(value) return value * 2 end
        local after = transform(10)
        return before == 11 and after == 20
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_multi_assign_resolves_targets_before_any_store() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/attrib.lua`'s "test conflicts in multiple assignment"
    // section: every target's own table/key sub-expression must be resolved
    // using the values in effect *before* the statement's assignments
    // began, even though one of the earlier targets in the very same
    // statement reassigns the variable a later target's sub-expression
    // reads (`a` is reassigned by the third target here, but the fifth and
    // sixth targets - `a[j]`, `a[i+j]` - must still index the original
    // table). This was a genuine bug: target addressing used to be resolved
    // one at a time, interleaved with each target's own store, so an
    // earlier store could corrupt a later target's addressing.
    let source = br#"
        local a, i, j, b
        a = {'a', 'b'}; i = 1; j = 2; b = a
        i, a[i], a, j, a[j], a[i+j] = j, i, i, b, j, i
        assert(i == 2 and b[1] == 1 and a == 1 and j == b and b[2] == 2 and b[3] == 1)

        local a2, i2, j2, b2
        a2 = {'a', 'b'}; i2 = 1; j2 = 2; b2 = a2
        local function foo()
            i2, a2[i2], a2, j2, a2[j2], a2[i2+j2] = j2, i2, i2, b2, j2, i2
        end
        foo()
        return i2 == 2 and b2[1] == 1 and a2 == 1 and j2 == b2 and b2[2] == 2 and b2[3] == 1
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_method_calls_preserve_self_and_support_trailing_multi_value_args() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br##"
        local t = { name = "world" }
        function t:greet(suffix) return "hello " .. self.name .. suffix end
        local direct = t:greet("!")

        local function pack_all(...) return select("#", ...) end
        local formatted = pack_all(("%s-%s"):format("a", "b"))

        return direct == "hello world!" and formatted == 1
    "##;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_dotted_and_method_function_declarations_assign_table_fields() {
    use sol::lua_runtime::{run_source, LuaValue};

    let nested = br#"
        local t = {}
        do
            function t.greet(x) return "hi " .. x end
            function t:shout(x) return self.tag .. x end
        end
        t.tag = "yo "
        return t.greet("a") == "hi a" and t:shout("b") == "yo b"
    "#;
    let run = run_source(nested).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));

    let top_level = br#"
        local obj = {}
        function obj.method(x) return x + 1 end
        return obj.method(41)
    "#;
    let run = run_source(top_level).unwrap();
    assert_eq!(run.value, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_function_statement_rebinds_visible_chunk_local() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function transform(value) return value + 1 end
        local before = transform(4)
        function transform(value) return value * 2 end
        return before == 5 and transform(4) == 8 and _G.transform == nil
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_top_level_function_captures_a_preceding_chunk_local_as_an_upvalue() {
    // Regression test: a plain top-level `function NAME(...) end` whose body
    // references a preceding chunk-local as a free variable must compile
    // in-order (as `Stmt::GlobalFunction`) so it can actually capture that
    // local as an upvalue, rather than being hoisted into the independently-
    // compiled `functions` list (which has no enclosing scope). See
    // `function_references_any_name` in `parser.rs`.
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local counter = 0
        function bump()
            counter = counter + 1
            return counter
        end
        return bump() == 1 and bump() == 2 and counter == 2
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_loop_local_captured_by_a_closure_survives_a_guard_condition_reusing_its_register(
) {
    // Regression test (minimized from upstream `closure.lua`'s line ~140):
    // a loop-body local later captured as an upvalue by a nested closure
    // used to be assignable to the same register number as an earlier-
    // compiled, textually-preceding scratch temp within the same loop body
    // (here, the `while` guard's `not first` computation). Because a loop's
    // body is compiled once but executed repeatedly, the guard's scratch-
    // temp code re-runs on a later iteration that returns before the local
    // is re-declared, silently overwriting the live closure's captured
    // cell with the guard's boolean scratch value instead of leaving it as
    // "xuxu". Fixed by `LoopCtx.reg_floor` in `lua_bytecode/func_state.rs`.
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local accessor
        local function make()
            local first = 1
            while true do
                if true and not first then return end
                local a = "xuxu"
                accessor = function(op)
                    if op == "set" then
                        a = 99
                    else
                        return a
                    end
                end
                first = nil
            end
        end
        make()
        return accessor("get") == "xuxu"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_named_vararg_mutations_control_later_expansion() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function aux(a, values, ...pack)
            for key, value in pairs(values) do pack[key] = value end
            return ...
        end
        local first = table.pack(aux(10, {11, [5] = 24}, 1, 2, 3, nil, 4))
        local second = table.pack(aux(nil, {1, [20] = "a", [30] = "b", n = 30}))
        return first.n == 5 and first[1] == 11 and first[5] == 24 and
            second.n == 30 and second[1] == 1 and second[20] == "a" and second[30] == "b"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_bare_global_declaration_does_not_clobber_existing_bindings() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        global <const> print, assert
        global counter
        counter = 41
        global counter
        assert(print ~= nil)
        return counter + 1
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_named_global_declaration_is_block_scoped_and_shadows_an_outer_local() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local x = 10
        do global x; x = 20 end
        local inner_after = x
        return inner_after == 10 and _ENV.x == 20
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_declaring_env_as_a_named_global_poisons_all_global_access() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"global _ENV, a; a = 10").unwrap_err();
    assert!(error.message.contains("_ENV is global"), "{error}");
}

#[test]
fn dynamic_lua_runtime_env_is_exempt_from_the_strict_declared_check() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        global<const> foo
        return _ENV.foo == nil
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_global_function_declaration_shadows_an_outer_local_of_the_same_name() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local foo = 20
        local inside_matches
        local inside_call
        do
            global function foo(x)
                if x == 0 then return 1 else return 2 * foo(x - 1) end
            end
            inside_matches = (foo == _ENV.foo)
            inside_call = foo(4)
        end
        return inside_matches and inside_call == 16 and _ENV.foo(4) == 16 and foo == 20
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_global_declaration_with_an_initializer_errors_if_already_defined() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"global print = 10").unwrap_err();
    assert!(
        error.message.contains("global 'print' already defined"),
        "{error}"
    );

    let error = run_source(b"global function print() end").unwrap_err();
    assert!(
        error.message.contains("global 'print' already defined"),
        "{error}"
    );
}

#[test]
fn dynamic_lua_runtime_global_already_defined_check_also_applies_to_a_rebound_env_table() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"local _ENV = {AA = false}; global AA = 10").unwrap_err();
    assert!(
        error.message.contains("global 'AA' already defined"),
        "{error}"
    );
}

#[test]
fn dynamic_lua_runtime_assert_default_message_matches_reference_lua() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"assert(false)").unwrap_err();
    assert_eq!(error.message, "assertion failed!");

    let error = run_source(b"assert(1 == 2, \"custom message\")").unwrap_err();
    assert_eq!(error.message, "custom message");
}

#[test]
fn dynamic_lua_runtime_generic_for_loop_variables_are_implicitly_const() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"for v, k in pairs({}) do v = 10 end").unwrap_err();
    assert!(
        error.message.contains("assign to const variable 'v'"),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn dynamic_lua_runtime_close_variables_run_on_normal_exit_break_and_return() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local function tracker(name, log)
            return setmetatable({}, { __close = function() log[#log + 1] = name end })
        end

        local block_log = {}
        do
            local a <close> = tracker("a", block_log)
            local b <close> = tracker("b", block_log)
        end

        local break_log = {}
        for i = 1, 5 do
            local c <close> = tracker("c", break_log)
            if i == 2 then break end
        end

        local return_log = {}
        local function f()
            local d <close> = tracker("d", return_log)
            return 42
        end
        local result = f()

        return table.concat(block_log, ",") == "b,a"
            and table.concat(break_log, ",") == "c,c"
            and table.concat(return_log, ",") == "d"
            and result == 42
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_close_variables_run_on_error_unwind_with_the_error_value() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local closed_with = nil
        local ok, err = pcall(function()
            local e <close> = setmetatable({}, {
                __close = function(_, err) closed_with = err end
            })
            error("boom")
        end)
        return ok == false and err:find("boom") ~= nil
            and closed_with ~= nil and closed_with:find("boom") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_close_rejects_a_non_closable_value() {
    use sol::lua_runtime::run_source;

    let error = run_source(b"local x <close> = 5").unwrap_err();
    assert_eq!(error.message, "variable 'x' got a non-closable value");
}

#[test]
fn dynamic_lua_runtime_close_variables_are_immutable() {
    use sol::lua_runtime::run_source;

    for source in [
        b"local x <close> = nil; x = nil".as_slice(),
        b"local x <close>, y = nil, nil; x, y = nil, nil",
        b"local x <close> = nil; local function assign() x = nil end",
    ] {
        let error = run_source(source).unwrap_err();
        assert!(
            error
                .message
                .contains("attempt to assign to const variable 'x'"),
            "unexpected message: {}",
            error.message
        );
    }
}

#[test]
fn dynamic_lua_runtime_generic_for_closes_an_implicit_fourth_iterator_value() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local closed = false
        local function iter(state, key)
            if key >= 3 then return nil end
            return key + 1, key * 10
        end
        local function make()
            local tbc = setmetatable({}, { __close = function() closed = true end })
            return iter, nil, 0, tbc
        end

        local total = 0
        for k, v in make() do
            total = total + v
            assert(not closed)
        end

        return total == 30 and closed
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_error_with_no_message_or_a_nil_message_becomes_no_error_object() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ok1, msg1 = pcall(function() error() end)
        assert(not ok1 and msg1 == "<no error object>")

        local ok2, msg2 = pcall(function() error(nil) end)
        assert(not ok2 and msg2 == "<no error object>")

        local ok3, msg3 = pcall(function() error("hi", 0) end)
        assert(not ok3 and msg3 == "hi")

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_return_must_be_the_last_statement_in_a_block() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local chunk, err = load("return;;")
        assert(chunk == nil and string.find(err, "last statement"))

        local ok = load("return 1;")
        assert(ok() == 1)

        local nested, nested_err = load("do return 1;; end")
        assert(nested == nil and string.find(nested_err, "last statement"))

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_vertical_tab_and_form_feed_count_as_whitespace_including_inside_a_z_escape()
{
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local src = "x " .. string.char(11, 12) .. " = " .. string.char(9, 13) .. " 1"
        local chunk = assert(load(src))
        chunk()
        assert(x == 1)

        local inner = "'a\\z" .. string.char(11) .. "b'"
        local chunk2 = assert(load("return " .. inner))
        assert(chunk2() == "ab")

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_length_of_requires_an_integer_result_unlike_the_raw_length_operator() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local weird = setmetatable({}, { __len = function() return "abc" end })

        local raw_ok, raw_result = pcall(function() return #weird end)
        assert(raw_ok and raw_result == "abc")

        local ok, err = pcall(table.insert, weird, "x")
        assert(not ok and err == "object length is not an integer")

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_bare_env_reassignment_in_a_loaded_chunk_does_not_leak_to_the_caller() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua treats `_ENV` as an implicit upvalue of every chunk: a bare
    // (non-`local`) assignment `_ENV = {}` replaces that chunk's own
    // upvalue cell, so every later plain-global read/write in the same
    // chunk resolves against the new table - but leaves the caller's
    // globals (and every other already-running chunk) untouched. Sol's
    // compiler used to resolve a name-only `_ENV` assignment as an ordinary
    // global write, which merely stashed a spurious `"_ENV"` key into the
    // *shared* globals table without changing which table subsequent plain
    // names resolved against, so `AA = 10` right after kept writing into
    // the caller's own globals.
    let source = br#"
        AA = 0
        local f = load([[
            _ENV = {}
            AA = 10
            return _ENV
        ]])
        local t = f()
        return AA == 0 and t.AA == 10
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
