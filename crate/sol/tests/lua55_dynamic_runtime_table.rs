//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: the table library).

#[test]
fn dynamic_lua_runtime_provides_core_table_and_math_library_slices() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local values = table.pack(20, 21)
        table.insert(values, 2, 1)
        local removed = table.remove(values, 3)
        local first, second = table.unpack(values)
        return values.n == 2 and table.concat(values, ":") == "20:1" and
            removed == 21 and first == 20 and second == 1 and
            math.abs(-42) == 42 and math.floor(2.9) == 2 and math.ceil(2.1) == 3 and
            math.min(9, 4, 7) == 4 and math.max(9, 4, 7) == 9 and
            math.tointeger(4.0) == 4 and math.tointeger(4.5) == nil and
            math.type(4) == "integer" and math.type(4.5) == "float" and
            utf8.len("h\u{e9}\u{1f600}") == 3 and utf8.char(0x41, 0x1f600) == "A\u{1f600}" and
            utf8.codepoint("A\u{e9}") == 0x41 and
            utf8.charpattern == "[\0-\x7f\xc2-\xfd][\x80-\xbf]*"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_table_sort_comparator_chains_without_native_stack_overflow(
) {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `comparator` itself performs another `table.sort(inner, comparator)`
    // call (on a fresh 2-element table, so each level's sort makes exactly
    // one comparison) before returning its own decision, chained 50,000
    // levels deep via the shared `counter` table (a plain local/upvalue
    // reassignment would only ever shadow itself in a fresh scope, same
    // reasoning as the `dofile` chain test above). Each level dispatches
    // through `NativeCont::Sort`/`sort_step`, so this must not grow the
    // native Rust call stack even though every level's comparator is itself
    // a full nested `table.sort` invocation - unlike plain recursive Lua
    // calls inside a comparator, which the Stage-1 closure trampoline
    // already made safe before this stage.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    let source = br#"
        counter = { n = 50000 }
        local function comparator(a, b)
            counter.n = counter.n - 1
            if counter.n > 0 then
                local inner = { 2, 1 }
                table.sort(inner, comparator)
            end
            return a < b
        end
        local values = { 2, 1 }
        table.sort(values, comparator)
        return counter.n
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(0));
}

#[test]
fn dynamic_lua_runtime_implements_lua_pattern_matching_format_and_table_create() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local find_start, find_end = string.find("hello world", "o w")
        local plain_start = string.find("a.b.c", ".", 1, true)
        local key, value = string.match("  key: 42", "(%a+):%s*(%d+)")

        local words = {}
        for w in string.gmatch("one two  three", "%a+") do
            words[#words + 1] = w
        end

        local replaced, count = string.gsub("hello world", "o", "0")
        local upper, upper_count = string.gsub("hello", "%a", string.upper)
        local templated = string.gsub("2024-01-02", "(%d+)-(%d+)-(%d+)", "%3/%2/%1")
        local mapped = string.gsub("$name", "%$(%a+)", { name = "World" })
        local fallback = setmetatable({}, {
            __index = function(_, key) return string.upper(key) end,
        })
        local mapped_fallback = string.gsub("a alo b hi", "%w%w+", fallback)
        local identity_source = string.rep("a", 20)
        local no_match = string.gsub(identity_source, "b", "c")
        local no_value = string.gsub(identity_source, ".", {})
        local replaced_same = string.gsub(identity_source, ".", function(value) return value end)

        local formatted = string.format("%d-%05.2f-%s-%x", 42, 3.14159, "hi", 255)

        -- `table.create(sizeseq, sizerest)`'s arguments are preallocation
        -- size *hints*, not a fill value or element count - a freshly
        -- created table is still empty until explicitly assigned into.
        local created = table.create(3, 0)
        created[1], created[2], created[3] = 10, 20, 30
        local gc_count = collectgarbage("count")

        return find_start == 5 and find_end == 7 and
            plain_start == 2 and
            key == "key" and value == "42" and
            #words == 3 and words[1] == "one" and words[2] == "two" and words[3] == "three" and
            replaced == "hell0 w0rld" and count == 2 and
            upper == "HELLO" and upper_count == 5 and
            templated == "02/01/2024" and
            mapped == "World" and
            mapped_fallback == "a ALO b HI" and
            string.format("%p", identity_source) == string.format("%p", no_match) and
            string.format("%p", identity_source) == string.format("%p", no_value) and
            replaced_same == identity_source and
            string.format("%p", identity_source) == string.format("%p", replaced_same) and
            string.format("%p", 4) == "(null)" and
            formatted == "42-03.14-hi-ff" and
            #created == 3 and created[1] == 10 and created[2] == 20 and created[3] == 30 and
            type(gc_count) == "number" and gc_count >= 0
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_insert_remove_see_hash_resident_integer_keys() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local a = {[0] = "ban"}
        local len_before_remove = #a
        local removed = table.remove(a)
        local cleared = a[0] == nil

        table.insert(a, 1, 10)
        table.insert(a, 1, 20)
        table.insert(a, 1, -1)
        local r1 = table.remove(a)
        local r2 = table.remove(a)
        local r3 = table.remove(a)
        local r4 = table.remove(a)

        local b = {[-7] = "kept"}
        table.insert(b, 1, "x")
        local shifted_negative_key_untouched = b[-7] == "kept" and b[1] == "x"

        return len_before_remove == 0 and removed == "ban" and cleared and
            r1 == 10 and r2 == 20 and r3 == -1 and r4 == nil and
            shifted_negative_key_untouched
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_library_respects_proxy_metamethods_including_sort() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = {}
        local proxy = setmetatable({}, {
            __len = function() return #t end,
            __index = t,
            __newindex = t,
        })
        for i = 1, 10 do
            table.insert(proxy, 1, i)
        end
        local inserted_ok = #proxy == 10 and #t == 10
        for i = 1, 10 do
            if t[i] ~= 11 - i then inserted_ok = false end
        end

        table.sort(proxy)
        local sorted_ok = true
        for i = 1, 10 do
            if t[i] ~= i or proxy[i] ~= i then sorted_ok = false end
        end

        local concat_ok = table.concat(proxy, ",") == "1,2,3,4,5,6,7,8,9,10"

        local removed_ok = true
        for i = 1, 8 do
            if table.remove(proxy, 1) ~= i then removed_ok = false end
        end
        removed_ok = removed_ok and #proxy == 2 and #t == 2

        local a, b, c = table.unpack(proxy)
        local unpack_ok = a == 9 and b == 10 and c == nil

        return inserted_ok and sorted_ok and concat_ok and removed_ok and unpack_ok
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_insert_end_position_wraps_around_on_overflow() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = setmetatable({}, {__len = function() return math.maxinteger end})
        table.insert(t, 20)
        local k, v = next(t)
        return k == math.mininteger and v == 20
    "#;
    let run = run_source(source).unwrap();
    assert_eq!(run.value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_read_of_a_nil_or_unhashable_key_returns_nil_but_a_write_still_errors()
{
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = {}
        assert(t[nil] == nil)
        assert(t[0/0] == nil)

        local ok, err = pcall(function() t[nil] = 5 end)
        assert(not ok and string.find(err, "table index is nil"))

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_create_validates_its_size_hints() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = table.create(4, 8)
        local starts_empty = #t == 0 and t[1] == nil
        for i = 1, 4 do t[i] = i * i end
        local filled_ok = true
        for i = 1, 4 do if t[i] ~= i * i then filled_ok = false end end
        return starts_empty and filled_ok
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));

    let negative = run_source(b"table.create(-1)").unwrap_err();
    assert!(
        negative
            .message
            .contains("bad argument #1 to 'create' (out of range)"),
        "{}",
        negative.message
    );

    let huge_rest = run_source(b"table.create(0, 2147483647)").unwrap_err();
    assert!(
        huge_rest.message.contains("table overflow"),
        "{}",
        huge_rest.message
    );
}

#[test]
fn dynamic_lua_runtime_table_insert_rejects_the_wrong_argument_count() {
    use sol::lua_runtime::run_source;

    let too_few = run_source(b"local t = {1, 2, 3}; table.insert(t)").unwrap_err();
    assert_eq!(too_few.message, "wrong number of arguments to 'insert'");

    let too_many = run_source(b"local t = {1, 2, 3}; table.insert(t, 1, 2, 3)").unwrap_err();
    assert_eq!(too_many.message, "wrong number of arguments to 'insert'");
}

#[test]
fn dynamic_lua_runtime_table_unpack_rejects_an_absurdly_large_range() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ok, err = pcall(table.unpack, {}, 1, math.maxinteger)
        return not ok and err:find("too many results to unpack") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_move_copies_ranges_including_overlap_and_across_tables() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local a = {1, 2, 3, 4, 5}
        local b = {}
        table.move(a, 2, 4, 1, b)
        local separate_ok = b[1] == 2 and b[2] == 3 and b[3] == 4 and b[4] == nil

        -- overlap, shifting right: must copy backward (high-to-low)
        local c = {1, 2, 3, 4, 5}
        table.move(c, 1, 3, 2)
        local overlap_right_ok = c[1] == 1 and c[2] == 1 and c[3] == 2
            and c[4] == 3 and c[5] == 5

        -- overlap, shifting left: must copy forward (low-to-high)
        local d = {1, 2, 3, 4, 5}
        table.move(d, 2, 5, 1)
        local overlap_left_ok = d[1] == 2 and d[2] == 3 and d[3] == 4
            and d[4] == 5 and d[5] == 5

        local e = {1, 2, 3}
        local implicit_dest_ok = table.move(e, 1, 3, 1) == e

        local f, g = {10, 20}, {}
        local return_value_ok = table.move(f, 1, 2, 1, g) == g

        return separate_ok and overlap_right_ok and overlap_left_ok
            and implicit_dest_ok and return_value_ok
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_move_respects_proxy_index_and_newindex_metamethods() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local backing_src = {10, 20, 30}
        local src_proxy = setmetatable({}, { __index = backing_src })

        local backing_dst = {}
        local dst_proxy = setmetatable({}, {
            __index = backing_dst,
            __newindex = backing_dst,
        })

        table.move(src_proxy, 1, 3, 1, dst_proxy)
        return backing_dst[1] == 10 and backing_dst[2] == 20 and backing_dst[3] == 30
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));

    use sol::lua_runtime::{Capabilities, LuaRuntime};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local backing = {1, 2, 3}
            debug.setmetatable(0, { __index = backing })
            local dest = {}
            table.move(0, 1, 3, 1, dest)
            debug.setmetatable(0, nil)
            return dest[1] == 1 and dest[2] == 2 and dest[3] == 3
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_move_validates_arguments_and_propagates_error_identity() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local ok1, err1 = pcall(table.move, 5, 1, 1, 1)
        assert(not ok1 and err1:find("table expected, got number") ~= nil)

        local ok2, err2 = pcall(table.move, {}, 1, 1, 1, "x")
        assert(not ok2 and err2:find("table expected, got string") ~= nil)

        local ok3, err3 = pcall(table.move, {}, 0, math.maxinteger, 1)
        assert(not ok3 and err3:find("too many elements to move") ~= nil)

        local ok4, err4 = pcall(table.move, {}, 1, 2, math.maxinteger)
        assert(not ok4 and err4:find("destination wrap around") ~= nil)

        local marker = {}
        local proxy = setmetatable({}, { __index = function() error(marker) end })
        local ok5, err5 = pcall(table.move, proxy, 1, 1, 1, {})
        assert(not ok5 and err5 == marker)

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_sort_rejects_an_oversized_array_without_a_native_panic() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local huge = setmetatable({}, { __len = function() return math.maxinteger end })
        local ok, err = pcall(table.sort, huge)
        return not ok and err:find("array too big") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_sort_detects_an_invalid_order_function_and_leaves_the_table_untouched()
{
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = {}
        for i = 1, 20 do t[i] = i end
        local always_less = function(a, b) return true end

        local ok, err = pcall(table.sort, t, always_less)

        local untouched = true
        for i = 1, 20 do
            if t[i] ~= i then untouched = false end
        end

        return not ok and err:find("invalid order function for sorting") ~= nil and untouched
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_constructor_with_many_fields_does_not_overflow_registers() {
    use sol::lua_runtime::{run_source, LuaValue};

    let mut source = String::from("local t = {");
    for i in 0..70_000 {
        if i > 0 {
            source.push(',');
        }
        source.push_str(&i.to_string());
    }
    source.push_str("}\nreturn #t == 70000 and t[1] == 0 and t[70000] == 69999");

    assert_eq!(
        run_source(source.as_bytes()).unwrap().value,
        LuaValue::Bool(true)
    );
}

#[test]
fn dynamic_lua_runtime_table_length_handles_holes_and_stays_fast_for_appends() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local t = {}
        for i = 1, 50000 do t[#t + 1] = i end
        assert(#t == 50000)

        local with_trailing_hole = {1, 2, 3}
        with_trailing_hole[3] = nil
        assert(#with_trailing_hole == 2)

        assert(#{} == 0)
        assert(#{1} == 1)

        return true
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_a_table_constructor_statement_does_not_absorb_a_following_paren_call() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua's "ambiguous syntax" continuation rule (adding `()`/`[]`/
    // `.`/`:` suffixes to a statement that spans onto the next line) only
    // applies to a genuine `prefixexp` root (a bare name, or a parenthesized
    // expression) - never to a table constructor or function literal, which
    // are not `prefixexp` at all. `local t = {}` followed on the next line
    // by `(function (a) ... end)(1)` must therefore parse as two separate
    // statements, not as `{}` being called.
    let source = br#"
        local t = {}
        local called = 0
        (function (a) called = a end)(7)
        return called
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Integer(7));
}

#[test]
fn dynamic_lua_runtime_table_unpack_rejects_a_lua_stack_sized_result_list() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Lua checks its one-million-slot result-stack limit before fetching
    // table entries, so this must fail even though every requested slot is
    // nil. This is exercised by the unchanged coroutine.lua corpus case.
    let source = br#"
        local ok, err = pcall(table.unpack, {}, 1, 1000000)
        return not ok and string.find(err, "too many results") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
