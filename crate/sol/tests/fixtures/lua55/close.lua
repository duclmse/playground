-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local events = {}
local function resource(id)
    return setmetatable({}, {
        __close = function(_, err)
            events[#events + 1] = {id, err}
        end
    })
end
do
    local a<close> = resource('a');
    local b<close> = resource('b')
end
assert(events[1][1] == 'b' and events[2][1] == 'a' and events[1][2] == nil)
local err = {};
local ok, e = pcall(function()
    local r<close> = resource('error');
    error(err)
end)
assert(not ok and e == err and events[3][2] == err)
local function early()
    local r<close> = resource('return');
    return 5
end
assert(early() == 5 and events[4][1] == 'return')
for i = 1, 2 do
    local r<close> = resource('break');
    break
end
assert(events[5][1] == 'break')
do
    local a<close> = nil;
    local b<close> = false
end
assert(not pcall(function()
    local x<close> = {}
end))
print('ok: close')
