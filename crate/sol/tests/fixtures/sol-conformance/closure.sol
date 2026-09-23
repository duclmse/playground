-- Upstream closure.lua needs _ENV modeling (globals as sugar for indexing
-- an _ENV upvalue table) and weak-table collection, neither implemented.
-- Reinterpreted using Sol's actual typed closure model: nested functions
-- used only in direct calls may capture supported scalar locals by value at
-- each call, so reassigning the captured local in the enclosing function is
-- observed at the *next* call (docs/spec/functions-and-modules.md);
-- assigning to a captured local from inside the closure itself is
-- unsupported and therefore not exercised here.
function main(): i64
    local base: i64 = 10
    local function add_base(x: i64): i64
        return x + base
    end
    local first: i64 = add_base(1)
    base = 20
    local second: i64 = add_base(1)

    local total: i64 = 0
    for index = 1, 3 do
        local function scaled(x: i64): i64
            return x * index
        end
        total = total + scaled(2)
    end

    return first + second + total
end
