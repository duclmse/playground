-- Upstream bwcoercion.lua injects bitwise metamethods onto the shared
-- string metatable (unsupported: Sol has no persistent shared string
-- metatable). Reinterpreted as Sol's actual numeric coercion rule: i64
-- widens implicitly to f64, and no other implicit numeric conversion
-- exists (docs/spec/types-and-values.md's "Inference and conversion").
function main(): i64
    local i: i64 = 7
    local f: f64 = i          -- implicit i64 -> f64 widening
    local g: f64 = f + 0.5
    local h: f64 = 10
    local total: f64 = g + h

    if total == 17.5 then
        return 1
    end
    return 0
end
