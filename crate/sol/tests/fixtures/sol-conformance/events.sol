-- Upstream events.lua exercises Lua metatables/metamethods (__index,
-- __add, __call, ...). Typed Sol has no operator-overloading/metamethod
-- facility at all (docs/spec/types-and-values.md defines no such hook).
-- Reinterpreted as the closest typed analogue: choosing between distinct
-- stateless function-value "handlers" by an integer tag, plus `any`+`is`
-- narrowing standing in for the dynamic-dispatch layer metatables provide
-- in Lua.
function on_add(a: i64, b: i64): i64
    return a + b
end

function on_mul(a: i64, b: i64): i64
    return a * b
end

function dispatch(event: i64, a: i64, b: i64): i64
    local handler: fn(i64, i64) -> i64 = on_add
    if event == 1 then
        handler = on_mul
    end
    return handler(a, b)
end

function main(): i64
    local sum_result: i64 = dispatch(0, 3, 4)
    local mul_result: i64 = dispatch(1, 3, 4)

    local boxed: any = sum_result
    local recovered: i64 = 0
    if boxed is i64 then
        recovered = boxed  -- narrowed to i64 in this branch; no `as` needed
    end

    return sum_result + mul_result + recovered
end
