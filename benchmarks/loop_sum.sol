-- Typed Sol equivalent of loop_sum.lua: one tight arithmetic loop.
function main(): i64
    local sum: i64 = 0
    for i = 1, 20000000 do
        sum = sum + i
    end
    return sum
end
