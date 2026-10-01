local function may_fail(x)
    if x < 0 then
        error("negative")
    end
    return x * 2
end

local function safe_call(x)
    local ok, result = pcall(may_fail, x)
    if ok then
        return result
    else
        return -1
    end
end

print(safe_call(5))
print(safe_call(-5))
