function main(): i64
    local point: { y: i64, x: i64 } = { x = 20, y = 21 }
    local nested: { point: { x: i64, y: i64 }, scale: i64 } = {
        scale = 2,
        point = { y = point.y, x = point.x }
    }
    nested.point.x = nested.point.x + 1
    return nested.point.x + nested.point.y
end
