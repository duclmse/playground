-- Reference-Lua/LuaJIT equivalent of matrix.sol - see that file.
local n = 120
local size = n * n
local a = {}
local b = {}
local c = {}
for i = 0, size - 1 do
    a[i] = i + 1
    b[i] = size - i
end

for row = 0, n - 1 do
    for col = 0, n - 1 do
        local sum = 0.0
        for k = 0, n - 1 do
            sum = sum + a[row * n + k] * b[k * n + col]
        end
        c[row * n + col] = sum
    end
end

local total = 0.0
for j = 0, size - 1 do
    total = total + c[j]
end
print(total)
