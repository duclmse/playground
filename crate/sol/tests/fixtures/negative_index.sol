function main(): i64
    local a = new_array_i64(3)
    a[0] = 10
    local i: i64 = 0
    i = i - 1
    -- A negative index must also trap (the unsigned-compare trick in
    -- array_elem_addr treats it as a huge index, not as "wrap to the end").
    return a[i]
end
