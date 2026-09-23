-- `square` is small and non-recursive - eligible for inlining
-- (faster_lua.md §7) at every one of its three call sites below. Each
-- call site needs its own fresh set of Cranelift variables for `square`'s
-- parameter/locals (`inline_call` in codegen.rs) - calling it multiple
-- times exercises that repeated-inlining path, not just a single call site.
function square(x: i64): i64
    local result = x * x
    return result
end

function main(): i64
    return square(2) + square(3) + square(4)
end
