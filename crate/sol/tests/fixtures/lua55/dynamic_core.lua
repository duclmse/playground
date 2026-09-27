-- Supported dynamic compatibility profile: `_ENV` globals, tables, closures,
-- multi-result calls, iteration, and protected errors.
--
-- Lua 5.5's `global` declaration makes every *ordinary* global access in the
-- chunk strict (real Lua's `buildglobal`/`singlevaraux`: once any `global` is
-- declared, an undeclared name resolving as `VGLOBAL` is a compile error,
-- "variable 'NAME' not declared" - confirmed against the pinned oracle
-- lua5.5.1 binary), so every stdlib global this fixture reads must be
-- forward-declared too, not just `seed`.
global ipairs
global pcall
global error
global seed = 40
local state = { seed + 2, name = "dynamic" }
local function add(base)
    return function(extra) return base + extra end
end
local total = 0
for _, value in ipairs({1, 2, 3}) do total = total + value end
-- `error(msg, 0)`: level 0 suppresses the default position-info prefix real
-- Lua's `error` would otherwise add (`luaL_where`), which the exact-string
-- comparison below does not expect.
local ok, message = pcall(function() error("expected", 0) end)
return state[1] == 42 and add(2)(seed) == 42 and total == 6 and not ok and message == "expected"
