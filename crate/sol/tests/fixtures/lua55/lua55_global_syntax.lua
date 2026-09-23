-- Parser-only fixture for Lua 5.5's global declarations. Execution remains
-- blocked until the dynamic `_ENV` runtime lands in M13 L2.
global <const> *
global answer = 42
global function identity(value)
    return value
end
local function local_identity(value)
    return value
end
function object.method:call(value)
    return value
end
global none
local table_value = {1; key = 2, ["other"] = 3}
require "fixture_module"
consume {4}
local closure = function(first, ...values)
    local rest = ...
    return first, ...
end
::again::
goto again
for key, value in pairs(table_value) do
    table_value:insert(value)
end
(function() return 1 end)()
