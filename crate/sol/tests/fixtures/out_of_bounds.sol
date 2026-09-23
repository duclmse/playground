function main(): i64
    local a = new_array_i64(3)
    a[0] = 10
    a[1] = 20
    a[2] = 30
    -- Deliberately out of range - must trap, not silently read garbage
    -- or corrupt adjacent memory (see docs/features/safety.md).
    return a[3]
end
