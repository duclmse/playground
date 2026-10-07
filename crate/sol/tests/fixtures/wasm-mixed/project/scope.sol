export function helper(n: i64): i64
    return n + 1
end
export function via_value(): i64
    local f: fn(i64) -> i64 = helper
    return f(41)
end
export function shadow(): i64
    local helper: i64 = 42
    return helper
end
export function param(helper: i64): i64
    return helper
end
export function loop(): i64
    local total: i64 = 0
    for helper = 1, 3 do
        total = total + helper
    end
    return total
end
