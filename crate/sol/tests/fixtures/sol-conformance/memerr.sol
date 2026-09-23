-- Upstream memerr.lua injects allocator failures through Lua's internal
-- `ltests` C API to test out-of-memory error paths. Sol's allocator
-- (gc.rs's chunked bump arena) has no fault-injection hook exposed to typed
-- programs, and typed allocation failures are fatal traps rather than
-- catchable errors (docs/spec/execution-and-runtime.md). Not applicable -
-- intentional stub.
function main(): i64
    return 0
end
