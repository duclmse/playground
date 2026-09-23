-- Parser-only Lua 5.5 surface fixture. Dynamic execution is intentionally
-- deferred to M13 L2-L4; this file must nevertheless parse as Lua source.

global declared, configured <const> = 1, 2

local first, second = 1, 2
local record = {
    first,
    named = second;
    [first + second] = "computed",
}

do
    local hidden = record["named"]
    record.hidden = hidden
end

if first > second then
    first = second
elseif first == second then
    first = first + 1
else
    first, second = second, first
end

while first < 4 do
    first = first + 1
end

repeat
    second = second - 1
until second == 0

for index = 4, 1, -1 do
    record[index] = index
end

for key, value in pairs(record), record, nil do
    record:key(value)
end

function module.member:method(value, ...values)
    return value, ...
end

local function collect(...items)
    local values = ...
    return function()
        return values
    end
end

local result = (function(value)
    return value
end)(record)

consume "string shorthand"
consume { result }

::again::
goto again

return first, second
