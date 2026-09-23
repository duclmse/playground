-- Upstream math.lua exercises Lua's math.* library. Typed Sol has no
-- language-level math module - the closest typed capability is binding
-- real libm symbols directly via `extern function`
-- (docs/spec/functions-and-modules.md), the same way
-- crates/sol/tests/fixtures/ffi_libm.sol already does. Reinterpreted as a
-- small extern-FFI-backed arithmetic exercise plus Sol's own core integer
-- operators (// and %, docs/spec/statements-and-expressions.md).
extern function sqrt(x: f64): f64
extern function pow(base: f64, exp: f64): f64

function main(): f64
    local a: f64 = sqrt(144.0)
    local b: f64 = pow(2.0, 6.0)
    local c: i64 = 17 % 5
    local d: i64 = 17 // 5
    return a + b + c + d
end
