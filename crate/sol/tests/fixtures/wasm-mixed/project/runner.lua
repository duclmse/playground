function bump(n: i64): i64
    local value = n + 1
    print(n)
    return value
end
function run(n: i64): i64
    local keep = { value = n }
    local co = coroutine.create(function()
        local value = typed(keep.value)
        coroutine.yield(value)
        return typed(value)
    end)
    local ok, first = coroutine.resume(co)
    assert(ok, first)
    local again, second = coroutine.resume(co)
    assert(again, second)
    assert(coroutine.status(co) == "dead")
    print(first, second)
    return second
end
