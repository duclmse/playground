function sum(a: Array<f64>): f64
    local s: f64 = 0.0
    for i = 0, #a - 1 do
        s = s + a[i]
    end
    return s
end

function main(): f64
    local a = new_array_f64(5)
    for i = 0, 4 do
        a[i] = i + 1
    end
    return sum(a)
end
