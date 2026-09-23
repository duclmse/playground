//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: the string library and pattern matching).

#[test]
fn dynamic_lua_runtime_preserves_non_string_error_values() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ok1, err1 = pcall(function() error({ code = 1 }) end)
        local ok2, err2 = pcall(function() error(42) end)
        local ok3, err3 = pcall(function() assert(false, { msg = "boom" }) end)
        local co = coroutine.create(function() error({ x = 5 }) end)
        local ok4, err4 = coroutine.resume(co)
        return not ok1 and type(err1) == "table" and err1.code == 1 and
            not ok2 and err2 == 42 and
            not ok3 and type(err3) == "table" and err3.msg == "boom" and
            not ok4 and type(err4) == "table" and err4.x == 5
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_exposes_a_shared_mutable_string_metatable() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local mt = getmetatable("")
        local same = mt == getmetatable("anything")
        local routes_through_index = mt.__index == string
        mt.__band = function(a, b) return "banded" end
        return same and routes_through_index and ("x" & "y") == "banded" and ("hi"):upper() == "HI"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_gsub_does_not_repeat_empty_match_after_nonempty_match() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local direct, direct_count = string.gsub("a b cd", " *", "-")
        local called, called_count = string.gsub("a b cd", " *", function() return "-" end)
        return direct == "-a-b-c-d-" and direct_count == 5 and
            called == direct and called_count == direct_count
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_require_rejects_non_string_package_path() {
    use sol::lua_runtime::run_source;

    // `lua-5.5.1-tests/attrib.lua` lines 42-48: reassigning `package.path`
    // to a non-string must make `require` fail with an error naming
    // `package.path`, not silently ignore the bogus value or panic.
    let source = br#"
        package.path = {}
        local ok, msg = pcall(require, 'no-such-file')
        assert(ok == false)
        assert(string.find(msg, "package.path", 1, true) ~= nil, msg)
        return true
    "#;
    run_source(source).unwrap();
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_gsub_function_replacement_chains_without_native_stack_overflow(
) {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `replacer` itself performs another `string.gsub("x", "x", replacer)`
    // call (a single-character source/pattern, so each level's scan finds
    // exactly one match) before returning its own replacement, chained
    // 50,000 levels deep via the shared `counter` table. Each level
    // dispatches through `NativeCont::Gsub`/`gsub_step`, so this must not
    // grow the native Rust call stack even though every level's replacement
    // is itself a full nested `string.gsub` invocation.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        counter = { n = 50000 }
        local function replacer(text)
            counter.n = counter.n - 1
            if counter.n > 0 then
                string.gsub("x", "x", replacer)
            end
            return text
        end
        string.gsub("x", "x", replacer)
        return counter.n
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(0));
}

#[test]
fn dynamic_lua_runtime_gsub_function_replacement_respects_max_limit() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A Lua-closure replacement (as opposed to the `string.upper` native-
    // function replacement covered elsewhere) over a source with more
    // matches than the `max` cap allows, confirming the replacement
    // function is called exactly `max` times and the untouched tail is
    // still appended verbatim.
    let source = br#"
        local calls = 0
        local replaced, count = string.gsub("aaaa", "a", function(x)
            calls = calls + 1
            return x .. x
        end, 2)
        return replaced == "aaaaaa" and count == 2 and calls == 2
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_implements_string_pack_unpack_and_packsize() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local packed = string.pack("<i4i4", -1, 42)
        local a, b, pos = string.unpack("<i4i4", packed)
        local sized = string.pack(">I2s1", 7, "hi")
        local n, s = string.unpack(">I2s1", sized)
        return string.packsize("i4i4") == 8 and
            a == -1 and b == 42 and pos == 9 and
            n == 7 and s == "hi" and
            string.pack("z", "abc") == "abc\0" and
            select(2, string.unpack("z", "xyz\0")) == 5
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_expands_portable_string_table_and_math_libraries() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ascending = { 3, 1, 2 }
        table.sort(ascending)
        local descending = { 1, 3, 2 }
        table.sort(descending, function(left, right) return left > right end)
        local bytes = string.char(65, 0, 255)
        local first, second, third = string.byte(bytes, 1, -1)
        return ascending[1] == 1 and ascending[2] == 2 and ascending[3] == 3 and
            descending[1] == 3 and descending[2] == 2 and descending[3] == 1 and
            first == 65 and second == 0 and third == 255 and
            string.sub("abc", 100) == "" and string.sub("abc", -2) == "bc" and
            tonumber("42") == 42 and tonumber("2a", 16) == 42 and
            math.sqrt(81) == 9 and math.sin(0) == 0 and math.cos(0) == 1 and
            math.log(8, 2) == 3 and math.maxinteger > 0 and math.mininteger < 0 and
            math.huge > math.maxinteger and math.pi > 3
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_handles_gsub_limits_anchors_and_numeric_format_specifiers() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local limited, limited_count = string.gsub("aaaa", "a", "b", 2)
        local anchored, anchored_count = string.gsub("aaa", "^a", "X")
        local no_captures_backref = string.gsub("hi", "h", "[%1]")

        local negative_int = string.format("%d", -42)
        local plus_int = string.format("%+d", 7)
        local hex_upper = string.format("%#X", 255)
        local scientific = string.format("%e", 12345.6789)
        local general_small = string.format("%g", 100000)
        local general_large = string.format("%g", 1000000)
        local quoted = string.format("%q", "line\nwith \"quotes\"")

        return limited == "bbaa" and limited_count == 2 and
            anchored == "Xaa" and anchored_count == 1 and
            no_captures_backref == "[h]i" and
            negative_int == "-42" and
            plus_int == "+7" and
            hex_upper == "0XFF" and
            scientific == "1.234568e+04" and
            general_small == "100000" and
            general_large == "1e+06" and
            quoted == "\"line\\\nwith \\\"quotes\\\"\""
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_string_rep_rejects_oversized_count() {
    use sol::lua_runtime::run_source;

    let huge_count = br#"
        return string.rep("aa", math.maxinteger)
    "#;
    let error = run_source(huge_count).unwrap_err();
    assert!(
        error.message.contains("too large"),
        "unexpected error message: {}",
        error.message
    );

    let huge_count_with_separator = br#"
        return string.rep("aa", math.maxinteger // 2 + 10, ",")
    "#;
    let error = run_source(huge_count_with_separator).unwrap_err();
    assert!(
        error.message.contains("too large"),
        "unexpected error message: {}",
        error.message
    );
}

#[test]
fn dynamic_lua_runtime_string_rep_handles_ordinary_and_edge_counts() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        return string.rep("ab", 3) == "ababab" and
            string.rep("ab", 3, "-") == "ab-ab-ab" and
            string.rep("x", 0) == "" and
            string.rep("x", -5) == "" and
            string.rep("x", 1) == "x"
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_number_and_string_coercion_errors_use_luas_own_wording() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ok1, err1 = pcall(math.floor, {})
        local ok2, err2 = pcall(string.rep, {}, 1)
        return not ok1 and err1:find("number expected") ~= nil
            and not ok2 and err2:find("string expected") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_float_tostring_round_trips_and_marks_whole_numbers_as_floats() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        assert(tostring(100.0) == "100.0")
        assert(tostring(0.0) == "0.0")
        assert(tostring(-0.0) == "-0.0")
        assert(tostring(1/0) == "inf")
        assert(tostring(-1/0) == "-inf")
        assert(tostring(0/0) == "nan")
        assert(tostring(0.001) == "0.001")
        assert(tostring(0.00001):find("e-05") ~= nil)
        assert(tostring(1e15):find("e%+15") ~= nil)
        assert(tostring(1234567890123456.0) == "1234567890123456.0")

        -- every distinct double near a power-of-two boundary must both
        -- print differently from its neighbor and round-trip exactly.
        for _, i in ipairs{56, 57, 58, 62} do
            local x = 2.0^i
            local y = x + 2.0^(i - 52)
            assert(x ~= y)
            assert(tostring(x) ~= tostring(y))
            assert(tonumber(tostring(x)) == x)
            assert(tonumber(tostring(y)) == y)
        end

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_string_dump_round_trips_through_load_in_binary_mode() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua` line 361:
    // `f = load(string.dump(function () return 1 end), nil, "b", {})`. Sol
    // has no portable bytecode-file format, so `string.dump` returns an
    // opaque same-process handle (see `dumped_protos`'s doc comment in
    // `lua_runtime/mod.rs`) rather than real serialized bytecode - but that
    // handle must still round-trip through `load(..., "b")` into a callable,
    // equivalent closure, and must be rejected by `string.dump` for a native
    // (non-Lua) function.
    let source = br#"
        local dumped = string.dump(function () return 1 end)
        local f = assert(load(dumped, nil, "b", {}))
        local roundtrips = type(f) == "function" and f() == 1
        local cannot_dump_native = not pcall(string.dump, print)
        return roundtrips and cannot_dump_native
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
