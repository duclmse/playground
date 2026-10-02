-- Gradual-typing step 2/4 for loop_sum: a genuine superset of step 1's
-- annotations - adds only `main`'s return-type annotation, `sum` stays
-- untyped/inferred. One annotation is enough to seed typeck's whole-file
-- flow inference (typeck/inference.rs), which here proves `sum: i64` on its
-- own, so this step already compiles through the native tier like ../loop_sum.sol.
function main(): i64
    local sum = 0
    for i = 1, 20000000 do
        sum = sum + i
    end
    return sum
end
