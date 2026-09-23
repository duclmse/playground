function main(): i64
    local seed: Map<i64, i64> = { [7] = 9, [8] = 11 }
    local values: Map<i64, i64> = {}
    for i = 0, 299 do
        values[i * 17] = i + 1
    end
    values[17] = 42
    local total: i64 = seed[7]
    for key, value in pairs(values) do
        total = total + key + value
        values[key] = value
    end

    local array: Array<i64> = { 10, 20, 30 }
    for index, value in ipairs(array) do
        total = total + index + value
    end
    return total + #values + values[999999]
end
