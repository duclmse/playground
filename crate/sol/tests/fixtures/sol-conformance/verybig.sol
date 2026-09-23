-- Upstream verybig.lua needs os.tmpname/io.output (unimplemented) to spill
-- a huge table through a temp file. Reinterpreted as a large in-memory
-- Array<i64> stress test exercising the same "very big data" intent
-- without any filesystem dependency.
function main(): i64
    local n: i64 = 200000
    local data = new_array_i64(n)
    local i: i64 = 0
    while i < n do
        data[i] = i
        i = i + 1
    end

    local total: i64 = 0
    i = 0
    while i < n do
        total = total + data[i]
        i = i + 1
    end
    return total % 1000000007
end
