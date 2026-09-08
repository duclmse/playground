-- Repeated string concatenation: string allocation + GC pressure (each `..`
-- allocates a new, longer string - deliberately naive/quadratic, a classic
-- stress test for a VM's string/GC handling).
local s = ""
for i = 1, 20000 do
  s = s .. "x"
end
print(#s)
