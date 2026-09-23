-- Upstream gengc.lua needs `require "debug"` and specifically targets the
-- generational collector's generation-crossing behavior. Reinterpreted as a
-- direct regression for Sol's own generational collector (crates/sol/src/
-- gc.rs): a struct that has been promoted to the old generation has its
-- array field replaced by a fresh young array, and the write barrier
-- (sol_gc_write_barrier) must keep that new array reachable across later
-- minor collections' remembered-set scans. See
-- crates/sol/tests/fixtures/gc_struct_field_write_barrier.sol for the more
-- detailed version of this same regression.
struct Holder {
    data: Array<i64>,
}

function main(): i64
    local h = Holder { data = new_array_i64(2) }
    h.data[0] = 1

    for i = 0, 4999 do
        local junk = new_array_i64(50)
        junk[0] = i
    end

    h.data = new_array_i64(2)
    h.data[0] = 99
    h.data[1] = 1

    for i = 0, 9999 do
        local junk = new_array_i64(50)
        junk[0] = i
    end

    return h.data[0] + h.data[1]
end
