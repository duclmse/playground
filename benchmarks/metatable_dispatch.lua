-- Metatable-dispatch / polymorphic-field-access benchmark (L8 checklist:
-- "Benchmark ... polymorphic field access ... metatable dispatch" -
-- docs/features/lua-compatibility.md). Three distinct "classes" share no
-- common base, so `shape:area()` is a real polymorphic call site: each
-- invocation resolves `area` through a different object's metatable
-- (`__index`), rather than hitting the same method every time the way a
-- monomorphic call site would - this defeats a single-shape inline cache
-- and exercises the general metatable-lookup path on every call.
local Circle = {}
Circle.__index = Circle
function Circle.new(radius) return setmetatable({ radius = radius }, Circle) end
function Circle:area() return 3.14159265358979 * self.radius * self.radius end

local Square = {}
Square.__index = Square
function Square.new(side) return setmetatable({ side = side }, Square) end
function Square:area() return self.side * self.side end

local Triangle = {}
Triangle.__index = Triangle
function Triangle.new(base, height) return setmetatable({ base = base, height = height }, Triangle) end
function Triangle:area() return 0.5 * self.base * self.height end

local shapes = {}
for i = 1, 3000 do
    local kind = i % 3
    if kind == 0 then
        shapes[i] = Circle.new(i % 50 + 1)
    elseif kind == 1 then
        shapes[i] = Square.new(i % 50 + 1)
    else
        shapes[i] = Triangle.new(i % 50 + 1, i % 30 + 1)
    end
end

local total = 0.0
for _ = 1, 200 do
    for i = 1, #shapes do
        total = total + shapes[i]:area()
    end
end
print(total)
