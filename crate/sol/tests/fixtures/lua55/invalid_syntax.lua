-- Each case must fail at compilation, rather than only at execution.
local invalid = { --
'local = 1', --
'if true then', --
'return ... +', --
'local x = "unterminated', --
'local x = [=[unterminated', --
'local x = "\\q"', --
'local x = 0x', --
'local x = 1e+', --
'local x = "\\256"', --
'local x = "\\u{}"', --
'break', --
'goto absent', --
'::x:: ::x::', --
'goto after; local x=1; ::after:: print(x)', --
'return function() return ... end', --
'return 1; print(2)', --
'local x <unknown> = 1', --
'local a <close>, b <close> = nil,nil', --
'local x <close> = nil; x=nil', 'local <const> a,b=1,2; b=3', --
'global <close> x', --
'global none; print(1)', --
'global <const> *; x=1', --
'global x <const>; x=1', --
'for i=1,2 do i=3 end', --
'for k in pairs({}) do k=2 end', --
'return function(...a) a={} end' --
}
for i, source in ipairs(invalid) do
    local f, e = load(source, 'invalid_' .. i, 't')
    assert(f == nil and type(e) == 'string', 'accepted invalid syntax: ' .. source)
end
print('ok: invalid_syntax')
