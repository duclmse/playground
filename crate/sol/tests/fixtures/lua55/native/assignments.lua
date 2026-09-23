local a, b = 10, 20
local c, d = b, a
a, b = b, a
local <const> x, y = 2, 3
local empty
empty = 7
local z
z = false
return a == 20 and b == 10 and c == 20 and d == 10 and x + y == 5 and empty == 7 and z == false
