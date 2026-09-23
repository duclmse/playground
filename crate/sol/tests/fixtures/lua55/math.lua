-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(math.type(1) == 'integer' and math.type(1.0) == 'float')
assert(math.tointeger(2.0) == 2 and math.tointeger(2.5) == nil)
assert(math.abs(-3) == 3 and math.floor(-1.2) == -2 and math.ceil(-1.2) == -1)
assert(math.max(1, 3, 2) == 3 and math.min(1, 3, 2) == 1)
assert(math.sqrt(9) == 3 and math.fmod(-7, 2) == -1)

local i, f = math.modf(2.5);
assert(i == 2 and f == 0.5)
assert(math.ult(0, -1) and math.huge > math.maxinteger)
assert(math.sin(0) == 0 and math.cos(0) == 1 and math.tan(0) == 0)
assert(math.asin(0) == 0 and math.acos(1) == 0 and math.atan(0, 1) == 0)
assert(math.exp(0) == 1 and math.log(1) == 0)
assert(math.abs(math.deg(math.pi) - 180) < 1e-10 and math.abs(math.rad(180) - math.pi) < 1e-10)
math.randomseed(123, 456);

local a = math.random();
math.randomseed(123, 456);
assert(math.random() == a)
for i = 1, 50 do
    local x = math.random(-3, 3);
    assert(x >= -3 and x <= 3 and math.type(x) == 'integer')
end
assert(math.type(math.random(0)) == 'integer')
print('ok: math')
