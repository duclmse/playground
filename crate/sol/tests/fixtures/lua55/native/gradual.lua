function add(a, b)
    return a + b
end
function quotient(a, b)
    return a // b
end
function bits(a, b)
    return a ~ b
end
function equal(a, b)
    return a == b
end
function negative(a)
    return -a
end
function truth(a)
    if a then
        return true
    else
        return false
    end
end
function empty()
end
local sum = add(20, 22)
return sum == 42 and add(1.5, 2) == 3.5 and quotient(-7, 2) == -4 and bits(7, 3) == 4 and equal(nil, nil) and
           not equal(nil, false) and negative(7) == -7 and not truth(nil) and not truth(false) and truth(0) and empty() ==
           nil and (nil or 7) == 7 and (0 and 9) == 9
