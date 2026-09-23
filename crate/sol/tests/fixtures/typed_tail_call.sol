function loop(remaining: i64, total: i64): i64
    if remaining == 0 then
        return total
    end
    return loop(remaining - 1, total + 1)
end

function main(): i64
    return loop(100000, 0)
end
