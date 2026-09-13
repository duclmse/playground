-- Coroutine resume/yield round-trip overhead (a fiber context switch each
-- way): a coroutine repeatedly yields a running total back to its resumer.
local co = coroutine.create(function()
  local total = 0
  while true do
    total = total + coroutine.yield(total)
  end
end)

local sum = 0
coroutine.resume(co)
for i = 1, 2000000 do
  local ok, value = coroutine.resume(co, i)
  sum = value
end
print(sum)
