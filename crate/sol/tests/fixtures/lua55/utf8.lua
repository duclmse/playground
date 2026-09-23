-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local s = utf8.char(65, 0x20ac, 0x1f600)
assert(utf8.len(s) == 3 and utf8.codepoint(s, 2) == 0x20ac)
local start, finish = utf8.offset(s, 2);
assert(start == 2 and finish == 4)
assert(utf8.offset(s, -1) == 5)
local cps = {};
for p, c in utf8.codes(s) do
    cps[#cps + 1] = c
end
assert(cps[3] == 0x1f600)
local n, bad = utf8.len('\255');
assert(n == nil and bad == 1)
assert(type(utf8.charpattern) == 'string')
print('ok: utf8')
