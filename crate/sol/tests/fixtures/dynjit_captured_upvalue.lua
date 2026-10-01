local function make_counter()
    local n = 0
    local function inc()
        n = n + 1
        return n
    end
    local a = inc()
    local b = inc()
    local c = inc()
    return a + b + c
end

print(make_counter())
print(make_counter())
