-- Upstream utf8.lua exercises Lua's utf8.* library (utf8.char, utf8.len,
-- utf8.charpattern, ...), none of which exist at the typed-Sol level.
-- Typed Sol treats `string` as an opaque byte sequence rather than a
-- Unicode codepoint sequence - `#` is always a byte length, and source/
-- string-literal bytes need not be valid UTF-8 at all
-- (docs/spec/source-and-lexical-grammar.md). Reinterpreted as a direct
-- demonstration of that byte-oriented invariant: a 3-byte UTF-8-encoded
-- codepoint has byte length 3, not 1.
function main(): i64
    local snowman: string = "\xe2\x98\x83"  -- U+2603 SNOWMAN, 3 UTF-8 bytes
    return #snowman
end
