-- M7 §25 FFI: `sqrt`/`pow` are real libc/libm symbols, resolved via dlsym
-- (already loaded in any process) rather than sol bytecode/native code.
extern function sqrt(x: f64): f64
extern function pow(base: f64, exp: f64): f64

function main(): f64
    local a = sqrt(144.0)
    local b = pow(2.0, 10.0)
    return a + b
end
