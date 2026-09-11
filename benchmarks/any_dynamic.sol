-- Gradual typing: identical workload to any_strict.sol, but
-- `compute` takes and returns `any` instead of `i64` - every call boxes
-- its argument (a real heap allocation - see value.rs) and unboxes its
-- result (a runtime tag check), the honest cost of opting into gradual
-- typing for this one function, measured against the strict baseline.
function compute(x: any): any
    local y: i64 = x
    local r: i64 = y + 1
    return r
end

function main(): i64
    local total = 0
    for i = 0, 4999999 do
        local boxed: any = i
        local result: i64 = compute(boxed)
        total = total + result
    end
    return total
end
