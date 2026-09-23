-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
collectgarbage('collect');
assert(type(collectgarbage('count')) == 'number')
collectgarbage('stop');
assert(not collectgarbage('isrunning'));
collectgarbage('restart')
assert(collectgarbage('isrunning'));
collectgarbage('step', 0)
collectgarbage('incremental');
collectgarbage('generational')
local finalized = 0
do
    local x = setmetatable({}, {
        __gc = function()
            finalized = finalized + 1
        end
    })
end
collectgarbage('collect');
assert(finalized == 1)
local weak = setmetatable({}, {
    __mode = 'kv'
})
do
    local k, v = {}, {};
    weak[k] = v
end
collectgarbage('collect');
assert(next(weak) == nil)
local values = setmetatable({}, {
    __mode = 'v'
});
do
    values[1] = {}
end
collectgarbage('collect');
assert(values[1] == nil)
print('ok: garbage_collection')
