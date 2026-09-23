-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local f = assert(io.tmpfile());
assert(io.type(f) == 'file')
assert(f:setvbuf('full'));
assert(f:write('one\ntwo\n') == f);
assert(f:flush())
assert(f:seek('set', 0) == 0);
assert(f:read('l') == 'one');
assert(f:read('a') == 'two\n')
assert(f:seek('set', 0) == 0);
local t = {};
for line in f:lines() do
    t[#t + 1] = line
end
assert(#t == 2)
assert(f:close());
assert(io.type(f) == 'closed file')
assert(type(io.stdin) == 'userdata' and type(io.stdout) == 'userdata' and type(io.stderr) == 'userdata')
assert(type(os.clock()) == 'number' and type(os.time()) == 'number')
assert(os.difftime(20, 10) == 10)
assert(os.date('!%Y-%m-%d', 0) == '1970-01-01')
assert(type(os.date('!*t', 0)) == 'table')
assert(type(os.setlocale(nil)) == 'string')
for _, name in ipairs({'execute', 'exit', 'getenv', 'remove', 'rename', 'tmpname'}) do
    assert(type(os[name]) == 'function', name)
end
print('ok: io_os')
