function main(): i64
    local base: i64 = 2
    local function sum_to(value: i64): i64
        if value == 0 then
            return base
        end
        return value + sum_to(value - 1)
    end
    base = 3

    local total: i64 = 0
    for index = 1, 3 do
        local function add_index(value: i64): i64
            return value + index
        end
        total = total + add_index(10)
    end
    local function double(value: i64): i64
        return value * 2
    end
    local callback: fn(i64) -> i64 = double

    local root: i64 = 5
    local function outer(value: i64): i64
        local function inner(extra: i64): i64
            return root + value + extra
        end
        return inner(2)
    end
    return sum_to(5) + total + callback(3) + outer(3)
end
