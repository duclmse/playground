-- U11 item 2 regression: `narrow`'s `any` parameter is a speculative
-- candidate (unboxes to `i64`), called many times with `i64` arguments to
-- warm it up, then once with an `f64` argument boxed directly at that call
-- site. The mismatched call site must disqualify
-- `jit::is_speculative_exhaustive`'s whole-program proof (so the guard in
-- `interp::try_speculative` stays in place) and the mismatched call must
-- still trap - proving the proof never turns a real mismatch into silent
-- bit-misinterpretation.
function narrow(x: any): i64
    local y: i64 = x
    return y + 1
end

function main(): i64
    local total: i64 = 0
    local i: i64 = 0
    while i < 40 do
        total = total + narrow(i)
        i = i + 1
    end
    return total + narrow(1.5)
end
