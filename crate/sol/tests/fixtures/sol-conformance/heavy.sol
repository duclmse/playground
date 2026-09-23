-- Upstream heavy.lua drives an unbounded allocation loop expecting a
-- catchable out-of-memory pcall error. Typed Sol traps are fatal, not
-- catchable (docs/spec/execution-and-runtime.md), so a literal port would
-- either loop forever or abort the whole test process. Reinterpreted as a
-- large but bounded allocation-and-compute stress loop, exercising the same
-- "heavy workload" intent without relying on an unbounded/catchable-OOM
-- path that doesn't exist in typed Sol.
function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 300000 do
        local scratch = new_array_i64(8)
        scratch[0] = i
        total = total + (scratch[0] % 7)
        i = i + 1
    end
    return total
end
