function double_it(value: f64): f64
    return value * 2.0
end

function main(): f64
    local input: Array<f64> = { 1.5, 2.5, 4.0 }
    local output: Array<f64> = map(input, double_it)
    local total: f64 = output[0] + output[1] + output[2]
    return total
end
