-- M4 GC benchmark (docs/sol-roadmap.md's "own benchmark category"
-- ask): pure allocation churn - a fresh, short-lived table every
-- iteration, immediately discarded. Exercises each implementation's
-- allocator + collector, not arithmetic throughput.
local function make(i)
    return { x = i, y = i * 2 }
end

local total = 0
for i = 1, 2000000 do
    local t = make(i)
    total = total + t.x + t.y
end
print(total)
