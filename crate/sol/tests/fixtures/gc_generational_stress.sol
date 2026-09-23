-- Combined generational stress test: `h.data` is repeatedly replaced with
-- a fresh (`Young`) array every 1000 iterations (each replacement exercises
-- the write barrier once `h` has been promoted to `Old`), interleaved with
-- enough short-lived garbage to force many minor collections and, given
-- this collector's coarse chunk-granularity promotion, at least one major
-- collection (`collect_heap`) as well - see gc.rs's module doc comment.
struct Holder {
    data: Array<i64>,
}

function main(): i64
    local h = Holder { data = new_array_i64(4) }
    h.data[0] = 0

    local i = 0
    while i < 200000 do
        local junk = new_array_i64(64)
        junk[0] = i
        if i % 1000 == 0 then
            h.data = new_array_i64(4)
            h.data[0] = i
        end
        i = i + 1
    end

    return h.data[0]
end
