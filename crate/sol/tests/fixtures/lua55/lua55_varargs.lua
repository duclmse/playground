-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local function named(a,...args)
 assert(a==10 and args.n==3 and args[1]==20 and args[2]==nil and args[3]==40)
 args[1]=21; args.n=2
 return ...
end
local t=table.pack(named(10,20,nil,40)); assert(t.n==2 and t[1]==21 and t[2]==nil)
local function capture(...args) return function() return args end end
local a=capture(1,nil,3)(); assert(a.n==3 and a[3]==3)
assert(load('return function(...args) args={} end')==nil)
print('ok: lua55_varargs')
