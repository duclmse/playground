-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local function values(...)
    return ...
end
local a, b, c = values(1, nil, 3);
assert(a == 1 and b == nil and c == 3)
local t = {values(1, 2, 3)};
assert(#t == 3)
local u = {(values(1, 2, 3))};
assert(#u == 1)
a, b = values(4, 5), 9;
assert(a == 4 and b == 9)
a, b = b, a;
assert(a == 9 and b == 4)
local function counter()
    local n = 0;
    return function()
        n = n + 1;
        return n
    end
end
local f, g = counter(), counter();
assert(f() == 1 and f() == 2 and g() == 1)
local function tail(n, acc)
    if n == 0 then
        return acc
    end
    return tail(n - 1, acc + 1)
end
assert(tail(20000, 0) == 20000)
local o = {
    nested = {
        x = 2
    }
};
function o.nested:add(v)
    return self.x + v
end
assert(o.nested:add(3) == 5)
local function identity(x)
    return x
end
assert(identity 's' == 's' and identity{
    x = 2
}.x == 2)
assert(select('#', values(1, nil, 3)) == 3 and select(2, 1, 7) == 7)
print('ok: functions')
