-- Upstream literals.lua needs `require "debug"` and also exercises Lua's
-- hex/hex-float numeral forms, which are Lua-mode only (typed Sol accepts
-- decimal numeric literals only; docs/spec/source-and-lexical-grammar.md).
-- Reinterpreted as a decimal-literal edge-case exercise: the i64 boundary
-- value, wrap-on-overflow arithmetic at that boundary
-- (docs/spec/statements-and-expressions.md's "Operators"), and a string
-- literal escape.
function main(): i64
    local max_i64: i64 = 9223372036854775807
    local wrapped: i64 = max_i64 + 1  -- wraps to i64::MIN
    local back: i64 = wrapped - 1     -- wraps back to i64::MAX

    local greeting: string = "hi\n"
    local length: i64 = #greeting

    if back == max_i64 then
        return length
    end
    return -1
end
