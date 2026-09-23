-- Upstream calls.lua exercises deep dynamic calls across the native
-- boundary via an any-typed parameter (unsupported: the FFI/dynamic
-- boundary only crosses i64/f64/bool). Reinterpreted as typed recursive
-- function calls, which typed Sol supports directly - including mutual
-- recursion between separately-declared top-level functions, both called
-- directly from main (see docs/features/sol-conformance.md's "A bug this
-- suite found": the JIT dependency-walk bug that used to block this is
-- fixed; crates/sol/tests/fixtures/mutual_recursion.sol is the dedicated
-- regression).
function is_even(n: i64): bool
    if n == 0 then
        return true
    end
    return is_odd(n - 1)
end

function is_odd(n: i64): bool
    if n == 0 then
        return false
    end
    return is_even(n - 1)
end

function sum_to(n: i64): i64
    if n == 0 then
        return 0
    end
    return n + sum_to(n - 1)
end

function main(): i64
    local total: i64 = sum_to(500)
    if is_even(1000) then
        if not is_odd(1000) then
            return total
        end
    end
    return -1
end
