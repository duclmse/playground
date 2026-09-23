-- Upstream bitwise.lua needs cross-file `require "bwcoercion"` / `require
-- 'bit32'`, which need filesystem-backed require across files (a stated
-- non-goal). Reinterpreted as a direct exercise of Sol's own native i64
-- bitwise operators (&, |, ~ binary/unary, <<, >>), which the language
-- supports natively without any library at all
-- (docs/spec/statements-and-expressions.md's operator precedence table).
function main(): i64
    local a: i64 = 240  -- 0xF0
    local b: i64 = 12   -- 0x0C
    local c: i64 = 5

    local and_result: i64 = a & b
    local or_result: i64 = a | b
    local xor_result: i64 = a ~ b
    local not_c: i64 = ~c
    local shl_result: i64 = c << 2
    local shr_result: i64 = a >> 4

    return and_result + or_result + xor_result + not_c + shl_result + shr_result
end
