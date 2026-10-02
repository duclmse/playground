function add_one(value: i64): i64
    return value + 1
end

function main(): f64
    local input: Array<f64> = { 1.0, 2.0 }
    local output: Array<f64> = map(input, add_one)
    return output[0]
end
