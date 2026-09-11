-- Table array-part write then read: table/GC allocation throughput.
local t = {}
for i = 1, 4000000 do
  local value = i + 0.0
  t[i] = value * value
end
local sum = 0.0
for i = 1, #t do
  sum = sum + t[i]
end
print(sum)
