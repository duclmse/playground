function compute(value: i64): i64
    local first = require("dynamic_math")
    local second = require("dynamic_math")
    assert(first == second)
    return first.answer(value)
end
