-- Regression for the generational GC's write barrier: `h` (a struct) is
-- forced to become an `Old` chunk survivor by the first loop below, then
-- its `data` field is reassigned to a brand-new (`Young`) array - the only
-- reference to that array is this `Old -> Young` edge, which must be
-- recorded by `sol_gc_write_barrier` so a later minor collection's
-- remembered-set scan (not a full trace) still finds it reachable.
struct Holder {
    data: Array<i64>,
}

function main(): i64
    local h = Holder { data = new_array_i64(4) }
    h.data[0] = 1

    for i = 0, 4999 do
        local junk = new_array_i64(100)
        junk[0] = i
    end

    h.data = new_array_i64(4)
    h.data[0] = 111
    h.data[1] = 222
    h.data[2] = 333
    h.data[3] = 444

    for i = 0, 19999 do
        local junk = new_array_i64(100)
        junk[0] = i
    end

    return h.data[0] + h.data[1] + h.data[2] + h.data[3]
end
