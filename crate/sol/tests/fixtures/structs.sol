struct Point {
    x: f64,
    y: f64,
}

function dist_squared(p: Point): f64
    return p.x * p.x + p.y * p.y
end

function main(): f64
    -- Fields given out of declaration order on purpose - typeck.rs must
    -- reorder to the struct's declared layout.
    local p = Point { y = 4.0, x = 3.0 }
    p.x = p.x + 1.0
    return dist_squared(p)
end
