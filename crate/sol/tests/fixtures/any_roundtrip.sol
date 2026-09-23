-- Gradual typing: the smallest useful round-trip fixture for gradual
-- typing - `identity` is a fully dynamic function (`any` param and
-- return), `main` is fully strict. Calling it boxes `main`'s i64 argument
-- at the call boundary, and unboxes the result back to i64 at the
-- assignment - both runtime type checks, both succeeding here since the
-- boxed value really is an i64.
function identity(x: any): any
    return x
end

function main(): i64
    local y: any = 42
    local z: i64 = identity(y)
    return z
end
