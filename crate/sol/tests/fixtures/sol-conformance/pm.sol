-- Upstream pm.lua exercises Lua's pattern-matching library (string.find/
-- match/gmatch/gsub, utf8.charpattern) plus the package/require loader.
-- Pattern matching exists in this crate only for dynamic `.lua` code
-- (lua_pattern.rs) - typed Sol's `string` has no pattern operations at all,
-- and typed Sol has no filesystem-backed module loader beyond its own
-- `import` (docs/spec/functions-and-modules.md). Not applicable -
-- intentional stub.
function main(): i64
    return 0
end
