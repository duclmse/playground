-- M5: unboxing an `any` value as the wrong type must trap (a real crash,
-- not silently reading garbage or a normal exit(1) error) - the runtime
-- type check `coerce`'s `TExprKind::Unbox` compiles to, mirroring how
-- array bounds checks already trap on a provably-invalid access.
function main(): f64
    local y: any = 42
    local z: f64 = y
    return z
end
