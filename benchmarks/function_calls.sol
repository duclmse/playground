-- faster_lua.md §33's "03_function_calls" category: real (non-inlined)
-- call overhead - `work` is padded past M2's INLINE_MAX_STMTS (20) so it
-- stays a genuine call, not scalar-replaced away.
function work(x: i64): i64
    local a1 = x + 1
    local a2 = a1 + 1
    local a3 = a2 + 1
    local a4 = a3 + 1
    local a5 = a4 + 1
    local a6 = a5 + 1
    local a7 = a6 + 1
    local a8 = a7 + 1
    local a9 = a8 + 1
    local a10 = a9 + 1
    local a11 = a10 + 1
    local a12 = a11 + 1
    local a13 = a12 + 1
    local a14 = a13 + 1
    local a15 = a14 + 1
    local a16 = a15 + 1
    local a17 = a16 + 1
    local a18 = a17 + 1
    local a19 = a18 + 1
    local a20 = a19 + 1
    local a21 = a20 + 1
    return a21
end

function main(): i64
    local total = 0
    local i = 0
    while i < 10000000 do
        total = total + work(i)
        i = i + 1
    end
    return total
end
