-- sol equivalent of fib.lua - same problem size (fib(32)), for a
-- direct comparison of typed/native-compiled recursion against reference
-- Lua/LuaJIT/this repo's dynamic VM.
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
