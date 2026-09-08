-- Tight numeric loop: raw interpreter dispatch/arithmetic throughput.
local sum = 0
for i = 1, 20000000 do
  sum = sum + i
end
print(sum)
