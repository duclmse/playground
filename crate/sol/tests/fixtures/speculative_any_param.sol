-- Tiered execution: `double`'s `any` parameter is immediately
-- narrowed to `i64` via a single `Unbox` and never used any other way -
-- exactly `jit::speculative_candidate`'s eligible shape. Called many times
-- with the same (i64) tag from `main`'s loop so it crosses
-- `SOL_SPECULATIVE_THRESHOLD` and gets compiled as a guarded native
-- specialization partway through the run - both the pre- and
-- post-specialization calls must agree on the answer.
function double(x: any): i64
    local y: i64 = x
    return y * 2
end

function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 60 do
        local boxed: any = i
        total = total + double(boxed)
        i = i + 1
    end
    return total
end
