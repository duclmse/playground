function first()
    return second()
end

function second()
    return 42
end

return first() == 42
