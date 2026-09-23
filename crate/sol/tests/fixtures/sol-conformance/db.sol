-- Upstream db.lua exercises Lua's `debug.*` library (getinfo, sethook,
-- getlocal, traceback, ...). Typed Sol exposes debugging only externally,
-- through the `sol debug` call-boundary CLI debugger (debug.rs) - there is
-- no in-language debug library a typed program can call into itself.
-- Not applicable - intentional stub.
function main(): i64
    return 0
end
