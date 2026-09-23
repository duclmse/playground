-- M7 §21: matches codegen.rs's elementwise-vectorization pattern exactly.
-- 7 elements deliberately odd, to exercise the scalar tail path too.
function main(): f64
    local n = 7
    local a = new_array_f64(n)
    local b = new_array_f64(n)
    local c = new_array_f64(n)
    for i = 0, n - 1 do
        a[i] = i + 1
        b[i] = (i + 1) * 2
    end
    for i = 0, n - 1 do
        c[i] = a[i] + b[i]
    end
    local total = 0.0
    for i = 0, n - 1 do
        total = total + c[i]
    end
    return total
end
