-- Regression for a JIT dependency-walk bug: `is_even`/`is_odd` mutually
-- recurse without either being *directly* self-recursive, so both are
-- eligible for compile-time inlining; `main` calls both directly. See
-- docs/features/sol-conformance.md's "A bug this suite found" section.
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

function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 100 do
        if is_even(i) then
            total = total + 1
        end
        if is_odd(i) then
            total = total + 1
        end
        i = i + 1
    end
    return total
end
