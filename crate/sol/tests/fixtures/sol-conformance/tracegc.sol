-- Upstream tracegc.lua loads the distribution's native `tracegc` C module
-- to trace collector internals from outside the process. Sol has no
-- native-module loading facility for typed programs, and no external
-- collector-tracing hook. Not applicable - intentional stub.
function main(): i64
    return 0
end
