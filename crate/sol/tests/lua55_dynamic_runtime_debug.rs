//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: the debug library).

#[test]
fn dynamic_lua_runtime_implements_debug_upvalueid() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    // Two closures that share a captured upvalue must report the same
    // `debug.upvalueid`; a closure capturing a distinct local must not.
    let value = runtime
        .run(&parse(
            br#"
            local function make()
                local shared = 0
                local other = 0
                local function get_shared() return shared end
                local function set_shared() shared = 1 end
                local function get_other() return other end
                return get_shared, set_shared, get_other
            end
            local get_shared, set_shared, get_other = make()
            local shared_id_a = debug.upvalueid(get_shared, 1)
            local shared_id_b = debug.upvalueid(set_shared, 1)
            local other_id = debug.upvalueid(get_other, 1)
            return shared_id_a == shared_id_b and
                shared_id_a ~= other_id and
                type(shared_id_a) == "userdata"
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getupvalue_reports_named_captures_and_env() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });

    let source = br#"
        local captured = 42
        local function f() return captured end
        local name, value = debug.getupvalue(f, 1)
        local env_name = debug.getupvalue(f, 2)
        local function reads_global() return global_value end
        local global_env_name, global_env = debug.getupvalue(reads_global, 1)
        return name == "captured" and value == 42 and env_name == nil and
            global_env_name == "_ENV" and global_env == _G
    "#;
    assert_eq!(runtime.run(&parse(source)).unwrap(), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_dumped_closure_preserves_assignment_target_upvalue_order() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let source = br#"
        local a, b = 20, 30
        local dumped = string.dump(function (x)
            if x == "set" then a = 10 + b; b = b + 1 else return a end
        end)
        local f = assert(load(dumped, "", "b"))
        return debug.setupvalue(f, 1, "hi") == "a" and f() == "hi" and
            debug.setupvalue(f, 2, 13) == "b"
    "#;
    assert_eq!(runtime.run(&parse(source)).unwrap(), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_upvalueid_returns_nil_out_of_range_and_supports_gmatch_iterators() {
    // Regression test: real Lua's `debug.upvalueid` returns `nil` for an
    // out-of-range upvalue index instead of erroring, and also works on a
    // `string.gmatch` iterator (a `LuaValue::GMatchIterator`, not a
    // `Closure`), giving it exactly one opaque identity at index 1.
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function make()
                local a = 1
                return function() return a end
            end
            local f = make()
            local iter = string.gmatch("ab cd", "%a+")
            return debug.upvalueid(f, 2) == nil and
                debug.upvalueid(iter, 1) ~= nil and
                debug.upvalueid(iter, 2) == nil
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_upvaluejoin_shares_upvalue_storage_across_closures() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function make_a()
                local x = 1
                return function() return x end, function(v) x = v end
            end
            local function make_b()
                local y = 100
                return function() return y end
            end
            local get_a, set_a = make_a()
            local get_b = make_b()
            debug.upvaluejoin(get_b, 1, get_a, 1)
            set_a(42)
            return get_b() == 42
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_setupvalue_replaces_a_closure_upvalue() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });

    let source = br#"
        local function make()
            local captured = 10
            return function() return captured end
        end
        local f = make()
        local name = debug.setupvalue(f, 1, 42)
        return name == "captured" and f() == 42 and debug.setupvalue(f, 2, 0) == nil
    "#;
    assert_eq!(runtime.run(&parse(source)).unwrap(), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_reports_the_calling_frames_current_line() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local chunk = assert(load("return debug.getinfo(1).currentline"))
            assert(chunk() == 1)

            local chunk2 = assert(load("return require\"debug\".getinfo(1).currentline"))
            assert(chunk2() == 1)

            local f = assert(load("return 'a'\n, debug.getinfo(1).currentline"))
            local s, l = f()
            assert(s == 'a' and l == 2)

            return true
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_names_an_active_local_function() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function named()
                local info = debug.getinfo(1, "n")
                return info.name == "named" and info.namewhat == "local"
            end
            return named()
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_setmetatable_on_numbers_is_honored_by_indexing_and_unpack() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            assert(debug.getmetatable(1) == nil)

            local backing = {10, 20, 30}
            debug.setmetatable(0, { __index = backing })
            assert(debug.getmetatable(1) ~= nil)

            local n = 0
            assert(n[1] == 10 and n[2] == 20 and n[3] == 30)

            local a, b, c = table.unpack(n, 1, 3)
            assert(a == 10 and b == 20 and c == 30)

            debug.setmetatable(0, nil)
            local ok = pcall(function() return n[1] end)
            assert(not ok)

            return true
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_extraargs_counts_call_chain_hops() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/calls.lua`'s "testing chains of '__call'" case (line
    // ~212): `debug.getinfo(1, 't').extraargs` is checked directly. Despite
    // its name, real Lua 5.5's `lua_Debug.extraargs` (`ldebug.c`'s
    // `auxgetinfo`, `'t'` case) is *not* a count of the inspected function's
    // received vararg arguments - it is `(ci->callstatus & MAX_CCMT) >>
    // CIST_CCMT`, the number of `__call` metamethod hops
    // `step_result_for_call`'s `luaD_precall`-style retry loop walked to
    // reach the currently-running closure (verified against the pinned
    // `lua5.5.1` oracle: `f(...)` called directly always reports 0
    // regardless of how many varargs `f` actually received, but reports the
    // exact hop count when called through a chain of `__call` tables).
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function f(...)
                return debug.getinfo(1, 't').extraargs, select('#', ...)
            end
            local t1 = setmetatable({}, {__call = f})
            local t2 = setmetatable({}, {__call = t1})
            local t3 = setmetatable({}, {__call = t2})

            local e0, n0 = f("x", "y")
            local e1, n1 = t1("x", "y")
            local e2, n2 = t2("x", "y")
            local e3, n3 = t3("x", "y")
            -- direct call: no `__call` hop, but two real varargs - these
            -- must not be conflated.
            if not (e0 == 0 and n0 == 2) then return false end
            if not (e1 == 1 and n1 == 3) then return false end
            if not (e2 == 2 and n2 == 4) then return false end
            if not (e3 == 3 and n3 == 5) then return false end
            return true
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_extraargs_is_zero_for_non_vararg_function() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // A non-vararg function called directly (no `__call` chain involved)
    // must report `extraargs == 0`, matching the pinned `lua5.5.1` oracle -
    // `extraargs` tracks `__call`-chain hops, not vararg-ness or parameter
    // count, so a plain fixed-arity function is indistinguishable here from
    // any other direct call.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function g(a, b, c)
                return debug.getinfo(1, 't').extraargs
            end
            return g(1, 2, 3) == 0
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_accepts_a_function_value_and_reports_loads_chunkname() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/calls.lua` line 344:
    // `assert(debug.getinfo(a).source == "modname")`, where `a` is the
    // closure `load(reader, "modname", "t", _G)` just produced. Real Lua's
    // `debug.getinfo([thread,] f [, what])` accepts a function *value* (not
    // just a numeric stack level) as its first argument, and `source` is the
    // raw chunkname given to `load`/`lua_load`, set uniformly across every
    // `Proto` the chunk compiles to.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local a = assert(load("return 1", "modname", "t", _G))
            return debug.getinfo(a).source == "modname"
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_reports_func_for_a_closure_value() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/db.lua` line 39-42: `debug.getinfo(test, "SfL")`
    // (looked up by function *value*, not stack level) must set `func` to
    // the exact closure passed in, so `b.func == test` holds under
    // `LuaValue`'s `Rc::ptr_eq`-based closure equality. This is only wired
    // for the direct-value lookup - the level-based lookup unpacks a
    // `LuaFrame`'s `proto`/`upvals` rather than keeping the original closure
    // `Rc`, so it still leaves `func` unset (see `natives_debug.rs`).
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function test(a, b) return a + b end
            local info = debug.getinfo(test, "SfL")
            return info.func == test
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_builds_activelines_from_the_source_map() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/db.lua` line 43-46: `activelines` is a set (line ->
    // `true`) of lines this prototype's own bytecode maps to. This only
    // checks the set is actually populated from `source_map` and doesn't leak
    // lines from an unrelated scope; `db.lua`'s own stricter boundary checks
    // (excluding the declaration/closing lines exactly, and an empty set for
    // a debug-info-stripped dump) are still blocked by separate, pre-existing
    // gaps - see `tests/lua55/manifest.toml`'s `db.lua` entry.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function test(a, b)
                local sum = a + b
                return sum
            end
            local info = debug.getinfo(test, "L")
            local has_body_line = info.activelines[info.linedefined + 1] == true
            local excludes_unrelated_line = not info.activelines[info.linedefined - 1]
            return has_body_line and excludes_unrelated_line
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_reports_lastlinedefined_as_the_closing_end_line() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/db.lua` lines 41-48, reproduced exactly: `test` spans
    // 10 source lines (declaration through closing `end`), so
    // `lastlinedefined == linedefined + 10` must hold, and `activelines` must
    // include the closing `end` line (the implicit `return` lands there) but
    // exclude the declaration line itself - regression coverage for two
    // compiler line-info bugs: `lastlinedefined` was approximated as the max
    // line in the source map instead of the real closing-`end` line, and the
    // implicit `return`/`CloseSlots` at function exit were attributed to the
    // declaration line instead of the closing `end` line.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function test (s, l, p)     -- line 1
              collectgarbage()                -- line 2
              local function f (event, line)  -- line 3
                assert(event == 'line')       -- line 4
                local l = table.remove(l, 1)  -- line 5
                if p then print(l, line) end  -- line 6
                assert(l == line, "x")        -- line 7
              end                             -- line 8
              debug.sethook(f,"l")            -- line 9
              print(1)                        -- line 10
            end                               -- line 11
            local b = debug.getinfo(test, "SfL")
            local ok = b.lastlinedefined == b.linedefined + 10
              and b.activelines[b.linedefined + 1]
              and b.activelines[b.lastlinedefined]
              and not b.activelines[b.linedefined]
              and not b.activelines[b.lastlinedefined + 1]
            return ok
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_getinfo_omits_activelines_for_native_functions() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `lua-5.5.1-tests/db.lua` line 37-38: `debug.getinfo(print, "L").activelines`
    // must be `nil` for a C/native function - there is no bytecode source map
    // to build a line set from.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(b"return debug.getinfo(print, \"L\").activelines == nil"))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_set_chunk_name_registers_the_top_level_chunks_source() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // The `sol` CLI's own top-level compile (`main.rs`'s `run`) never went
    // through `load`/`loadfile`, so `chunk_sources` had no entry for the
    // running main chunk and `debug.getinfo(1).source`/`.short_src` were
    // `nil`. `LuaRuntime::set_chunk_name`, called before `run`, registers the
    // program's own top-level functions the same way `load` registers a
    // chunk's, using the `@`-prefixed filename convention.
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    runtime.set_chunk_name(b"@script.lua".to_vec());
    let value = runtime
        .run(&parse(
            br#"
            local info = debug.getinfo(1)
            return info.source == "@script.lua" and info.short_src == "script.lua"
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}
