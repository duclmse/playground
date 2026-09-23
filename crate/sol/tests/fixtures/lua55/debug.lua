-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local n = 1;
local function f(x)
    return n + x
end
local info = debug.getinfo(f, 'Su');
assert(info.what == 'Lua' and info.nups == 1)
local name, value = debug.getupvalue(f, 1);
assert(name == 'n' and value == 1)
assert(debug.setupvalue(f, 1, 3) == 'n' and f(2) == 5)
local function g()
    return n
end
assert(debug.upvalueid(f, 1) == debug.upvalueid(g, 1))
assert(type(debug.getregistry()) == 'table')
local t = {};
debug.setmetatable(t, {
    __index = function()
        return 42
    end
});
assert(t.x == 42)
assert(type(debug.getmetatable(t)) == 'table')
local calls = 0;
debug.sethook(function()
    calls = calls + 1
end, '', 1)
local x = 1 + 2;
debug.sethook();
assert(calls > 0 and x == 3)
assert(type(debug.traceback('fixture')) == 'string')
for _, k in ipairs({'getlocal', 'setlocal', 'gethook', 'upvaluejoin', 'getuservalue', 'setuservalue', 'debug'}) do
    assert(type(debug[k]) == 'function', k)
end
print('ok: debug')
