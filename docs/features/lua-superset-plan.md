# Execution plan: Sol as a Lua 5.5 superset, faster than LuaJIT

This is the concrete, sequenced execution plan for closing the gaps found in
the 2026-09 review of `crates/sol`. It complements
[`lua-compatibility.md`](lua-compatibility.md)'s L0–L8 checklist (which
states *what* is required and *why*) with *what to build next, in what
order, touching which files* — so each phase is independently landable and
testable. Update the L-level checklist item together with each phase, per
this repo's standing discipline of keeping behavior and status docs in sync.

## Baseline (as of this plan)

- **Compatibility:** 0/34 upstream Lua 5.5.1 corpus files pass (8
  `host-required`, 26 `pending`). Coroutines, `load`/`loadstring`/`dofile`,
  `io`/`os`, GC finalization/weak tables, and `__close` are entirely
  unimplemented.
- **Performance:** the only "beats LuaJIT" evidence is two typed `.sol`
  microbenchmarks (`fib`, `table_array`) at 1.15–1.30×. There is no benchmark
  for the dynamic `.lua` path, and the per-function `check_partitioned` split
  means most real Lua programs (anything using tables/closures pervasively)
  run through the bytecode interpreter, not native code.
- **Conclusion driving this plan:** compatibility work (L5/L6/L7) has to come
  before more performance work on the dynamic path, both because
  `lua-compatibility.md`'s own dependency order says so (metatables and GC
  must be stable before coroutines; coroutines change what "the dynamic
  path" even is) and because there is no honest performance claim to make
  about code Sol can't yet run.

## Phase 1 — `os`/`io`/`load` (L5 completion) — **done this session**

Scope: the mechanical, capability-gated stdlib surface that's currently just
declared (`LuaCapabilities::os`/`io`) but gates nothing observable.

- Add `LuaRuntime::with_capabilities(LuaCapabilities) -> Self` (new
  constructor; `new()` keeps its sandboxed-by-default capabilities).
- Add an `os` global table: `time`, `clock`, `difftime`, `date` (`*t`/`!*t`
  table form plus a `strftime`-subset string form: `%Y %m %d %H %M %S %y %p
  %A %a %B %b %j %%`), `getenv`, `exit`. Calendar math is hand-rolled
  (civil-from-days), no new dependency.
- Add an `io` global table: `io.write(...)` (appends to the runtime's
  captured output buffer, like `print` but no separators/newline) and
  `io.read(fmt)` reading from real process stdin (`"l"`/`"n"`/`"a"`
  variants). This is the CLI (`sol run`), not the browser sandbox, so real
  stdin/stdout is appropriate.
- Every function checks `self.capabilities.{os,io}` and raises the same
  "capability is disabled" error shape `require` already uses when off.
- `main.rs`'s CLI entry points (both the plain `compile`+tier path fallback
  and `run_lua_partitioned`) enable `os`+`io` by default, matching a real
  `lua` binary's behavior; library embedders keep default-deny.
- Add `load`/`loadstring` (compile a Lua string into a callable closure at
  runtime, reusing `lexer::lex_bytes`/`parser::parse_lua`) and `dofile`
  gated behind the existing `package`-style explicit-loader model (no raw
  filesystem access yet — `dofile` only resolves through the same
  `add_module`-registered sources `require` uses, documented as a deliberate
  deviation until real filesystem capability lands).
- Tests: `crates/sol/tests/lua55.rs` additions for `os.time`/`os.clock`
  monotonicity, `os.date` table and string forms, `os.getenv`, `io.write`
  captured-output round trip, `load` compiling and calling a string body,
  and capability-denial error text for each when disabled.
- Doc: mark the L5 "capability profiles" bullet in `lua-compatibility.md` as
  `[~]` (still no filesystem I/O) and add a new `load`/`loadstring`/`dofile`
  bullet (todo: this gap wasn't in the checklist at all before this plan).
- **Found and fixed during this phase (not originally scoped):** the initial
  whole-program `typeck::check()` attempt `main.rs`'s `run()` makes before
  falling back to `check_partitioned`/the dynamic runtime failed hard on any
  `.lua` program that called a new dynamic-only builtin (e.g. `os.time()`,
  `load(...)`) as its first dynamic-looking construct, because "unknown
  function '{name}'" was a plain type error, not tagged with the `[EDYNLUA]`
  suffix `requires_dynamic_runtime` checks for - so the new `os`/`io`/`load`
  surface was unreachable via the real CLI. Fixed by threading a `lua_mode`
  flag through `Checker` (`typeck::check`/`check_partitioned` both set it;
  `.sol`-mode callers pass `false`) so an unknown-function error is tagged
  `[EDYNLUA]` only when checking `.lua` source - `.sol` keeps it as a genuine
  hard error, since `.sol` has no dynamic-runtime fallback at all. Also
  closed a related gap: `dofile` wasn't checking `capabilities.package`
  before resolving a module name, unlike `require`.

## Phase 2 — `string.pack`/`unpack`/`packsize` (L5 completion) — **done this session**

- New `NativeFunction` variants mirroring Lua 5.5's format-string mini
  language (`b`/`B`/`h`/`H`/`i`/`I`/`l`/`L`/`j`/`J`/`f`/`d`/`s`/`z`/`x`,
  endianness `<`/`>`/`=`, alignment `!`). Implement in a new
  `crates/sol/src/lua_pack.rs` (parallels `lua_pattern.rs`'s existing
  standalone-engine style) rather than growing `lua_runtime.rs` further.
- Tests: round-trip pack/unpack for each format character, `packsize`
  agreement, and at least one fixture cross-checked against the pinned Lua
  5.5 reference via the existing oracle scripts.
- **Delivered scope:** `lua_pack.rs`'s unit tests cover round-trips for every
  integer/float/string option, endianness, alignment padding, and
  `packsize`/`pack` length agreement; `lua55.rs` adds a `.lua`-level
  regression test exercising `string.pack`/`unpack`/`packsize` through the
  real dynamic runtime (`T` size-prefixed strings, `z` zero-terminated
  strings, multi-value unpack positions). Not delivered: the `Xop`
  align-without-storing option (documented as unsupported, errors clearly);
  cross-checking against the pinned Lua 5.5 reference binary via the oracle
  scripts (deferred to Phase 3's broader corpus-conformance pass, which is
  the point where the reference toolchain is exercised systematically rather
  than ad hoc per phase).

## Phase 3 — Corpus conformance triage (L0/L1–L4 hardening) — **done this session**

Before adding more surface area, spend a pass turning `pending` into real
`pass`/`fail` signal:

- Run the existing reference-oracle scripts
  (`scripts/test-lua55-reference.sh`/`-container.sh`) against the 26
  `pending` manifest entries and record actual pass/fail per file (today
  this is unmeasured, not just unimplemented).
- Fix what's cheaply fixable (most likely: multi-value adjustment edge
  cases, `next`/`pairs` invalidation semantics, numeric coercion corner
  cases — the `[~]` items already flagged in L1–L4).
- Anything that needs a not-yet-built feature (coroutines, `io`/`os` — being
  built in Phases 1–2, `load` etc.) gets an explicit `blocked-on:` note in
  the manifest rather than staying a bare `pending`.
- Exit: manifest has zero bare `pending` rows with no explanation — every
  row is `pass`, `host-required`, or `blocked-on:<phase>`.

**Delivered scope:** ran `scripts/test-lua55-suite.sh` against the pinned Lua
5.5.1 checkout (all 34 upstream files; no real Lua 5.5.1 reference binary was
built this session, so classification is Sol-only, not yet a differential
oracle diff — that remains open, see below). Root-caused and fixed five real
bugs surfaced by running actual corpus files, none of which were previously
known:

- Method calls with one or more explicit fixed arguments clobbered the
  `self` register (`compile_call_args` started fixed args at `base` instead
  of `base + 1`); a method call used as a trailing multi-value call argument
  panicked a `debug_assert_eq!` (`compile_method_base` allocated its base
  register after evaluating the receiver instead of before). Both in
  `lua_bytecode.rs`.
- `function t.f(...)`/`function t:f(...)` declarations (nested and
  top-level) compiled the dotted/colon name as a literal global variable
  instead of a field assignment into `t`. Fixed in `lua_bytecode.rs`
  (nested case, via a new `compile_function_name_assign` helper) and
  `parser.rs` (top-level case, by no longer hoisting dotted/method
  declarations ahead of the statement that creates their base table, per
  real Lua's sequential-sugar semantics for these forms).
- A bare `global name1, name2` declaration (no `= value`) unconditionally
  overwrote every name with `nil`, clobbering built-ins like `print` —
  this is an extremely high-impact fix, since `global <const> print, assert,
  ...`/`global <const> *` appears at the top of nearly every real corpus
  file. Fixed in `lua_bytecode.rs`'s `Stmt::Global` codegen.
- `assert(v)` with no explicit message raised the falsy `v` itself as the
  error message instead of Lua's real default `"assertion failed!"`. Fixed
  in `lua_runtime.rs`.
- Numeric `for` loops (`Instr::ForPrep`/`ForLoop` in `lua_runtime.rs`) only
  supported integer control values and raised "expected integer" the
  instant any of start/stop/step was a non-integral float — e.g.
  `for i = 1, math.huge do` failed before the loop body ever ran once. Real
  Lua runs the whole loop in floats when any control value is a float.
  Fixed by branching on whether all three control values are integers at
  `ForPrep` time and running a parallel float-mode path otherwise (detected
  at `ForLoop` time by checking the loop variable's current runtime type).
  This one was found by chasing why `heavy.lua`'s `toomanyidx()` (a
  `for i = 1, math.huge do a[i] = i end` loop meant to run until a real,
  catchable out-of-memory error) "unexpectedly ran" in ~8ms instead of
  actually stress-testing anything — it was silently short-circuiting on
  the first iteration's float limit, and a `pcall` around the loop was
  swallowing the resulting error.

All six have regression coverage in `crates/sol/tests/lua55.rs`
(`dynamic_lua_runtime_method_calls_preserve_self_and_support_trailing_multi_value_args`,
`dynamic_lua_runtime_dotted_and_method_function_declarations_assign_table_fields`,
`dynamic_lua_runtime_bare_global_declaration_does_not_clobber_existing_bindings`,
`dynamic_lua_runtime_assert_default_message_matches_reference_lua`,
`dynamic_lua_runtime_numeric_for_loop_supports_float_control_values`).

Every one of the 26 `pending` manifest rows now carries a specific
`blocked-on:` note (see `tests/lua55/manifest.toml`) rather than a bare
`pending`. The most common blockers, in order of how many files they affect:

- **No `debug` library to register** (`big`, `constructs`, `db`, `errors`,
  `events`, `gc`, `gengc`, `literals`): these all `require "debug"` at the
  top of the file; `require` is intentionally sandboxed to explicit
  pre-registered in-memory modules (no filesystem loader), and Sol has no
  `debug` library implementation to register even if it did.
- **`<close>`/finalization not implemented** (`coroutine`, `goto`, `locals`):
  correctly deferred to Phase 4.
- **`_ENV` is not implemented at all** (`closure`, and likely others): Sol
  resolves globals through a dedicated `Globals` map, not an actual `_ENV`
  upvalue table, so the identifier `_ENV` itself reads as `nil` and code
  that indexes/reassigns it directly does not work. This was diagnosed by
  bisecting `closure.lua` down to its first failing construct
  (`_ENV[true] = 10`); a second, independent gap (weak tables never
  collecting, so the file's GC-wait loop spins until the instruction budget
  is exhausted) is reachable immediately after and is Phase 4-deferred as
  expected.
- **No persistent shared string metatable** (`bwcoercion`): `getmetatable("")`
  returns `nil` (strings are indexable via a special case in `index()`, not
  a real metatable object), so mutating the shared string metatable directly
  cannot work.
- **`Xop` string.pack alignment option** (`tpack`): already-documented Phase
  2 scope exclusion, not a new bug.
- **`any`-value dynamic/native boundary** (`attrib`, `calls`): only
  `i64`/`f64`/`bool` can cross the bridge; both files call a boundary
  function with an `any`-typed parameter.
- **Package/require sandboxing** (`bitwise`, `pm`): need filesystem-backed
  cross-file `require`, a stated non-goal.
- **No automatic string-to-number coercion in arithmetic** (`math`):
  confirmed even trivial `"2"+"3"` errors — coercion is entirely absent,
  not an edge case.
- **`math.random`/`math.randomseed` missing entirely** (`nextvar`).
- **`utf8.charpattern` missing** (`pm`, `utf8`): the UTF-8 pattern constant
  isn't exposed on either the global `utf8` table or `require'utf8'`'s
  table.
- **`table.create`'s hash-size hint isn't honored** (`sort`): no hash-part
  preallocation actually occurs for `table.create(0, n)`.
- **`string.rep` doesn't reject huge repeat counts** (`strings`): hangs
  indefinitely (confirmed a genuine infinite loop, even inside `pcall`,
  not just slow) instead of raising Lua's "too large" error.
- **Named-vararg parameter syntax `function f(...t)` is parsed but not
  implemented** (`vararg`): `t` is never bound to the packed extra
  arguments.
- **`os.tmpname` (and `io.output`/`io.close`/`os.remove`) missing**
  (`verybig`).
- **`heavy.lua`** used to false-pass in ~8ms due to the numeric-for-loop
  float bug above; now that it's fixed, the file's `toomanyidx()` stress
  loop runs for real and hits Sol's non-catchable instruction budget
  instead of a catchable out-of-memory error (Sol has no real
  allocation-tracked OOM signal a `pcall` can catch yet — a Phase
  4/GC-adjacent gap, tracked in its manifest note rather than fixed now).

Every one of these 9 files, plus all 17 already covered above, now has a
specific `blocked-on:` note in `tests/lua55/manifest.toml` — Phase 3's exit
criterion (zero bare `pending` rows) is met.

**Not delivered / still open:** building the pinned Lua 5.5.1 reference
binary and running the actual differential-oracle scripts
(`scripts/test-lua55-reference.sh`/`-container.sh`) — this session classified
failures by reading Sol's own error output, not by diffing against reference
Lua's actual behavior line-for-line. No corpus file was promoted to `pass`.

## Phase 4 — L6: precise GC semantics, weak tables, finalizers

This is the prerequisite the compatibility doc itself names before
coroutines (suspended coroutine frames are more reference-dense than
anything the current GC has had to track).

**Newly found while starting this phase (not visible when the phase was
first scoped):** two prerequisite gaps sit underneath all three bullets
below.

1. `LuaTable::get`/`LuaValue::key()` (`lua_runtime.rs`) only accept
   bool/integer/float/string as a table key today — a table or function
   used as a key errors `"table index has an unsupported type"`. Real Lua
   permits any non-nil, non-NaN value as a key. Weak *keys* (`__mode`
   containing `"k"`) are meaningless until this is fixed, since a
   table/closure-valued key can't be stored at all yet.
2. Every reference type (`LuaValue::Table`/`Closure`/etc.) is a plain `Rc`.
   There is no cycle collector at all today: a table that (directly or
   through a chain) references itself leaks for the process lifetime.
   `collectgarbage("collect")` is a total no-op stub (`lua_runtime.rs`
   `NativeFunction::CollectGarbage`) — it doesn't just run less often than
   real Lua, it does nothing. Finalizers (`__gc`) additionally need a
   callback hook at the exact moment an object becomes unreachable, which a
   bare `Rc`'s `Drop` can't provide (`Drop` has no access to the
   interpreter to call back into Lua, and can't detect "unreachable except
   for a cycle" at all).

Given that, this phase splits into two independently-shippable sub-phases
rather than one:

### Phase 4a — weak-value tables and table/function-valued keys (tractable now) — **done this session**

- Add `Table`/`Closure`/`NativeFunction`/`Native` variants to `LuaKey`
  (identity-hashed: `Rc::as_ptr(...) as usize`, mirroring the existing
  `PartialEq` identity semantics for these types at `lua_runtime.rs:394-398`)
  so any non-nil, non-NaN value can be a table key, matching real Lua.
- Add `__mode` parsing from a table's metatable (`"k"`, `"v"`, `"kv"`
  substrings, matching real Lua's flexible matching).
- Implement weak *values* using the fact that today's value graph is
  acyclic-collectible-by-refcount for anything not in a cycle: at
  `collectgarbage("collect")`, for every table whose mode includes `"v"`,
  scan entries whose value is a `Table`/`Closure`/`Native`/`GMatchIterator`
  Rc; if `Rc::strong_count(...) == 1` (this table's own copy is the only
  strong reference left), remove the entry. This is correct and matches
  real Lua's own "collection may happen at any explicit or implicit GC
  point" latitude — it just needs an explicit `collectgarbage()` call to
  observe the removal, which is honest given no automatic background
  collector exists yet.
- Weak *keys* (`"k"`) can reuse the same live-check now that table/function
  keys exist, with the same `strong_count == 1` rule, but only once
  Phase 4a's `LuaKey` change lands.
- Iteration (`next`/`pairs`) over a weak table must skip already-collected
  slots (should fall out of removing them at `collectgarbage` time rather
  than needing separate iterator logic).
- Tests: weak-value table drops an entry once its only other strong
  reference goes out of scope and `collectgarbage()` runs, but keeps it
  alive while a strong reference remains elsewhere; table-valued and
  function-valued keys round-trip through `get`/`set`/`pairs`; weak-key
  table drops an entry the same way.
- Does **not** attempt cycle collection or finalizers — a table cycle
  still leaks under this sub-phase, honestly documented as such.

### Phase 4b — cycle-collecting GC and finalizers — **done this session**

- The design fork this section used to pose ("(a) build a from-scratch
  tracing collector" vs. "(b) scope down to no cycle collection, ever") was
  resolved in favor of (a), per explicit user direction ("build a real
  tracing GC now"). Rather than replacing `LuaValue`'s `Rc<RefCell<...>>`
  representation with a separate arena/handle system, (a) was implemented as
  a CPython-style **trial-deletion cycle collector layered on top of** the
  existing `Rc` graph — `LuaRuntime::collect_cycles`
  (`crates/sol/src/lua_runtime.rs`):
  - Every ordinary table/closure construction is registered as a candidate
    via `track_table`/`track_closure`, which push a `Weak` handle into
    `gc_tables`/`gc_closures`. Bootstrap library tables (`string`/`math`/
    `table`/`utf8`/`package`/`os`/`io`, `package_loaded`) and the
    `Globals`/`GlobalsInner` scope-chain tables are deliberately left
    untracked — they're always reachable from `globals`, which the
    collector treats as an opaque, untracked root.
  - `collect_cycles` upgrades every candidate's `Weak`, then computes each
    candidate's residual reference count by subtracting one for every
    inter-candidate edge found by enumerating that table's array/hash
    entries and metatable, or that closure's upvalue cells. A candidate
    whose residual is still `> 0` is provably referenced from outside the
    tracked set, so **no root enumeration is ever needed** — reachability
    propagates by BFS from every positive-residual candidate over the same
    children edges. Anything left unreached after that is garbage,
    regardless of how large a cycle it's part of.
  - Subtlety worth preserving: the snapshot vectors used to hold candidates
    for the duration of the pass themselves hold one extra strong reference
    per candidate (from `Weak::upgrade`), so the residual must be seeded
    from `Rc::strong_count(candidate) - 1`, not the raw strong count —
    otherwise every candidate reads one reference too high and nothing is
    ever collected.
  - Safety argument: under-counting a children edge only makes the
    collector more conservative (leaks a bit more, never unsound);
    over-counting is the dangerous direction (could zero out a genuinely
    live candidate's residual and sweep it while still referenced), so
    every enumeration function is required to visit each stored `Rc` slot
    exactly once.
  - Sweep clears a table's array/hash/metatable or a closure's upvalues
    (breaking the `Rc` cycle so ordinary refcounting reclaims the rest) and
    credits the reclaimed struct sizes back into the allocation budget, so
    a loop that allocates cyclic garbage and calls `collectgarbage()`
    doesn't spuriously exhaust its budget.
  - Finalizers: a table's `__gc` metamethod (if its metatable defines one)
    is called exactly once, immediately before the table is cleared during
    sweep, with its fields still intact. Finalizer errors are discarded —
    `collectgarbage()` itself must not fail because of a broken `__gc`.
    **Known, deliberate limitation**: unlike real Lua, there is **no
    resurrection support** — an object referenced from inside its own
    `__gc` call is not kept alive for one more cycle; this collector always
    proceeds to clear it afterward. A `finalized` flag guards against
    running the same table's finalizer twice.
  - Tests (`crates/sol/tests/lua55.rs`): a self-referencing table cycle is
    reclaimed via `collectgarbage()` under a budget too small to leak
    unboundedly; a table+closure mutual cycle (a closure stored in a table,
    capturing that same table as an upvalue) is reclaimed the same way;
    a `__gc` finalizer is called exactly once per collected table and not
    at all for one still reachable when the script ends.
  - Still a documented no-op: the rest of `collectgarbage`'s option set
    (`"stop"`/`"restart"`/`"step"`/`"incremental"`/`"generational"`) beyond
    what's needed to trigger a full trial-deletion pass, and finalizer
    resurrection semantics. Both remain open follow-ups, not required by
    this sub-phase's exit bar.
- Add GC stress-test fixtures (collect at every allocation point / every
  dynamic bytecode instruction) per the L6 exit gate — **not done**; the
  three regression tests above cover the core correctness properties but
  don't yet stress every allocation site.

## Phase 5 — L7: coroutines

- Design decision to make explicit before coding (this is the single
  biggest architectural fork in the whole plan): coroutines need their own
  suspendable stack. Options: (a) heap-allocated explicit VM frames/registers
  in the existing tree-walking/bytecode interpreter (keeps everything on one
  Rust call stack, most compatible with "keep coroutines on bytecode
  initially" per the doc), or (b) OS threads + channel/condvar handoff per
  coroutine (simpler to implement, much higher per-coroutine memory/latency
  cost, harder to bound). The doc's own L7 items assume (a); follow that
  unless a spike shows it's impractical in the current bytecode design.
- Implement `coroutine.create/resume/yield/running/status/wrap` and
  yieldability rules across `pcall`/metamethods/native builtins per the
  checklist.
- Defer any dynamic-JIT interaction with coroutines entirely (per the doc:
  "keep coroutines on bytecode initially").

## Phase 6 — L8: differential testing, benchmarking, and the performance decision

- Build the differential runner (run each manifest fixture on both
  reference Lua 5.5 and Sol, diff normalized output) — this is what makes
  every future "conformant" claim checkable instead of aspirational.
- Add the missing benchmark categories against LuaJIT: hashmap/string
  workloads, closures, small objects, coroutine resume — not just the two
  numeric-array cases that exist today.
- Make the explicit call this plan has been deferring: either (a) invest in
  a real JIT/tracing tier for the dynamic `.lua` path (the only way "beats
  LuaJIT" becomes true for ordinary Lua code, not just typed `.sol`), or (b)
  formally narrow the performance claim in `docs/sol.md` to "beats LuaJIT on
  typed `.sol` code" and stop implying it for dynamic `.lua`. This decision
  should be revisited once Phases 1–5 are done and there's actual dynamic
  Lua code (real corpus programs) to benchmark against LuaJIT, not before.

## Non-goals for this plan

- `sol build`/`sol debug` getting the same per-function partition as `sol
  run` — unchanged, tracked separately.
- Real filesystem access for `io`/`dofile` beyond the in-memory loader model
  — a deliberate deviation until a capability/sandboxing design is chosen
  (this is a security-relevant decision, not a mechanical gap, and shouldn't
  be bundled into Phase 1).
- Any change to `crates/vm`/`crates/lua-vm` (the separate browser runtime) —
  out of scope per `AGENTS.md`.
