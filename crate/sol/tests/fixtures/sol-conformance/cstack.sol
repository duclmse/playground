-- Upstream cstack.lua measures Lua's C call-stack depth/overflow behavior
-- through the internal `ltests` library. Sol's native code runs on the
-- normal OS stack with no analogous host C-stack instrumentation exposed
-- to typed programs. Not applicable - intentional stub.
function main(): i64
    return 0
end
