-- Upstream coroutine.lua exercises Lua's coroutine.create/resume/yield/
-- wrap/status family. That family is implemented only for dynamic `.lua`
-- code (lua_runtime.rs, stackful fibers via corosensei) - typed Sol's
-- function model has no suspend/resume primitive at all, and coroutines
-- are not part of the typed language surface (docs/spec/types-and-values.md
-- lists no coroutine type). Not applicable - intentional stub.
function main(): i64
    return 0
end
