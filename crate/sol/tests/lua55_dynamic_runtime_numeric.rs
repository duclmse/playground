//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: numeric/math semantics).

#[test]
fn dynamic_lua_runtime_matches_lua55_math_and_utf8_extensions() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local integral, fraction = math.modf(-2.5)
        local mantissa, exponent = math.frexp(10)
        math.randomseed(1007)
        local integer_random = math.random(0)
        math.randomseed(1007, 0)
        local float_random = math.random()

        local text = utf8.char(0x41, 0x20ac, 0x1f600)
        local positions, points = 0, 0
        for position, point in utf8.codes(text) do
            positions = positions + position
            points = points + point
        end
        local bad, bad_at = utf8.len("a\xff")

        return integral == -2.0 and fraction == -0.5 and
            mantissa == 0.625 and exponent == 4 and math.ldexp(mantissa, exponent) == 10 and
            math.fmod(-10, 3) == -1 and math.ult(math.maxinteger, math.mininteger) and
            math.atan(1, 0) == math.pi / 2 and math.deg(math.pi) == 180 and
            integer_random == 0x7a7040a5a323c9d6 and
            float_random == 0x0.7a7040a5a323c9d6 and
            utf8.offset(text, 2) == 2 and utf8.offset(text, -1) == 5 and
            positions == 8 and points == 0x41 + 0x20ac + 0x1f600 and
            bad == nil and bad_at == 2
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_numeric_for_loop_supports_float_control_values() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local count = 0
        for i = 1, math.huge do
            count = count + 1
            if i >= 3 then break end
        end

        local product = 1.0
        for i = 1.0, 3.0 do product = product * i end

        local descending = {}
        for i = 5, 1, -2 do descending[#descending + 1] = i end

        return count == 3 and product == 6.0 and #descending == 3 and
            descending[1] == 5 and descending[2] == 3 and descending[3] == 1
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_numeric_for_loop_fixes_a_float_limit_into_an_integer_loop() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ascending_count, ascending_all_int = 0, true
        for i = 1, 10.9 do
            ascending_count = ascending_count + 1
            if math.type(i) ~= "integer" then ascending_all_int = false end
        end

        local descending_count, descending_all_int = 0, true
        for i = 10, 0.001, -1 do
            descending_count = descending_count + 1
            if math.type(i) ~= "integer" then descending_all_int = false end
        end

        local m = math.maxinteger
        local overflow_count = 0
        for i = m, m - 10, -1 do overflow_count = overflow_count + 1 end

        return ascending_count == 10 and ascending_all_int and
            descending_count == 10 and descending_all_int and
            overflow_count == 11
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_hex_float_literals_with_thousands_of_digits_do_not_overflow() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        return tonumber("0xe03" .. string.rep("0", 1000) .. "p-4000") == 3587.0
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_float_modulo_stays_precise_for_large_magnitudes() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        return 2.0^54 % 3 == 1.0 and -(2.0^54) % 3 == 2.0 and 2.0^54 % -3 == -2.0
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_math_tointeger_coerces_numeral_strings() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local minint, maxint = math.mininteger, math.maxinteger
        return math.tointeger(minint .. "") == minint
            and math.tointeger(maxint .. "") == maxint
            and math.tointeger("34.0") == 34
            and math.tointeger("34.3") == nil
            and math.tointeger("not a number") == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_float_to_integer_conversion_rejects_two_to_the_63() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local minint = math.mininteger
        local two_to_63 = 0.0 - minint
        local t = {}
        t[two_to_63] = "float key"
        return math.tointeger(two_to_63) == nil
            and math.type(two_to_63) == "float"
            and t[two_to_63] == "float key"
            and rawequal(next(t), two_to_63)
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_math_max_min_compare_large_integers_exactly() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local minint, maxint = math.mininteger, math.maxinteger
        local hi = math.max(minint, minint + 1)
        local lo = math.min(maxint, maxint - 1)
        return hi == minint + 1 and math.type(hi) == "integer"
            and lo == maxint - 1 and math.type(lo) == "integer"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
