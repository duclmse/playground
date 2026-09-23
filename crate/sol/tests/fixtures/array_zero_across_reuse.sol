-- Memory-management follow-up (docs/features/memory-management.md and the benchmark
-- section): gc.rs stopped bulk-zeroing a whole chunk on every reuse,
-- relying instead on `sol_gc_alloc_atomic`'s own per-carve zero-fill
-- to uphold the language's "an unwritten array element reads as zero"
-- guarantee. This is the regression test for that specific guarantee,
-- across many real chunk reuses. `dirty` writes every element to a
-- definitely-nonzero value and is immediately discarded, deliberately
-- polluting whatever chunk memory it occupied; `a` (allocated right
-- after, likely reusing `dirty`'s just-freed position once a collection
-- runs) only ever writes element 0 - if the zero-fill wiring were ever
-- wrong, a[5] would read back `dirty`'s leftover 999.0 instead of 0.0,
-- and this sum would come out far from 0.
function main(): f64
    local total = 0.0
    for i = 0, 19999 do
        local dirty = new_array_f64(10)
        for j = 0, 9 do
            dirty[j] = 999.0
        end

        local a = new_array_f64(10)
        a[0] = i + 1.0
        total = total + a[5]
    end
    return total
end
