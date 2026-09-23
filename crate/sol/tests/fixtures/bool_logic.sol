function is_even(n: i64): bool
    return n % 2 == 0
end

function main(): bool
    local a = is_even(10)
    local b = is_even(7)
    return a and not b
end
