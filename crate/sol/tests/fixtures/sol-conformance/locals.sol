-- Upstream locals.lua needs <close> (unimplemented; see attrib.sol).
-- Reinterpreted as a pure lexical-scoping exercise: nested `do ... end`
-- blocks each shadow the same name, and each block sees exactly the
-- binding introduced in its own scope (docs/spec/statements-and-expressions.md's
-- "Declarations and blocks").
function main(): i64
    local x: i64 = 1
    local total: i64 = x
    do
        local x: i64 = 2
        total = total + x
        do
            local x: i64 = 3
            total = total + x
        end
        total = total + x
    end
    total = total + x
    return total
end
