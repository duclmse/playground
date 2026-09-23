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

local total = n
for i = 5, 1, -2 do
    total = total + i
end

for i = 1, 100 do
    if i == 3 then
        break
    end
    total = total + 1
end

while true do
    repeat
        total = total + 1;
        break
    until false
    break
end
return total == 16
