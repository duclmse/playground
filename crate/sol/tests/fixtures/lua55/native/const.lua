local a <const> = 20
local <const> b = 22
local total=0
for i=1,3 do
 local i=10 -- a fresh mutable local shadows the read-only control variable
 i=i+1
 total=total+i
end
do local a=1; a=2; total=total+a end
return a+b==42 and total==35
