-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(type(nil) == 'nil' and type(true) == 'boolean' and type(1) == 'number')
assert(tonumber('ff', 16) == 255 and tonumber('bad') == nil and tonumber('2.5') == 2.5)
assert(tostring(true) == 'true' and tostring(nil) == 'nil')
local t = {};
assert(rawset(t, 'x', 3) == t and rawget(t, 'x') == 3)
assert(rawequal(t, t) and not rawequal({}, {}))
assert(rawlen('abc') == 3 and rawlen({1, 2}) == 2)
assert(next({}) == nil)
local mt = {
    __metatable = 'locked'
};
setmetatable(t, mt)
assert(getmetatable(t) == 'locked' and not pcall(setmetatable, t, {}))
assert(assert(7, 8) == 7)
print('ok: base_library')
