-- Even length (no scalar tail) and each of +, -, *, / individually.
function sum(arr: Array<f64>, n: i64): f64
    local total = 0.0
    for i = 0, n - 1 do
        total = total + arr[i]
    end
    return total
end

function main(): f64
    local n = 8
    local a = new_array_f64(n)
    local b = new_array_f64(n)
    for i = 0, n - 1 do
        a[i] = i + 2
        b[i] = i + 1
    end

    local add_r = new_array_f64(n)
    for i = 0, n - 1 do
        add_r[i] = a[i] + b[i]
    end
    local sub_r = new_array_f64(n)
    for i = 0, n - 1 do
        sub_r[i] = a[i] - b[i]
    end
    local mul_r = new_array_f64(n)
    for i = 0, n - 1 do
        mul_r[i] = a[i] * b[i]
    end
    local div_r = new_array_f64(n)
    for i = 0, n - 1 do
        div_r[i] = a[i] / b[i]
    end

    return sum(add_r, n) + sum(sub_r, n) + sum(mul_r, n) + sum(div_r, n)
end
