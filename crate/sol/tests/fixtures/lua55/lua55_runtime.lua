-- New 5.5 runtime/library behavior beyond declarations and varargs.
for _, p in ipairs({'minormul', 'majorminor', 'minormajor', 'pause', 'stepmul', 'stepsize'}) do
    local old = collectgarbage('param', p)
    assert(type(old) == 'number')
    assert(collectgarbage('param', p, old) == old)
    assert(collectgarbage('param', p) == old)
end
assert(not pcall(collectgarbage, 'param', 'invalid'))
local closed = false
local co = coroutine.create(function()
    local r<close> = setmetatable({}, {
        __close = function()
            closed = true
        end
    })
    coroutine.close() -- 5.5: close the running coroutine; never returns.
    error('unreachable')
end)
assert(coroutine.resume(co));
assert(closed and coroutine.status(co) == 'dead')
local define = assert(load('global fixture_initialized = 1'))
define();
assert(not pcall(define))
local f = assert(load('global <const> *; return math.pi'));
assert(f() == math.pi)
print('ok: lua55_runtime')
