local function size(value)
    if type(value) == "string" then
        return #value
    end
    return 0
end

return size("sol")
