function typed(n: i64): i64
    return n + 1
end
local wrapped = coroutine.wrap(function() return typed("bad") end)
local ok = pcall(wrapped)
print(ok)
local co = coroutine.create(function()
    local guard <close> = setmetatable({}, { __close = function() print("closed") end })
    return typed("bad")
end)
local resumed = coroutine.resume(co)
print(resumed, coroutine.status(co))
local closed = coroutine.close(co)
print(closed)
local direct = coroutine.create(typed)
print(coroutine.resume(direct, 40))
