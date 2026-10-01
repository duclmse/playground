counter = 0

local function touch(key)
    local t = {}
    t.x = 10
    t.y = 20
    t[key] = 30
    counter = counter + 1
    return t.x + t.y + t[key] + counter
end

print(touch("z"))
print(touch("z"))
