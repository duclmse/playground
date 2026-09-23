-- Deliberately keeps `a + b` on two genuinely `any`-typed values (never
-- narrowed to a concrete type before the arithmetic), so codegen must emit
-- a real `sol_dynamic_binary` runtime call - see
-- dumped_ir_legend_resolves_runtime_calls_to_their_real_names, which checks
-- that this call is identifiable by name in --dump-ir output.
function add_any(a: any, b: any): any
    return a + b
end

function main(): i64
    local y: any = 2
    local r: any = 1
    for i = 0, 4 do
        r = add_any(r, y)
    end
    local n: i64 = r
    return n
end
