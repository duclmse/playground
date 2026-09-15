-- Sol language demo - a typed runtime trap, not a demo_* checksum
-- contributor. A negative (or otherwise unrepresentable) array-allocation
-- length must trap rather than wrapping into a huge unsigned size
-- (docs/spec/statements-and-expressions.md). Run via
-- scripts/run-sol-demo-traps.sh, which expects this program to abort.

function main(): i64
    local n: i64 = 0 - 1
    local xs: Array<i64> = new_array_i64(n)
    return #xs
end
