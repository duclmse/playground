local function sum_table(t)
    local total = 0
    for _, v in ipairs(t) do
        total = total + v
    end
    return total
end

local arr = {10, 20, 30, 40}
print(sum_table(arr))
print(sum_table(arr))
