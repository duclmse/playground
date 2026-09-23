function add_one(value: i64): i64
    return value + 1
end

function main(): i64
    local input: Array<i64> = { 10, 20, 9 }
    local output: Array<i64> = map(input, add_one)
    return output[0] + output[1] + output[2]
end
