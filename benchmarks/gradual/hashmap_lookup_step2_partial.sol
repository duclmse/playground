-- Gradual-typing step 2/4 for hashmap_lookup: a genuine superset of step 1's
-- annotations - adds only `values`'s `Map<i64, i64>` declaration, `total`
-- and `main`'s return type stay untyped/inferred. This is the *required*
-- first annotation for this program: annotating `main`'s return type alone
-- without this one is a hard compile error ([EDYNLUA] "dynamic Lua
-- table/function syntax is parsed, but requires the M13 dynamic runtime"),
-- because a bare `{}` literal is only legal under the fully-dynamic parse.
function main()
    local values: Map<i64, i64> = {}
    for key = 0, 999 do
        values[key] = key + 1
    end

    local total = 0
    for round = 0, 99 do
        for key = 0, 999 do
            total = total + values[key]
        end
    end
    return total
end
