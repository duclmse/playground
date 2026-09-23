function increment(value: i64): i64
  return value + 1
end

function apply_twice(callback: fn(i64) -> i64, value: i64): i64
  return callback(callback(value))
end

function main(): i64
  local callback: fn(i64) -> i64 = increment
  return apply_twice(callback, 40)
end
