local a = -7
local b = 2
local shifts = -65
local min = 0x8000000000000000

return a // b == -4 and a % b == 1 and a / b == -3.5 --
and 7 % -2 == -1 and -7 % -2 == -1 --
and min // -1 == min and min % -1 == 0 --
and (0xff & 3) == 3 and (8 | 3) == 11 and (7 ~ 3) == 4 --
and ~0 == -1 and (1 << 4) == 16 and (-1 >> 63) == 1 --
and (1 << shifts) == 0 and (8 << -1) == 4 and (1 >> min) == 0 --
and -2 ^ 2 == -4 and 2 ^ 3 ^ 2 == 512 and 2 ^ -2 == 0.25 --
and -7.5 % 2.0 == 0.5 and -7.5 // 2.0 == -4.0
