-- Typed Sol equivalent of string_concat.lua: deliberately quadratic string
-- concatenation and allocation pressure.
function main(): i64
    local value: string = ""
    for i = 1, 20000 do
        value = value .. "x"
    end
    return #value
end
