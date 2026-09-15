-- Sol language demo - a typed runtime trap, not a demo_* checksum
-- contributor. `value as Type` is a checked cast: it traps when the boxed
-- `any` value's runtime type doesn't match `Type`, rather than
-- reinterpreting the value's memory (docs/spec/types-and-values.md). Run via
-- scripts/run-sol-demo-traps.sh, which expects this program to abort.

struct Point { x: i64, y: i64 }
struct Other { z: i64 }

function main(): i64
    local p: Point = Point { x = 1, y = 2 }
    local boxed: any = p
    local mismatched: Other = boxed as Other
    return mismatched.z
end
