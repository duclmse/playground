-- M8: small, distinct-named calls for debug.rs's REPL tests.
function add(a: i64, b: i64): i64
    return a + b
end

function main(): i64
    local x = add(1, 2)
    local y = add(x, 10)
    return y
end
