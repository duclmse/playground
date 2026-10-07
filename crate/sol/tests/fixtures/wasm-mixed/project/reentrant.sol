function generic(n: i64): i64
    print(n)
    if n == 0 then return 40 end
    return typed(n - 1) + 1
end
function typed(n: i64): i64
    return generic(n) + 1
end
function main(): i64
    return typed(1)
end
