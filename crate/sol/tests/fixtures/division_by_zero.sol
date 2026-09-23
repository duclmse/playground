function main(): i64
    local a: i64 = 10
    local b: i64 = 0
    -- `b` is a variable, not a literal, so `optimize.rs`'s constant folder
    -- can't (and doesn't try to) fold this away - it must reach codegen's
    -- plain `sdiv` and trap there (confirmed: Cranelift's `sdiv`/`srem`
    -- already trap on a zero divisor on this target, see
    -- docs/features/safety.md).
    return a // b
end
