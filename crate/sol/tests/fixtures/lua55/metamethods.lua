-- Every language metamethod family, fallback order, and callable objects.
local ops = {{'__add', function(a, b)
    return a + b
end}, {'__sub', function(a, b)
    return a - b
end}, {'__mul', function(a, b)
    return a * b
end}, {'__div', function(a, b)
    return a / b
end}, {'__idiv', function(a, b)
    return a // b
end}, {'__mod', function(a, b)
    return a % b
end}, {'__pow', function(a, b)
    return a ^ b
end}, {'__band', function(a, b)
    return a & b
end}, {'__bor', function(a, b)
    return a | b
end}, {'__bxor', function(a, b)
    return a ~ b
end}, {'__shl', function(a, b)
    return a << b
end}, {'__shr', function(a, b)
    return a >> b
end}, {'__concat', function(a, b)
    return a .. b
end}}

for _, op in ipairs(ops) do
    local left, right = {}, {};
    local calls = 0
    setmetatable(right, {
        [op[1]] = function(a, b)
            assert(a == left and b == right);
            calls = calls + 1;
            return 42
        end
    })
    assert(op[2](left, right) == 42 and calls == 1, op[1])
    setmetatable(left, {
        [op[1]] = function()
            return 43
        end
    })
    assert(op[2](left, right) == 43, op[1])
end

local t = setmetatable({}, {
    __unm = function()
        return 10
    end,
    __bnot = function()
        return 11
    end,
    __len = function()
        return 12
    end,
    __tostring = function()
        return 'object'
    end,
    __call = function(self, x)
        return x + 1
    end,
    __index = {
        answer = 42
    },
    __newindex = function(self, k, v)
        rawset(self, k, v * 2)
    end,
    __pairs = function()
        return next, {
            key = 7
        }, nil
    end
})

assert(-t == 10 and ~t == 11 and #t == 12 and tostring(t) == 'object' and t(3) == 4)
assert(t.answer == 42);
t.x = 5;
assert(t.x == 10)
for k, v in pairs(t) do
    assert(k == 'key' and v == 7)
end
local mt = {
    __eq = function(a, b)
        return a.v == b.v
    end,
    __lt = function(a, b)
        return a.v < b.v
    end,
    __le = function(a, b)
        return a.v <= b.v
    end
}
local a, b, c = setmetatable({
    v = 1
}, mt), setmetatable({
    v = 1
}, mt), setmetatable({
    v = 2
}, mt)
assert(a == b and a ~= c and a < c and a <= b and c > a and c >= b)
mt.__le = nil;
assert(not pcall(function()
    return a <= b
end))
local callable = setmetatable({}, {
    __call = function(_, x)
        return x * 2
    end
})
assert(callable(4) == 8)
-- 5.5 bounds chains of callable objects.
local chain = function()
    return true
end
for i = 1, 16 do
    chain = setmetatable({}, {
        __call = chain
    })
end
assert(not pcall(chain))
print('ok: metamethods')
