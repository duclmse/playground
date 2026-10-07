function generic(n: i64): i64
    return coroutine.yield(n)
end
function typed(n: i64): i64
    return generic(n)
end
local co = coroutine.create(function() return typed(40) end)
local ok = coroutine.resume(co)
print(ok, coroutine.status(co))
local again = coroutine.resume(co)
print(again)
