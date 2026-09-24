# Call-site name resolution

> Status: design only - not yet implemented. Written to unblock two U6 Lua
> 5.5 corpus files (`errors.lua`, `db.lua`); no code in this document has
> landed.

**Purpose**: real Lua's error messages and `debug.getinfo` both answer the
same question - "how did the caller refer to this value?" - by walking the
*caller's* bytecode backward from the call site (`ldebug.c`'s `getobjname`/
`funcnamefromcode`). Sol already has a scaled-down `getobjname` equivalent
(`dispatch.rs`'s `describe_register`), but it is wired into exactly two
error-annotation call sites and nowhere else. `debug.getinfo`'s `name`/
`namewhat` fields - needed by `lua-5.5.1-tests/db.lua` - currently fall back
to the callee's own declared name (or `nil`/`""`), which is a different and
strictly weaker signal than what the caller's bytecode actually says. This
document scopes the work to extend the existing machinery to cover that
case, rather than building a second, parallel resolver.

**Prerequisite**: none beyond what already exists - `dispatch.rs`'s
`describe_register`/`instr_writes`, and `LuaFrame`'s existing
`pending: Pending` field (`frame.rs`), which already records the exact
inputs this needs (see below).

## What real Lua does

`ldebug.c`'s `funcnamefromcode` inspects the *calling* frame's currently
executing instruction (an `OP_CALL`/`OP_TAILCALL`) to find which register
held the callee, then defers to `getobjname` to resolve how that register's
value was last written: a global load, a local slot, an upvalue, a table
field, or a method-call self-materialization (`OP_SELF`). The result is a
`(namewhat, name)` pair - `"global"`, `"local"`, `"upvalue"`, `"field"`,
`"method"`, or (falling back) `""`/`NULL`. `lua_getinfo`'s `'n'` option and
every "attempt to call/index a ... value" error message both go through
this same resolution; `debug.getinfo(level)`'s `name`/`namewhat` describe
how the function *at that level* was referenced by its caller (level `N`'s
name is resolved from level `N + 1`'s bytecode, not from the callee's own
prototype).

## What Sol already has

`dispatch.rs`'s `describe_register(proto: &Proto, pc: usize, reg: Reg) ->
Option<String>` (lines 78-153) is explicitly Sol's `getobjname`: it scans
`proto.instrs[..pc]` backward from a given program point, chasing `Move`
the way real Lua's scan does, and recognizes:

- `NewLocal` -> `"local 'x'"`
- `GetUpval` -> `"upvalue 'x'"`
- `GetGlobal` -> `"global 'x'"`
- `GetField` -> `"field 'x'"`, or `"method 'x'"` when immediately followed
  by the `Move(base + 1, receiver)` self-materialization that
  `compile_method_base` (`lua_bytecode/compile_calls.rs:178-200`) emits for
  `recv:method(...)` calls
- dotted receivers (`aaa.bbb.ccc`) via a `dotted_field` accumulator that
  walks the self-overwriting `GetField` chain back to its root

It gives up (`None`) at the first write it can't name, matching real Lua's
own conservative fallback rather than guessing.

This is wired into exactly two call sites, both in `dispatch.rs`:

- `annotate_index_error` (159-165): appends `" (kind 'name')"` to an
  "attempt to index a ... value" error.
- `annotate_call_error` (170-176): the same, for "attempt to call a ...
  value".

`natives_debug.rs`'s `NativeFunction::DebugGetinfo` (lines 224-365) does
**not** call `describe_register` at all. Its `name`/`namewhat` for a
Lua-frame level (354-357) come only from `Self::declared_lua_name(&lua_frame
.proto)` - the callee's own declared name, hardcoded to `namewhat = "local"`
- with `nil`/`""` otherwise (611-612, 649-650). This is documented in-line
as a known gap (241-251): real Lua's call-site resolution "walks the
*caller's* bytecode at the call site," which this runtime does not yet do.

## The two gaps

### 1. `debug.getinfo`'s `name`/`namewhat` (blocks `db.lua`)

`db.lua` (lines 87-92) requires, for a level-2 lookup from inside `x`:

```lua
local g = {x = function ()
  local a = debug.getinfo(2)
  assert(a.name == 'f' and a.namewhat == 'local')  -- how f's CALLER named it
  a = debug.getinfo(1)
  assert(a.name == 'x' and a.namewhat == 'field')  -- how x's CALLER named it
  ...
end}
local f = function () return 1+1 and (not 1 or g.x()) end
```

`f` is a local variable holding an anonymous closure - `declared_lua_name`
has nothing to report for it (it isn't a named `function f() end`
declaration), so today this returns `nil`/`""` instead of `'f'`/`'local'`.
The fix is call-site resolution, not a better guess at the callee's own
name.

**Feasibility**: everything `describe_register` needs is already sitting on
the caller's frame at the moment it issues a call. `dispatch.rs:870-875`
(the `StepResult::CallLeaf` handling) already reads exactly this pair to
compute a call-site source location for error reporting:

```rust
let call_pc = frame.header.pc as usize;
let call_base = match frame.pending {
    Pending::Call { base, .. } => Some(base as Reg),
    ...
};
```

`frame.pending` stays `Pending::Call { base, .. }` on the caller's
`LuaFrame` for the callee's entire execution (that is how the trampoline
knows what to do when the callee returns), so this is available lazily,
not just at call-issue time. For `debug.getinfo(level)`:

1. Resolve `level` to a frame index the way the existing code already does
   (`natives_debug.rs:313-324`, walking `self.frames.iter().rev()`).
2. Look at the frame *below* that one (index `- 1`). If it is `Frame::Lua`
   and its `pending` is `Pending::Call { base, .. }`, call
   `describe_register(&caller.proto, caller.header.pc as usize, base)`.
3. Split the resulting `"{kind} '{name}'"` string into `namewhat`/`name`
   instead of formatting it into a message suffix.

No new state needs to be threaded through `LuaFrame` or `Pending` - the
caller's `proto`, `header.pc`, and `pending.base` are already exactly the
three inputs `describe_register` takes elsewhere. The only prerequisite
change is giving `describe_register` a structured return type (see
"Implementation plan" below) so both the existing error-message callers and
the new `debug.getinfo` path can consume it without going through a
formatted string.

The base frame (level equal to the number of frames, i.e. no caller below
it) legitimately has no call-site name, matching real Lua's own top-level
`nil`/`""` for the outermost chunk.

### 2. `errors.lua` line 331's method-vs-field degradation

```lua
checkmessage(s.."; local t = {}; t:bbb()", "field 'bbb'")
```

After `s` accumulates enough preceding assignments, real Lua's `OP_SELF`
can't fit the method-name constant's index into that instruction's fixed
bit width, so `luaK_self` falls back to a slower non-`OP_SELF` encoding
(`GETFIELD` + a separate call) - and `getobjname` correctly reports that
fallback shape as `"field 'bbb'"` rather than `"method 'bbb'"`. This is a
consequence of real Lua's specific 8-bit RK-embedded-constant instruction
encoding, not a general semantic Sol needs to replicate.

Sol's own bytecode has no equivalent limit: `compile_method_base`
(`compile_calls.rs:178-200`) always emits `GetField(base, recv, name)`
immediately followed by `Move(base + 1, recv)`, unconditionally, regardless
of how many locals or constants are in scope - `name` is a full separate
operand (`push_name_const`), never packed into a fixed-width instruction
field. There is no register/constant-pressure threshold in Sol's ISA for
`describe_register`'s `is_method` check to degrade at, because there is
nothing analogous to fall back to.

**Recommendation: explicitly out of scope.** Replicating this exact
divergence would mean inventing an artificial, Sol-specific "pretend RK
limit" with no grounding in Sol's own instruction format, purely to match a
side effect of real Lua's encoding width. `errors.lua`'s manifest entry
should stay `pending` with this documented as a real, permanent
`.lua`-corpus divergence once the `debug.getinfo` gap above is closed and
this becomes the file's sole remaining blocker - not something to chase
further.

## Implementation plan

- [ ] Change `describe_register`'s return type from `Option<String>` to a
      structured `Option<(&'static str, String)>` (`namewhat`, `name`), or
      an equivalent small enum/struct. Update `annotate_index_error`/
      `annotate_call_error` to format `"{kind} '{name}'"` themselves from
      the structured result - no behavior change at those two call sites.
- [ ] Add a helper (e.g. `LuaRuntime::call_site_name(&self, frame_index:
      usize) -> Option<(&'static str, String)>`) that, given the index of a
      frame in `self.frames`, looks at `self.frames[frame_index - 1]`,
      matches `Frame::Lua` with `pending: Pending::Call { base, .. }`, and
      calls `describe_register` on the caller's `proto`/`header.pc`/`base`.
      Returns `None` for a missing caller, a non-`Lua` caller, or a
      caller whose `pending` isn't `Pending::Call` (e.g. resumed after a
      metamethod dispatch mid-instruction - real Lua reports `nil` there
      too via its own `"metamethod"`/unresolved fallback path, which this
      first pass does not need to distinguish).
- [ ] Wire that helper into `natives_debug.rs`'s `DebugGetinfo` level-based
      branch (currently lines 325-359): prefer the call-site result over
      `declared_lua_name`'s fallback when present, keep the existing
      `declared_lua_name` fallback for frames with no resolvable caller
      (e.g. level equals the frame count, or a coroutine's base frame).
- [ ] Regression coverage in `crate/sol/tests/lua55.rs`: a focused test
      exercising `debug.getinfo(2).name`/`namewhat` for a local-variable-
      held anonymous closure and for a field-held one (mirroring
      `db.lua`'s two assertions at lines 88 and 91), independent of the
      full corpus file.
- [ ] Update `tests/lua55/manifest.toml`'s `db.lua` entry and
      `docs/features/unified-sol-runtime-plan.md`'s U6 narrative together
      once the file's actual stopping point (promoted or newly pending
      elsewhere) is confirmed by rerunning it against the pinned oracle -
      `db.lua` is a large file and line 91 is very unlikely to be its only
      remaining blocker.

**Explicitly out of scope**: the RK-limit method/field degradation
(gap 2 above, permanent divergence); real Lua's `"metamethod"`/`upvalue`-
via-`_ENV` namewhat refinements beyond what `describe_register` already
covers; `namewhat = "constant"` (real Lua reports this for a few
`OP_GETTABUP`/constant-folded cases `describe_register` doesn't model);
resolving names across tail calls (`Pending::TailCall` carries no callee
register to resolve from, since a tail call reuses the caller's own frame
rather than leaving one behind to inspect).

**Files**: `crate/sol/src/lua_runtime/dispatch.rs` (`describe_register`,
`annotate_index_error`, `annotate_call_error`), `crate/sol/src/lua_runtime/
natives_debug.rs` (`NativeFunction::DebugGetinfo`), `crate/sol/src/
lua_runtime/frame.rs` (`Pending::Call`, read-only - no changes needed),
`crate/sol/src/lua_bytecode/compile_calls.rs` (`compile_method_base`,
referenced only, not changed).
