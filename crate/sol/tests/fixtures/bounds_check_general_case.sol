-- A loop that does NOT match the `for i = 0, #a - 1` elimination pattern
-- (stop bound is an arbitrary local, not `#a - 1`) - must still be
-- correctly bounds-checked (and correct) even though the fast-path
-- elimination in codegen.rs's `recognize_safe_for_loop` doesn't fire here.
function main(): i64
    local a = new_array_i64(5)
    for i = 0, 4 do
        a[i] = i * 2
    end
    local limit = 4
    local sum = 0
    for i = 0, limit do
        sum = sum + a[i]
    end
    return sum
end
