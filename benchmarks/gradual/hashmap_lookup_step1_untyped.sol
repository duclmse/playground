-- Gradual-typing step 1/4 for hashmap_lookup (see
-- benchmarks/gradual-manifest.json): same logic as ../hashmap_lookup.lua
-- and ../hashmap_lookup.sol, 0% annotation coverage. `local values = {}`
-- with no annotation is legal dynamic-Lua table syntax, so this runs
-- through the dynamic, budget-gated interpreter like the .lua version.
function main()
    local values = {}
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
