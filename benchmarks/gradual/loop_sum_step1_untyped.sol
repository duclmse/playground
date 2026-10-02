-- Gradual-typing step 1/4 for loop_sum (see benchmarks/gradual-manifest.json):
-- same logic as ../loop_sum.lua and ../loop_sum.sol, 0% annotation coverage.
-- No explicit type anywhere in this file falls back to the dynamic,
-- budget-gated interpreter (lua_runtime.rs) exactly like a .lua file - the
-- .sol extension alone buys nothing without at least one annotation.
function main()
    local sum = 0
    for i = 1, 20000000 do
        sum = sum + i
    end
    return sum
end
