-- Gradual-typing step 4/4 for function_calls: a genuine superset of
-- ../function_calls.sol's annotations (which types only `work`'s signature
-- and leaves every local to inference) - every local in both functions is
-- now explicitly annotated too. Measures whether writing out what
-- typeck/inference.rs already infers for free changes anything.
function work(x: i64): i64
    local a1: i64 = x + 1
    local a2: i64 = a1 + 1
    local a3: i64 = a2 + 1
    local a4: i64 = a3 + 1
    local a5: i64 = a4 + 1
    local a6: i64 = a5 + 1
    local a7: i64 = a6 + 1
    local a8: i64 = a7 + 1
    local a9: i64 = a8 + 1
    local a10: i64 = a9 + 1
    local a11: i64 = a10 + 1
    local a12: i64 = a11 + 1
    local a13: i64 = a12 + 1
    local a14: i64 = a13 + 1
    local a15: i64 = a14 + 1
    local a16: i64 = a15 + 1
    local a17: i64 = a16 + 1
    local a18: i64 = a17 + 1
    local a19: i64 = a18 + 1
    local a20: i64 = a19 + 1
    local a21: i64 = a20 + 1
    return a21
end

function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 10000000 do
        total = total + work(i)
        i = i + 1
    end
    return total
end
