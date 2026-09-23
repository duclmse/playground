extern fn labs(value: i64): i64

export fn twice(value: i64): i64
    return value * 2
end

fn main(): i64
    local offset: i64 = 1
    local fn add_offset(value: i64): i64
        return value + offset
    end
    local callback: fn(i64) -> i64 = twice
    return callback(20) + add_offset(0) + labs(-1)
end
