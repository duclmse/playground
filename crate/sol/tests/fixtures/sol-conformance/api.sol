-- Upstream api.lua exercises Lua's embeddable C API (lua_push*, lua_call,
-- lua_gettable, ...) directly through the internal `ltests` test library.
-- Typed Sol's only native-boundary surface is `extern function`, which
-- crosses scalar i64/f64/bool values one call at a time - there is no
-- embeddable host API to drive Sol's stack/registry the way the C API
-- drives Lua's. Not applicable - intentional stub.
function main(): i64
    return 0
end
