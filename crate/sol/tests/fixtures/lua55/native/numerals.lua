-- Decimal/hex integers and floats, exponents, comments, and wrapping literals.
--[==[ multiline
]=] not the closing delimiter
]==]

local a = 0xffffffffffffffff
local b = 0x1.fp2
local c = .5 + 1. + 1e2 + 2E-1
return a == -1 and b == 7.75 and c == 101.7 and 0x8000000000000000 == -9223372036854775807 - 1
