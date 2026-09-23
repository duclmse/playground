-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
-- Comments, all numeral forms, byte strings, long delimiters, escapes.
--[==[ a nested-looking ]=] comment ]==]

assert(0xff == 255 and 0x1.fp2 == 7.75 and .5 == 0.5 and 1e2 == 100)
assert([==[
hello ]=] world]==] == 'hello ]=] world')
assert('\x41\065\u{42}\z  \nC' == 'AAB\nC')
assert(#'\0\255' == 2)
assert('\a\b\f\n\r\t\v' == string.char(7,8,12,10,13,9,11))
print('ok: lexical')
