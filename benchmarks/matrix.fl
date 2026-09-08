-- faster_lua.md §33's "14_matrix" category: classic dense N x N matrix
-- multiply over a flat Array<f64> (row-major), O(n^3).
function main(): f64
    local n = 120
    local size = n * n
    local a = new_array_f64(size)
    local b = new_array_f64(size)
    local c = new_array_f64(size)
    local i = 0
    while i < size do
        a[i] = i + 1
        b[i] = size - i
        i = i + 1
    end

    local row = 0
    while row < n do
        local col = 0
        while col < n do
            local sum = 0.0
            local k = 0
            while k < n do
                sum = sum + a[row * n + k] * b[k * n + col]
                k = k + 1
            end
            c[row * n + col] = sum
            col = col + 1
        end
        row = row + 1
    end

    local total = 0.0
    local j = 0
    while j < size do
        total = total + c[j]
        j = j + 1
    end
    return total
end
