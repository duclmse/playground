function main(): i64
    local values: Map<i64, i64> = {}
    for key = 0, 999 do
        values[key] = key + 1
    end

    local total: i64 = 0
    for round = 0, 99 do
        for key = 0, 999 do
            total = total + values[key]
        end
    end
    return total
end
