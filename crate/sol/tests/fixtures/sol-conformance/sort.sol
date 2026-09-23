-- Upstream sort.lua exercises table.sort and table.create's hash-size
-- hint. Neither exists for typed Sol's Array<T>: there is no built-in sort
-- intrinsic and no hash-part concept at all - Array<T> is a fixed-length
-- dense buffer (docs/spec/types-and-values.md). Reinterpreted as a
-- hand-written insertion sort over Array<i64>, the closest typed analogue
-- to "the table library can put a sequence in order."
function main(): i64
    local n: i64 = 20
    local a = new_array_i64(n)
    a[0] = 9
    a[1] = 3
    a[2] = 17
    a[3] = 1
    a[4] = 8
    a[5] = 2
    a[6] = 19
    a[7] = 0
    a[8] = 15
    a[9] = 4
    a[10] = 11
    a[11] = 6
    a[12] = 13
    a[13] = 10
    a[14] = 5
    a[15] = 18
    a[16] = 7
    a[17] = 16
    a[18] = 12
    a[19] = 14

    local i: i64 = 1
    while i < n do
        local key: i64 = a[i]
        local j: i64 = i - 1
        while j >= 0 and a[j] > key do
            a[j + 1] = a[j]
            j = j - 1
        end
        a[j + 1] = key
        i = i + 1
    end

    local ok: i64 = 1
    for idx = 0, n - 2 do
        if a[idx] > a[idx + 1] then
            ok = 0
        end
    end
    return ok * 100 + a[0] + a[n - 1]
end
