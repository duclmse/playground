local count = 0
for i = 0x7ffffffffffffffe, 0x7fffffffffffffff do
    count = count + 1
end

for i = 0x8000000000000001, 0x8000000000000000, -1 do
    count = count + 1
end

local start = 1
local stop = 4
local step = 1
for i = start, stop, step do
    start = 100;
    stop = 100;
    step = 100
    count = count + i
end
return count == 14
