type Count = i64
type Dynamic = any

struct Point {
    value: Count
}

function read(value: Dynamic): Count
    if value is Point and value.value > 0 then
        return value.value
    end
    return 0
end

function add_one(value: i64): i64
    return value + 1
end

function main(): Count
    local point: Point = Point { value = 1 }
    local boxed: Dynamic = point
    local same: Point = boxed as Point
    same.value = 42

    local array: Array<i64> = { 1 }
    local boxed_array: any = array
    local same_array: Array<i64> = boxed_array as Array<i64>
    same_array[0] = 3

    local values: Map<i64, i64> = { [1] = 1 }
    local boxed_map: any = values
    local same_map: Map<i64, i64> = boxed_map as Map<i64, i64>
    same_map[1] = 4

    local callback: fn(i64) -> i64 = add_one
    local boxed_callback: any = callback
    local same_callback: fn(i64) -> i64 = boxed_callback as fn(i64) -> i64
    return read(boxed) + point.value + array[0] + values[1] + same_callback(4)
end
