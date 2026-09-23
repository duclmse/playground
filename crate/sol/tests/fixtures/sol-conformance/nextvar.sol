-- Upstream nextvar.lua needs math.random (missing) and exercises Lua's
-- `next`-based table traversal and global-variable semantics. Typed Sol has
-- no globals and no `next` - only `Map<i64, V>` with `pairs`
-- (docs/spec/types-and-values.md). Reinterpreted as a
-- Map<i64, i64> insert/lookup/iteration exercise.
function main(): i64
    local values: Map<i64, i64> = {}
    for i = 0, 999 do
        values[i] = i * i
    end

    local total: i64 = 0
    for key, value in pairs(values) do
        total = total + value
    end

    return total + #values + values[10]
end
