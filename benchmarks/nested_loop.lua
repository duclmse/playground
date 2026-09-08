-- Nested loops: 9,000,000 simple increments - a second, larger data point
-- on raw loop/arithmetic throughput independent of `loop_sum`'s single loop.
local count = 0
for i = 1, 3000 do
  for j = 1, 3000 do
    count = count + 1
  end
end
print(count)
