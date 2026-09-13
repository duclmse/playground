-- Vararg / multi-result-call benchmark (L8 checklist: "isolate
-- vararg/multi-result-call overhead" - docs/features/lua-compatibility.md).
-- Exercises the multi-value paths a correctness-only test can't measure:
-- a function returning several fixed results consumed by multiple
-- assignment; a vararg function that forwards `...` into another vararg
-- function and walks it with `select`; and a call used as the sole,
-- trailing argument of another call, which must expand to all of its
-- results rather than being truncated to one.
local function triple(x)
    return x, x + 1, x + 2
end

local function sum_varargs(...)
    local n = select('#', ...)
    local total = 0
    for i = 1, n do
        total = total + select(i, ...)
    end
    return total
end

local function forward(...)
    return sum_varargs(...)
end

local function pass_through(...)
    return ...
end

local total = 0
for i = 1, 2000000 do
    local a, b, c = triple(i)
    total = total + a + b + c
    total = total + forward(pass_through(i, i + 1, i + 2))
end
print(total)
