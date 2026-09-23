fn main(): i64
    local floats: Map<i64, f64> = { [1] = 1.5, [9] = 2.25 }
    local flags: Map<i64, bool> = { [1] = true, [2] = false }
    floats[9] = floats[9] + 0.25
    flags[2] = true

    local total: f64 = 0.0
    for key, value in pairs(floats) do
        total = total + value
    end
    if flags[1] and flags[2] and total == 4.0 then
        return 42
    end
    return 0
end
