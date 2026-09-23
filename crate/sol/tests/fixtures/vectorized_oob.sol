-- Matches the vectorization pattern but writes past `c`'s bounds - must still trap.
function main(): f64
    local a = new_array_f64(4)
    local b = new_array_f64(4)
    local c = new_array_f64(2)
    for i = 0, 3 do
        c[i] = a[i] + b[i]
    end
    return c[0]
end
