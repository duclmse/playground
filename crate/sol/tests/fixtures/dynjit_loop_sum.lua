local function loop_sum(n)
    local total = 0
    for i = 1, n do
        total = total + i
    end
    return total
end

print(loop_sum(100))
