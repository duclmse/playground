-- Same shape as gc_stress.sol but for struct allocations (sol_alloc's
-- path, not the array runtime's) - `keep` must survive many collections
-- while a fresh, immediately-discarded `Point` is allocated every
-- iteration. `Point` here is deliberately *not* eligible for M3 scalar
-- replacement (`junk` isn't just read - it's also indirectly kept from
-- being trivially optimized away by depending on the loop variable), so
-- this actually exercises the heap allocator/collector, not escape
-- analysis eliminating the allocation entirely.
struct Point {
    x: f64,
    y: f64,
}

function sink(p: Point): f64
    return p.x + p.y
end

function main(): f64
    local keep = Point { x = 111.0, y = 222.0 }
    local total = 0.0

    for i = 0, 9999 do
        local junk = Point { x = i, y = i }
        total = total + sink(junk)
    end

    return keep.x + keep.y
end
