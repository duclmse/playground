-- Upstream files.lua exercises Lua's io/os file, locale, and `/dev/full`
-- behavior. Typed Sol has no io/os library surface at all - the only
-- native-boundary mechanism is `extern function` over scalar i64/f64/bool,
-- which is not a filesystem API. Not applicable - intentional stub.
function main(): i64
    return 0
end
