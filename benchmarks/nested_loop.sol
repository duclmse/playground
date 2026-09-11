-- Typed Sol equivalent of nested_loop.lua: 9,000,000 increments.
function main(): i64
    local count: i64 = 0
    for i = 1, 3000 do
        for j = 1, 3000 do
            count = count + 1
        end
    end
    return count
end
