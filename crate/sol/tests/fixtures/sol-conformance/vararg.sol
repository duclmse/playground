-- Upstream vararg.lua exercises Lua's `...` varargs and Lua 5.5's named-
-- vararg packing (`function f(...t)`). Typed Sol has no variadic
-- functions - every typed function has a fixed, statically declared
-- parameter list (docs/spec/functions-and-modules.md). Reinterpreted using
-- an explicit Array<i64> as the "variable argument list" a caller
-- assembles instead, the closest typed analogue to a packed vararg table.
function sum_args(args: Array<i64>): i64
    local total: i64 = 0
    for index, value in ipairs(args) do
        total = total + value
    end
    return total
end

function count_args(args: Array<i64>): i64
    return #args
end

function main(): i64
    local args: Array<i64> = { 10, 20, 30, 40 }
    return sum_args(args) + count_args(args)
end
