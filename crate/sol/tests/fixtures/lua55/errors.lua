-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local marker = {};
local ok, e = pcall(error, marker);
assert(not ok and e == marker)
local ok, e = pcall(error, nil);
assert(not ok and type(e) == 'string')
local ok, e = xpcall(function(a)
    assert(a == 42);
    error('boom')
end, function(e)
    return 'handled:' .. e
end, 42)
assert(not ok and e:find('handled:') and e:find('boom'))
assert(not pcall(assert, false, 'failed'))
assert(load('local =') == nil)
warn('@off');
warn('fixture warning');
warn('@on');
warn('@off')
print('ok: errors')
