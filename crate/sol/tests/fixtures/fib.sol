function fib(n: i64): i64
    if n < 2 then
        return n
    else
        return fib(n - 1) + fib(n - 2)
    end
end

function main(): i64
    return fib(32)
end
