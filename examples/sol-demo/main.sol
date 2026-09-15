-- Sol language demo: a runnable tour of typed Sol's implemented language
-- surface (docs/spec/*.md is the normative reference this mirrors). Each
-- `demo_*` function below exercises one feature area in isolation and
-- returns a small checksum contribution; `main` sums them all into a
-- single `f64` so the whole program can be run and verified with one
-- number (`scripts/run-sol-demo.sh` checks it against the expected total).
--
-- Out of scope here: the Lua-compatibility (`.lua`) surface - see
-- `docs/spec/lua-compatibility.md` and `crates/sol/tests/fixtures/lua55/`
-- for that side of the language instead.

import geometry
import shapes.pipeline

-- A `type` alias: names another type, no distinct runtime representation.
type Count = i64

-- A nominal `struct`.
struct Point {
    x: i64,
    y: i64,
}

-- `extern function` declares a symbol resolved from the process (here,
-- real libm symbols already loaded in this process) - only `i64`, `f64`,
-- and `bool` may cross this FFI boundary.
extern function sqrt(x: f64): f64
extern function pow(base: f64, exp: f64): f64

-- `fn` is an exact alias for `function`, including in declarations.
fn add_one(value: i64): i64
    return value + 1
end

-- A function-typed parameter (`fn(i64) -> i64`), for passing/calling
-- function values indirectly.
function apply_twice(callback: fn(i64) -> i64, value: i64): i64
    return callback(callback(value))
end

-- `any`, `is`/`as` narrowing: `value is Type` tests the runtime type and
-- narrows a directly-tested local in the true branch; `value as Type` is a
-- checked cast that traps on mismatch.
function classify_point(value: any): i64
    if value is Point and value.x > 0 then
        return value.x + value.y
    elseif value is Point then
        return 0 - value.x
    else
        return -1
    end
end

function demo_ffi(): f64
    local a: f64 = sqrt(144.0)
    local b: f64 = pow(2.0, 4.0)
    return a + b
end

function demo_module(): f64
    local v: geometry.Vector2 = geometry.Vector2 { y = 4.0, x = 3.0 }
    return geometry.magnitude_squared(v)
end

function demo_struct_and_record(): i64
    -- Fields given out of declaration order; the struct's declared field
    -- order is what's actually laid out.
    local p: Point = Point { y = 5, x = 2 }
    p.x = p.x + 1
    local total: Count = p.x + p.y

    -- A structural record type (`{ field: Type, ... }`) needs no `struct`
    -- declaration; a compatible record literal constructs it, and field
    -- access resolves statically, including through nesting.
    local rec: { x: i64, y: i64 } = { x = 10, y = 20 }
    local nested: { point: { x: i64, y: i64 }, scale: i64 } = {
        scale = 2,
        point = { x = rec.x, y = rec.y },
    }
    nested.point.x = nested.point.x + 1
    total = total + nested.point.x + nested.point.y * nested.scale
    return total
end

function demo_arrays(): f64
    local xs: Array<i64> = new_array_i64(5)
    for i = 0, 4 do
        xs[i] = i * i
    end
    local total: f64 = 0.0
    for i = 0, #xs - 1 do
        total = total + xs[i]
    end

    local ys: Array<f64> = new_array_f64(3)
    ys[0] = 1.0
    ys[1] = 2.0
    ys[2] = 3.0
    total = total + ys[0] + ys[1] + ys[2]
    return total
end

function demo_maps(): i64
    local seed: Map<i64, i64> = { [7] = 9, [8] = 11 }
    local values: Map<i64, i64> = {}
    for i = 0, 9 do
        values[i] = i + 1
    end

    local total: i64 = seed[7] + seed[8]
    -- Generic `for` over a map: allocation-free `pairs` lowering. Traversal
    -- order is implementation-defined, but summing is order-independent.
    for key, value in pairs(values) do
        total = total + key + value
    end
    return total + #values
end

function demo_generic_for_and_map_builtin(): i64
    local items: Array<i64> = { 3, 4, 5 }
    local total: i64 = 0
    -- Generic `for` over an array: allocation-free `ipairs` lowering.
    for index, value in ipairs(items) do
        total = total + index + value
    end

    -- `map(array, fn)`: the built-in generic array-mapping helper.
    local incremented: Array<i64> = map(items, add_one)
    total = total + incremented[0] + incremented[1] + incremented[2]
    return total
end

function demo_closures(): i64
    local base: i64 = 2
    -- A nested function directly capturing an enclosing local. Since it's
    -- only ever called directly (never escapes), the compiler passes the
    -- current captured value as a hidden parameter at each call site - so
    -- reassigning `base` below is observed the next time `sum_to` runs.
    local function sum_to(value: i64): i64
        if value == 0 then
            return base
        end
        return value + sum_to(value - 1)
    end
    local first: i64 = sum_to(3)
    base = 10
    local second: i64 = sum_to(3)

    local total: i64 = 0
    for index = 1, 3 do
        local function add_index(value: i64): i64
            return value + index
        end
        total = total + add_index(0)
    end
    return first + second + total
end

function demo_function_values(): i64
    local callback: fn(i64) -> i64 = add_one
    return apply_twice(callback, 40)
end

function demo_any_narrowing(): i64
    local point: Point = Point { x = 6, y = 7 }
    local boxed: any = point
    local same: Point = boxed as Point
    same.y = 8

    local array: Array<i64> = { 1, 2, 3 }
    local boxed_array: any = array
    local same_array: Array<i64> = boxed_array as Array<i64>
    same_array[0] = 4

    local values: Map<i64, i64> = { [1] = 1 }
    local boxed_map: any = values
    local same_map: Map<i64, i64> = boxed_map as Map<i64, i64>
    same_map[1] = 9

    local callback: fn(i64) -> i64 = add_one
    local boxed_callback: any = callback
    local same_callback: fn(i64) -> i64 = boxed_callback as fn(i64) -> i64

    return classify_point(boxed) + point.y + array[0] + values[1]
        + same_map[1] + same_callback(0)
end

function demo_array_of_structs(): i64
    -- A typed array literal's element type isn't limited to `i64`/`f64`; a
    -- `struct` element works too, as long as each entry is a compatible
    -- literal for the array's declared element type.
    local pts: Array<Point> = { Point { x = 1, y = 2 }, Point { x = 3, y = 4 } }
    return pts[0].x + pts[1].y
end

function demo_map_specializations(): i64
    -- `Map<i64, V>` also supports `f64` and `bool` values, not just `i64`
    -- (the specialization used in `demo_maps`).
    local scores: Map<i64, f64> = { [1] = 1.5, [2] = 2.5 }
    local flags: Map<i64, bool> = { [1] = true, [2] = false }
    local total: i64 = 0
    if scores[1] + scores[2] == 4.0 then
        total = total + 10
    end
    if flags[1] then
        total = total + 1
    end
    if flags[2] then
        total = total + 100
    end
    return total
end

function demo_overflow(): i64
    -- Integer addition wraps on overflow rather than trapping or promoting;
    -- the largest representable `i64` plus one wraps around to the smallest
    -- (negative) representable value.
    local max: i64 = 9223372036854775807
    local wrapped: i64 = max + 1
    local total: i64 = 0
    if wrapped < 0 then
        total = total + 1
    end
    return total
end

function demo_string_closures(): i64
    -- Closures can directly capture a `string` local too, not just `i64`
    -- (the type `demo_closures` exercises).
    local greeting: string = "hi"
    local function shout(name: string): i64
        local message: string = greeting .. " " .. name
        return #message
    end
    return shout("sol")
end

function demo_two_level_modules(): i64
    -- A qualified name can chain through more than one import: `main`
    -- imports `shapes.pipeline`, which itself imports `shapes.base`
    -- (relative to its own directory) - see examples/sol-demo/shapes/.
    local value: shapes.pipeline.Doubled = shapes.pipeline.double_twice(5)
    return value
end

function demo_bitwise_and_more_operators(): i64
    -- Bitwise operators, integer floor division, the `^` power operator,
    -- string ordering comparisons, and `~=` - none of which demo_control_
    -- flow_and_operators below exercises.
    local a: i64 = 6
    local b: i64 = 3
    local total: i64 = (a & b) + (a | b) + (a ~ b) + (a << 1) + (a >> 1) + (~a) + (7 // 2)

    local squared: f64 = 2.0 ^ 3.0
    if squared == 8.0 then
        total = total + 1
    end

    local s1: string = "abc"
    local s2: string = "abd"
    if s1 < s2 then
        total = total + 1
    end
    if s1 <= s2 then
        total = total + 1
    end
    if s2 > s1 then
        total = total + 1
    end
    if s2 >= s1 then
        total = total + 1
    end
    if a ~= b then
        total = total + 1
    end
    return total
end

function demo_control_flow_and_operators(): i64
    local total: i64 = 0

    -- `if`/`elseif`/`else`.
    for n = 0, 5 do
        if n % 2 == 0 then
            total = total + n
        elseif n == 3 then
            total = total + 100
        else
            total = total - 1
        end
    end

    -- `while`.
    local remaining: i64 = 3
    while remaining > 0 do
        total = total + remaining
        remaining = remaining - 1
    end

    -- Numeric `for` with an explicit negative step.
    for n = 5, 1, -2 do
        total = total + n
    end

    -- `and`/`or` short-circuit, `not`, `#` (string length).
    local a: bool = true
    local b: bool = false
    if (a and not b) or false then
        total = total + 1
    end
    local greeting: string = "hello, " .. "sol"
    total = total + #greeting

    -- `do ... end` and `{ ... }` blocks both create a lexical scope;
    -- a block-local `total` here shadows the outer one without touching
    -- it.
    do
        local total: i64 = 999
        total = total + 1
    end
    {
        local total: i64 = 999
        total = total + 1
    }

    return total
end

function main(): f64
    local total: f64 = 0.0
    total = total + demo_ffi()
    total = total + demo_module()
    total = total + demo_struct_and_record()
    total = total + demo_arrays()
    total = total + demo_maps()
    total = total + demo_generic_for_and_map_builtin()
    total = total + demo_closures()
    total = total + demo_function_values()
    total = total + demo_any_narrowing()
    total = total + demo_control_flow_and_operators()
    total = total + demo_array_of_structs()
    total = total + demo_map_specializations()
    total = total + demo_overflow()
    total = total + demo_string_closures()
    total = total + demo_two_level_modules()
    total = total + demo_bitwise_and_more_operators()
    return total
end
