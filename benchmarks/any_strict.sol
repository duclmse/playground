-- Gradual typing: the strict-mode baseline for measuring
-- gradual typing's actual cost - see any_dynamic.sol for the identical
-- workload with an `any`-typed function boundary in the hot path.
function compute(x: i64): i64
    return x + 1
end

function main(): i64
    local total = 0
    for i = 0, 4999999 do
        total = total + compute(i)
    end
    return total
end
