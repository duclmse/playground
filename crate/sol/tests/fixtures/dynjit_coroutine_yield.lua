local function yielder()
    coroutine.yield(1)
    coroutine.yield(2)
    return 3
end

local function driver()
    local co = coroutine.create(yielder)
    local ok1, v1 = coroutine.resume(co)
    local ok2, v2 = coroutine.resume(co)
    local ok3, v3 = coroutine.resume(co)
    return v1 + v2 + v3
end

print(driver())
print(driver())
