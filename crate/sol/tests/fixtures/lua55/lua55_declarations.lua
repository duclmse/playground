-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(_VERSION=='Lua 5.5')
do
 global assert, answer
 answer=42
 assert(answer==42)
end
do
 global assert, load
 assert(load('global print; return undeclared')==nil)
end
do
 global *
 local <const> a,b=2,3
 assert(a+b==5)
 global lua55_initialized = 10
 assert(lua55_initialized==10)
 global function lua55_function() return 9 end
 assert(lua55_function()==9)
end
assert(load('local x <const> = 1; x=2')==nil)
assert(load('for i=1,3 do i=2 end')==nil)
assert(load('for k,v in pairs({}) do k=2 end')==nil)
print('ok: lua55_declarations')
