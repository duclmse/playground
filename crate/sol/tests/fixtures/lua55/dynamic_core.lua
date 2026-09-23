-- Supported dynamic compatibility profile: `_ENV` globals, tables, closures,
-- multi-result calls, iteration, and protected errors.
global seed = 40
local state = { seed + 2, name = "dynamic" }
local function add(base)
    return function(extra) return base + extra end
end
local total = 0
for _, value in ipairs({1, 2, 3}) do total = total + value end
local ok, message = pcall(function() error("expected") end)
return state[1] == 42 and add(2)(seed) == 42 and total == 6 and not ok and message == "expected"
