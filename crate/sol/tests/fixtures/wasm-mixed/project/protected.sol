function add(n: i64): i64
    return n + 2
end
print(add(40))
local ok = pcall(add, 'bad')
print(ok)
