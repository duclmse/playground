-- Sol language demo - a typed runtime trap, not a demo_* checksum
-- contributor. Integer floor division (`//`) or `%` by zero traps
-- (docs/spec/statements-and-expressions.md); float `/` always widens to
-- `f64` and would produce infinity instead, so this uses `//` to exercise
-- the actual integer-division trap. Run via scripts/run-sol-demo-traps.sh,
-- which expects this program to abort.

function main(): i64
    local a: i64 = 10
    local b: i64 = 0
    return a // b
end
