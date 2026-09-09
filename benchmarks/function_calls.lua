-- Reference-Lua/LuaJIT equivalent of function_calls.sol - see that file.
local function work(x)
    local a1 = x + 1
    local a2 = a1 + 1
    local a3 = a2 + 1
    local a4 = a3 + 1
    local a5 = a4 + 1
    local a6 = a5 + 1
    local a7 = a6 + 1
    local a8 = a7 + 1
    local a9 = a8 + 1
    local a10 = a9 + 1
    local a11 = a10 + 1
    local a12 = a11 + 1
    local a13 = a12 + 1
    local a14 = a13 + 1
    local a15 = a14 + 1
    local a16 = a15 + 1
    local a17 = a16 + 1
    local a18 = a17 + 1
    local a19 = a18 + 1
    local a20 = a19 + 1
    local a21 = a20 + 1
    return a21
end

local total = 0
for i = 0, 9999999 do
    total = total + work(i)
end
print(total)
