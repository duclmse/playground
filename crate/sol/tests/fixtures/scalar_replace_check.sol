-- `p` never escapes `main` (never returned, passed to a call, stored into
-- an array/another struct, or reassigned as a whole) - eligible for M3
-- scalar replacement (escape.rs), which should compile this to plain
-- arithmetic with zero heap allocation. See
-- tests/programs.rs's non_escaping_struct_local_has_no_allocation_in_the_emitted_ir.
struct Point {
    x: f64,
    y: f64,
}

function main(): f64
    local p = Point { x = 3.0, y = 4.0 }
    return p.x * p.x + p.y * p.y
end
