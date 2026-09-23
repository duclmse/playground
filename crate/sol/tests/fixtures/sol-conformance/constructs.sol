-- Upstream constructs.lua needs `require "debug"` (not implemented).
-- Reinterpreted as a pure control-flow exercise: if/elseif/else, numeric
-- for with an explicit step (ascending and descending, both endpoints
-- inclusive), a break out of a bounded search loop, and a while loop
-- (docs/spec/statements-and-expressions.md's "Control flow").
function classify(n: i64): i64
    if n < 0 then
        return -1
    elseif n == 0 then
        return 0
    else
        return 1
    end
end

function main(): i64
    local total: i64 = 0
    for i = 0, 10, 2 do
        total = total + i
    end
    for i = 10, 0, -2 do
        total = total + i
    end

    local found: i64 = -1
    for i = 0, 99 do
        if i * i > 50 then
            found = i
            break
        end
    end

    local product: i64 = 1
    local i: i64 = 1
    while i <= 5 do
        product = product * i
        i = i + 1
    end

    return total + found + product + classify(-5) + classify(0) + classify(5)
end
