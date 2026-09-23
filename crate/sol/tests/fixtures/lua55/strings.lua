-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(('hello'):upper() == 'HELLO' and string.lower('ABC') == 'abc')
assert(string.reverse('abc') == 'cba' and string.sub('abcd', -2) == 'cd')
assert(string.rep('ab', 3, ':') == 'ab:ab:ab')
assert(string.char(0, 65, 255) == '\0A\255' and string.byte('ABC', 2) == 66)
assert(string.format('%04d %s %.1f', 7, 'x', 1.25) == '0007 x 1.2')
local a, b = string.find('a.b', '. ', 1, true);
assert(a == nil)
a, b = string.find('a.b', '.', 1, true);
assert(a == 2 and b == 2)
assert(string.match('key=123', '(%d+)') == '123')
assert(string.match('(a(b)c)', '%b()') == '(a(b)c)')
assert(string.match('hello world', '%f[%a]world') == 'world')
local t = {};
for x in string.gmatch('a,b,c', '[^,]+') do
    t[#t + 1] = x
end
assert(table.concat(t) == 'abc')
local s, n = string.gsub('a1 b2', '(%a)(%d)', '%2%1');
assert(s == '1a 2b' and n == 2)
assert(string.gsub('a b', '%a', {
    a = 'A',
    b = 'B'
}) == 'A B')
assert(string.gsub('12', '%d', function(x)
    return tonumber(x) + 1
end) == '23')
local packed = string.pack('<i4I2z', -42, 65535, 'abc')
local x, y, z, pos = string.unpack('<i4I2z', packed)
assert(x == -42 and y == 65535 and z == 'abc' and pos == #packed + 1)
assert(string.packsize('<i4I2') == 6)
print('ok: strings')
