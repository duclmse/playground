-- Upstream goto.lua exercises Lua's goto/label control flow and <close>
-- variables. Neither exists in typed Sol: goto/labels are accepted only in
-- Lua-compatibility mode (docs/spec/source-and-lexical-grammar.md), and
-- <close> is unimplemented (see attrib.sol). Reinterpreted using the
-- structured control flow typed Sol actually has (nested while/return) to
-- express the same "exit nested work early" intent goto is normally used
-- for.
function find_first_pair_summing_to(target: i64, limit: i64): i64
    local i: i64 = 0
    while i < limit do
        local j: i64 = i + 1
        while j < limit do
            if i + j == target then
                return i * 1000 + j
            end
            j = j + 1
        end
        i = i + 1
    end
    return -1
end

function main(): i64
    return find_first_pair_summing_to(10, 20)
end
