-- Upstream attrib.lua exercises Lua's <const>/<close> local attributes and
-- a dynamic any-typed callback crossing the native boundary. Typed Sol has
-- <close> unimplemented (parser.rs::parse_attribute rejects it outright)
-- and no any-across-native-FFI-boundary support, but it does support
-- <const> locals. Reinterpreted as: a <const> local is read-only and usable
-- exactly like any other local (the rejection of writes to it is a
-- compile-time typeck error, not something a runnable program can exercise
-- here; see typeck.rs's "cannot assign to read-only variable" check).
function main(): i64
    local limit <const> = 100
    local total: i64 = 0
    for i = 1, limit do
        total = total + i
    end
    return total
end
