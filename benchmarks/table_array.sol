-- sol equivalent of table_array.lua - same problem size (4,000,000
-- elements), for a direct comparison of a typed `Array<f64>` (contiguous,
-- unboxed, no per-element type checks - see faster_lua.md's showcase
-- example and docs/sol.md) against reference Lua/LuaJIT/this repo's
-- dynamic VM's general-purpose table.
function main(): f64
    local n = 4000000
    local a = new_array_f64(n)
    for i = 0, n - 1 do
        local v = i + 1
        a[i] = v * v
    end
    local sum = 0.0
    for i = 0, n - 1 do
        sum = sum + a[i]
    end
    return sum
end
