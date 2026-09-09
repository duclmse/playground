-- M7 §21: faster_lua.md's own vectorization showcase shape - elementwise
-- addition over two large f64 arrays. Matches codegen.rs's
-- try_vectorize_elementwise_loop pattern exactly, so this is the direct
-- before/after measurement for that optimization (see benchmarks/RESULTS.md).
function main(): f64
    local n = 20000000
    local a = new_array_f64(n)
    local b = new_array_f64(n)
    local c = new_array_f64(n)
    for i = 0, n - 1 do
        a[i] = i + 1
        b[i] = i + 2
    end
    for i = 0, n - 1 do
        c[i] = a[i] + b[i]
    end
    local sum = 0.0
    for i = 0, n - 1 do
        sum = sum + c[i]
    end
    return sum
end
