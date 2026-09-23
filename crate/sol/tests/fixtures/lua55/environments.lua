-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(_G._G == _G)
local outer = 7
local env = {
    assert = assert,
    x = 10
}
do
    local _ENV = env;
    x = x + 1;
    assert(x == 11)
end
assert(env.x == 11 and outer == 7)
local fn = assert(load('return x+1', 'env', 't', env));
assert(fn() == 12)
local pieces = {'return ', '6*7'};
local i = 0
local f = assert(load(function()
    i = i + 1;
    return pieces[i]
end));
assert(f() == 42)
local binary = string.dump(function(a)
    return a + 1
end)
assert(assert(load(binary, 'binary', 'b'))(4) == 5)
assert(load(binary, 'binary', 't') == nil)
print('ok: environments')
