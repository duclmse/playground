local function loop_sum(n)
    local total = 0
    local i = 1
    while i <= n do
        total = total + i
        i = i + 1
    end
    return total
end

print(loop_sum(2000))
