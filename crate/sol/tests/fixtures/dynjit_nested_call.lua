local function add(a, b)
    return a + b
end

local function compute(x)
    local y = add(x, 1)
    local z = add(y, x)
    return z + 10
end

print(compute(5))
print(compute(6))
