-- Memory management: a real GC reclaim test, not just "doesn't
-- crash". `keep` is a single array allocated once, before the loop, and
-- read again only at the end - it must survive every collection the loop
-- below triggers. `junk` is redefined every iteration: each iteration's
-- previous array becomes immediately unreachable, generating garbage on
-- purpose. 20000 iterations * (16-byte header + 800-byte data) far exceeds
-- gc.rs's 64 KiB collection threshold many times over, forcing multiple
-- real mark-sweep cycles - if `keep`'s root isn't found correctly by the
-- conservative stack scan, it gets collected out from under `main` and
-- this returns garbage (or traps) instead of 1110.
function main(): i64
    local keep = new_array_i64(4)
    keep[0] = 111
    keep[1] = 222
    keep[2] = 333
    keep[3] = 444

    for i = 0, 19999 do
        local junk = new_array_i64(100)
        junk[0] = i
    end

    return keep[0] + keep[1] + keep[2] + keep[3]
end
