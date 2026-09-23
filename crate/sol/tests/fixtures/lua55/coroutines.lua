-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local main, is_main = coroutine.running();
assert(type(main) == 'thread' and is_main)
assert(not coroutine.isyieldable())
local co = coroutine.create(function(x)
    assert(coroutine.isyieldable());
    local y = coroutine.yield(x + 1);
    return y + 2
end)
assert(coroutine.status(co) == 'suspended')
local ok, v = coroutine.resume(co, 10);
assert(ok and v == 11)
ok, v = coroutine.resume(co, 20);
assert(ok and v == 22 and coroutine.status(co) == 'dead')
assert(not coroutine.resume(co))
local wrap = coroutine.wrap(function()
    coroutine.yield(1);
    return 2
end);
assert(wrap() == 1 and wrap() == 2)
local closed = false
co = coroutine.create(function()
    local r<close> = setmetatable({}, {
        __close = function()
            closed = true
        end
    });
    coroutine.yield()
end)
assert(coroutine.resume(co));
assert(coroutine.close(co));
assert(closed)
co = coroutine.create(function()
    local ok, v = pcall(function()
        coroutine.yield(7);
        return 8
    end);
    assert(ok);
    return v
end)
ok, v = coroutine.resume(co);
assert(ok and v == 7);
ok, v = coroutine.resume(co);
assert(ok and v == 8)
print('ok: coroutines')
