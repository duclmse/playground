-- Memory management: regression fixture for a real bug found
-- via benchmarking (see benchmarks/RESULTS.md's M4 section) - allocating
-- a large array's data buffer used to get conservatively traced word by
-- word (all 1,000,000 words here) whenever it was discovered as a stack
-- root during a collection triggered by the very next allocation (the
-- array's own small header). `sol_gc_alloc_atomic` (gc.rs) fixes this
-- by marking a scalar array's data buffer pointer-free, so the tracer
-- never reads its contents - tests/programs.rs's
-- allocating_a_large_array_does_not_conservatively_scan_its_contents
-- checks this via SOL_GC_DEBUG's words_scanned counter, not just that
-- the answer is correct.
function main(): f64
    local n = 1000000
    local a = new_array_f64(n)
    for i = 0, n - 1 do
        a[i] = i
    end
    return a[0] + a[n - 1]
end
