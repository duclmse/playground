-- Regression for the array-atomicity fix: `Array<Point>`'s data buffer
-- holds real pointers (each element is a `Point` struct pointer), so it
-- must be allocated via sol_new_array_ptr/Op::NewArrayPtr (traced) rather
-- than the scalar-array path (sol_new_array_i64/f64, atomic - never
-- scanned). If that path were wrong, every `Point` here would be
-- collected out from under `pts` by the garbage-generating loop below.
struct Point {
    x: i64,
    y: i64,
}

function main(): i64
    local pts: Array<Point> = {
        Point { x = 1, y = 2 },
        Point { x = 3, y = 4 },
        Point { x = 5, y = 6 },
    }

    for i = 0, 19999 do
        local junk = new_array_i64(100)
        junk[0] = i
    end

    return pts[0].x + pts[0].y + pts[1].x + pts[1].y + pts[2].x + pts[2].y
end
