-- U11 item 2: `triple`'s `any` parameter is a speculative candidate (same
-- shape as `speculative_any_param.sol`'s `double`), but unlike that fixture
-- its only call site passes the argument boxed directly inline
-- (`triple(i)`, not through an intermediate `any`-typed local) - so the
-- whole-program proof in `jit::is_speculative_exhaustive` can show every
-- call site always passes `i64`, and the per-call runtime tag guard in
-- `interp::try_speculative` is skipped entirely once specialized.
function triple(x: any): i64
    local y: i64 = x
    return y * 3
end

function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 60 do
        total = total + triple(i)
        i = i + 1
    end
    return total
end
