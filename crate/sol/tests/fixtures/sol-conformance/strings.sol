-- Upstream strings.lua exercises Lua's full string library (patterns,
-- string.rep, locale-sensitive comparison, etc). Typed Sol only has the
-- operators the language itself defines directly on `string`: `..`
-- concatenation, `#` length, and `==`/`<` comparison
-- (docs/spec/statements-and-expressions.md) - no pattern matching or
-- library functions at the typed level (that surface is `.lua`-mode only,
-- via lua_pattern.rs). Reinterpreted as an exercise of exactly those typed
-- string operators.
function main(): i64
    local a: string = "sol"
    local b: string = "lang"
    local combined: string = a .. b
    local length: i64 = #combined

    local equal_check: i64 = 0
    if a == "sol" then
        equal_check = 1
    end

    local order_check: i64 = 0
    if a < b then
        order_check = 1
    end

    return length + equal_check + order_check
end
