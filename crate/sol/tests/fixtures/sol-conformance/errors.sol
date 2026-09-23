-- Upstream errors.lua needs `require "debug"` and exercises Lua's
-- catchable pcall/error error model. Typed Sol has no pcall: array bounds,
-- division/remainder by zero, and checked-cast failures are all fatal
-- traps, not catchable values (docs/spec/execution-and-runtime.md's "Errors
-- and traps"). That trap behavior is already directly regression-tested in
-- crates/sol/tests/programs.rs (out_of_bounds.sol, division_by_zero.sol,
-- negative-index and any_type_mismatch_traps.sol all assert a real crash,
-- not a returnable value). This file instead exercises the valid,
-- non-trapping side of those same boundaries: a last-valid-index read, a
-- zero-divisor guarded before it reaches the trapping operator, and a
-- successful `is`/`as` narrowing round-trip through `any`.
function safe_floor_div(a: i64, b: i64): i64
    if b == 0 then
        return 0
    end
    return a // b
end

function main(): i64
    local arr = new_array_i64(10)
    arr[9] = 42  -- last valid index; index 10 would trap (tested elsewhere)
    local last: i64 = arr[9]

    local quotient: i64 = safe_floor_div(20, 4)
    local guarded: i64 = safe_floor_div(5, 0)

    local boxed: any = 7
    local recovered: i64 = 0
    if boxed is i64 then
        recovered = boxed  -- narrowed to i64 in this branch; no `as` needed
    end

    return last + quotient + guarded + recovered
end
