-- Upstream gc.lua needs `require "debug"` and further exercises precise
-- roots, finalizers, and weak-table collection - all still-deferred work
-- (docs/features/memory-management.md). Reinterpreted as a direct exercise
-- of Sol's actual mark/sweep collector: a live struct reference must
-- survive many minor collections' worth of short-lived garbage. See
-- crates/sol/tests/programs.rs's gc_* fixtures for the fuller version of
-- this same property (including the generational-specific case, ported
-- separately as gengc.sol in this suite).
struct Cell {
    value: i64,
}

function main(): i64
    local live = Cell { value = 7 }
    local i: i64 = 0
    while i < 50000 do
        local junk = new_array_i64(32)
        junk[0] = i
        i = i + 1
    end
    return live.value
end
