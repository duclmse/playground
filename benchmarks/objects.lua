-- Reference-Lua/LuaJIT equivalent of objects.fl - a table stands in for
-- fastlua's struct (Lua's only aggregate type).
local function dist_squared(p)
    local s1 = p.x * p.x
    local s2 = s1 + 0.0
    local s3 = s2 + 0.0
    local s4 = s3 + 0.0
    local s5 = s4 + 0.0
    local s6 = s5 + 0.0
    local s7 = s6 + 0.0
    local s8 = s7 + 0.0
    local s9 = s8 + 0.0
    local s10 = s9 + 0.0
    local s11 = s10 + 0.0
    local s12 = s11 + 0.0
    local s13 = s12 + 0.0
    local s14 = s13 + 0.0
    local s15 = s14 + 0.0
    local s16 = s15 + 0.0
    local s17 = s16 + 0.0
    local s18 = s17 + 0.0
    local s19 = s18 + 0.0
    local s20 = s19 + 0.0
    return s20 + p.y * p.y
end

local total = 0.0
for i = 0, 1999999 do
    local p = { x = i, y = i }
    total = total + dist_squared(p)
end
print(total)
