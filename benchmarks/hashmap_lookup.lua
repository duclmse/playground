local values = {}
for key = 0, 999 do
    values[key] = key + 1
end

local total = 0
for _ = 0, 99 do
    for key = 0, 999 do
        total = total + values[key]
    end
end
print(total)
