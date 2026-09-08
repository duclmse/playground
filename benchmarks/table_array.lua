-- Table array-part write then read: table/GC allocation throughput.
local t = {}
for i = 1, 4000000 do
  t[i] = i * i
end
local sum = 0
for i = 1, #t do
  sum = sum + t[i]
end
print(sum)
