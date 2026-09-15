-- Sol language demo - a typed runtime trap, not a demo_* checksum
-- contributor. Array reads/writes must trap when the index is negative or
-- >= the array's length (docs/spec/statements-and-expressions.md), rather
-- than returning garbage or corrupting memory. Run via
-- scripts/run-sol-demo-traps.sh, which expects this program to abort.

function main(): i64 {
    local xs: Array<i64> = new_array_i64(3)
    return xs[5]
}
