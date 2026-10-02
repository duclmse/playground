local function add_one(x)
    return x + 1
end

local function times_ten(x)
    return x * 10
end

local function caller(f)
    local x = 5 + 2
    local r = f(x)
    return r
end

print(caller(add_one))
print(caller(add_one))
print(caller(times_ten))
