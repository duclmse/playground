-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local n = 0;
do
    local x = 3;
    n = x
end
if false then
    n = 99
elseif n == 3 then
    n = 4
else
    n = 98
end
while n < 7 do
    n = n + 1
end
repeat
    local done = n == 5;
    n = n - 1
until done
assert(n == 4)
for i = 5, 1, -2 do
    n = n + i
end
assert(n == 13)
for i = 1, 100 do
    if i == 3 then
        break
    end
    n = n + 1
end
assert(n == 15)
local s = 0;
for k, v in ipairs({2, 4, 6}) do
    s = s + k * v
end
assert(s == 28)
::again::
n = n - 1;
if n > 0 then
    goto again
end
assert(n == 0)
local fs = {};
for i = 1, 3 do
    fs[i] = function()
        return i
    end
end
assert(fs[1]() == 1 and fs[3]() == 3)
print('ok: control_flow')
