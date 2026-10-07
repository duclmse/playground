import runner
function typed(n: i64): i64
    local value = runner.bump(n)
    return value + 1
end
function main(): i64
    local base: i64 = 1
    return runner.run(40) + base
end
