function typed(n: i64): i64
    local value: i64 = n + 1
    return value
end
local root = { value = 40 }
local leaf = coroutine.create(function()
    local value = root.value
    local result = typed(value)
    coroutine.yield(result)
    return typed(result)
end)
local parent = coroutine.create(function()
    local marker = 7
    print(coroutine.resume(leaf))
    local ok, result = coroutine.resume(leaf)
    print(marker)
    return ok, result
end)
print(coroutine.resume(parent))
print(coroutine.status(parent), coroutine.status(leaf))
