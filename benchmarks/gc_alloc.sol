-- sol equivalent of gc_alloc.lua - see that file's comment. `p` is
-- assigned from a function call, not a struct literal, so M3's escape
-- analysis (which only recognizes direct struct-literal initializers)
-- never considers it a scalar-replacement candidate: every iteration
-- genuinely allocates and immediately orphans a Point, exercising gc.rs's
-- mark-sweep collector rather than being optimized away entirely.
struct Point {
    x: f64,
    y: f64,
}

function make(i: f64): Point
    return Point { x = i, y = i * 2.0 }
end

function main(): f64
    local total = 0.0
    for i = 1, 2000000 do
        local t = make(i)
        total = total + t.x + t.y
    end
    return total
end
