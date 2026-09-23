-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
assert(2 + 3 * 4 == 14 and -2 ^ 2 == -4 and 2 ^ 3 ^ 2 == 512)
assert(7 / 2 == 3.5 and -7 // 2 == -4 and -7 % 2 == 1)
assert((0xf & 3) == 3 and (8 | 3) == 11 and (7 ~ 3) == 4)
assert(~0 == -1 and (1 << 4) == 16 and (16 >> 2) == 4)
assert((-1 >> 63) == 1 and (1 << 64) == 0 and (8 << -1) == 4)
assert(math.maxinteger + 1 == math.mininteger)
assert((false or 'x') == 'x' and (0 and 'y') == 'y' and not nil)
local n = 0;
local function bump()
    n = n + 1;
    return true
end
assert(true or bump());
assert(not (false and bump()));
assert(n == 0)
assert('a' .. 1 .. 'b' == 'a1b' and #'abc' == 3)
assert(1 == 1.0 and 1 ~= 2 and 1 < 2 and 2 <= 2 and 3 > 2 and 3 >= 3)
print('ok: operators')
