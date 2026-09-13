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

## Phase 4 — L6: precise GC semantics, weak tables, finalizers — **stress fixtures done this session**

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
  dynamic bytecode instruction) per the L6 exit gate — **done this session**.
  - `LuaRuntime` gained a `gc_stress: bool` field and a `set_gc_stress`
    setter (default off — this mode is far too slow for normal use). When
    enabled, `tick()` (the single chokepoint called once per dispatched
    dynamic bytecode instruction) and `charge_allocation()` (called at every
    allocation site) both run a full weak-table sweep + trial-deletion
    cycle-collection pass, instead of only running that work on an explicit
    `collectgarbage()` call.
  - This is safe by construction, not just by luck: the trial-deletion
    collector's residual computation
    (`Rc::strong_count(candidate) - 1 - inter_candidate_edges`) never relies
    on enumerating program roots. Any strong reference to a candidate held
    outside the tracked set — e.g. a live VM register or local holding an
    `Rc` clone — shows up automatically as a positive residual, since it was
    never subtracted as an inter-candidate edge. Running the collector at an
    arbitrary mid-execution point therefore can't reclaim something a live
    register still points to.
  - Exposed to the `sol` CLI via `SOL_LUA_GC_STRESS=1` (mirroring the
    existing `SOL_LUA_*_BUDGET` convention in `main.rs`'s
    `lua_dynamic_budgets`/new `lua_gc_stress_enabled`), threaded through
    `run_program_with_natives_and_budgets`'s new `gc_stress` parameter.
  - Tests (`crates/sol/tests/lua55.rs`): four new stress-mode fixtures cover
    the same table-cycle, table+closure-cycle, and finalizer scenarios as
    the non-stress GC tests but with `collectgarbage()` removed from the
    script entirely (stress mode collects on its own), plus a dedicated test
    that a still-reachable local survives 200 stress-collected garbage
    allocations around it.
  - Still open, tracked separately, not part of this bullet: running
    `gc.lua`/`gengc.lua`/`tracegc.lua` themselves (blocked on the unrelated,
    pre-existing `debug`-library/native-module gaps recorded in
    `tests/lua55/manifest.toml`).

## Phase 5 — L7: coroutines — **done this session**

- Design decision (the single biggest architectural fork in the whole plan):
  coroutines need their own suspendable stack. On inspection, the doc's
  originally-assumed option (a) - heap-allocated explicit VM frames/registers
  in the existing tree-walking interpreter - turned out to require rewriting
  `run_proto`/`call`'s recursive structure into an explicit frame-stack
  machine, not a small addition; this was new information not visible when
  the plan was written, so it was surfaced to the user as a genuine
  architectural fork rather than decided silently. The user's steering
  instruction was "prioritize performance to choose solution." Weighed
  against that criterion: (a) the interpreter rewrite (large, risky, and not
  obviously faster than the alternative), (b) OS threads + condvar handoff
  per coroutine (simplest, but a full OS context switch plus scheduler and
  mutex overhead per resume/yield), and (c) stackful fibers via a crate
  (a resume/yield is just a stack-pointer/register swap - no allocation, no
  OS scheduler involvement, no mutex). (c) was chosen and implemented via
  `corosensei` (`crates/sol/Cargo.toml`): each `LuaCoroutine`
  (`crates/sol/src/lua_runtime.rs`) owns a `corosensei::Coroutine` with its
  own heap-allocated, guard-paged OS stack (1 MiB, lazily committed), and
  `resume`/`yield` are real stack-pointer/register context switches within
  the existing single OS thread - this gets "coroutine.yield works from any
  Lua call depth, including inside `pcall`/metamethods/native builtin
  callbacks" for free, with **zero changes** to `run_proto`/`call`, unlike
  option (a).
- Implementation notes: a small `Rc<CoroLink>` indirection (not
  `Rc<LuaCoroutine>` directly) is captured by the fiber closure to avoid a
  permanent self-reference cycle (the fiber lives inside
  `LuaCoroutine::fiber`, so a closure capturing `Rc<LuaCoroutine>` back would
  keep every coroutine alive forever). `CoroLink` holds a raw
  `*mut LuaRuntime` refreshed immediately before each `resume()` (sound
  because coroutines are strictly cooperative - only one of
  {resumer, coroutine} ever runs at a time on this thread - and `LuaRuntime`
  never moves while a `resume()` call is on the Rust stack) and a raw
  `*const Yielder` that `coroutine.yield` dereferences via
  `LuaRuntime::coroutine_stack` (the chain of currently-resumed coroutines,
  pushed/popped by `resume_coroutine` itself, not the fiber body, so nested
  coroutine-resumes-coroutine cases track "who's actually running" correctly
  and so `coroutine.status`/`running`/`isyieldable` have an accurate source
  of truth). `LuaCoroutine.fiber` is `RefCell<Option<_>>`, taken out via
  `.take()` and put back around the (long, reentrant) `resume()` call rather
  than held borrowed, so a coroutine inspecting its own status doesn't hit a
  `RefCell` panic.
- Implemented `coroutine.create/resume/yield/running/status/wrap/isyieldable`
  and yieldability across `pcall`/metamethods/native builtins (verified with
  a `string.gsub` replacement-function callback specifically, since that's
  the deepest/most native-adjacent call site) per the checklist. Did not
  implement `coroutine.close`, coroutine cycle-collector tracking, threads as
  table keys, or a "main coroutine" sentinel value for `coroutine.running()`
  - documented as known limitations in `docs/features/lua-compatibility.md`'s
  L7 section.
- Deferred any dynamic-JIT interaction with coroutines entirely (per the doc:
  "keep coroutines on bytecode initially") - unchanged, since the `.lua` path
  has no dynamic JIT yet.
- Tests: `crates/sol/tests/lua55.rs` covers create/resume/yield round trips,
  yielding across nested helper calls/`pcall`/`gsub` callbacks, status
  transitions (including a coroutine inspecting its own status),
  `coroutine.wrap` error propagation, and rejecting resume of a dead,
  running, or normal coroutine.

## Phase 6 — L8: differential testing, benchmarking, and the performance decision — **done this session**

- **Differential runner**: added `scripts/test-lua55-differential.sh`,
  which runs a manifest case through both Sol and a pinned reference Lua
  5.5.1 build and diffs their stdout directly - distinct from the two
  scripts that already existed (`test-lua55-suite.sh` checks Sol against the
  manifest's declared expectation; `test-lua55-reference.sh` checks
  reference Lua against checked-in `tests/lua55/reference/*.stdout`
  snapshots; neither compares Sol's and reference Lua's output to each
  other). Follows the same conventions as `test-lua55-reference.sh`
  (`LUA55_REFERENCE_BIN`, refuses to run without it rather than silently
  substituting the system `lua` binary as the oracle - verified this
  refusal path directly). Not yet run end-to-end in this environment: no
  pinned Lua 5.5.1 source checkout is present locally (only the test
  corpus), so the script is ready but unexercised until that checkout
  exists.
- **Benchmarks**: added a `sol (dynamic)` row to `scripts/benchmark.sh` for
  every `.lua` benchmark (`sol run <name>.lua` with the sandboxed-embedder
  instruction/call-depth/allocation budgets overridden to effectively
  unlimited, since `sol run`'s CLI defaults are sized for untrusted embedded
  code and would abort a benchmark-sized workload partway through), and
  added `benchmarks/coroutine_resume.lua` (2M resume/yield round trips) -
  the one category that didn't exist before Phase 5. Ran the full suite with
  real hyperfine methodology (warmup 3, ≥10 runs); full results and analysis
  are in `benchmarks/RESULTS.md`'s "Phase 6 (L8)" section.
- **The decision**: the plan asked for an explicit choice between (a)
  investing in a real dynamic JIT/tracing tier for `.lua`, or (b) formally
  narrowing the "beats LuaJIT" claim to typed `.sol` only. The benchmark data
  makes this unambiguous: `sol (dynamic)` is 26-334× slower than LuaJIT
  across call/allocation-heavy workloads, but more decisively, it is also
  **1.3-4.2× slower than this project's own separate, unoptimized
  `crates/vm` tree-walking interpreter** (which itself has no JIT and is
  already 5-40× behind LuaJIT) on every workload but one near-noise outlier.
  Building a tracing/method JIT on top of an interpreter that isn't yet
  competitive with this repo's own naive tree-walker would be optimizing the
  wrong layer first. Decision: **(b)**. `docs/sol.md`'s existing "beats
  LuaJIT" claim was already scoped to typed `.sol` (it never claimed this for
  `.lua` compatibility mode), so no retraction was needed, but the doc now
  states the measured dynamic-path numbers explicitly instead of leaving
  them unmeasured, so the claim can't be misread as applying more broadly.
  A future dynamic-path optimization pass, if ever prioritized, should start
  with basic interpreter-level work (the bytecode/tree-walk tier itself),
  not a JIT - that's a separate initiative from this plan, not started here.

### Addendum: closing seven L8 benchmark-coverage and differential-runner gaps

All 6 phases above are now done (Phase 4's one remaining item, GC stress
fixtures, shipped in a later session pass; see its section). Reconciling
`docs/features/lua-compatibility.md`'s L8 checklist against what Phase 6
actually delivered turned up seven concrete, narrowly-scoped gaps, now closed
(six fully, one partially):

- `benchmarks/metatable_dispatch.lua`: the benchmark suite had no fixture
  exercising metatable dispatch or polymorphic field access at all. Added
  one modeled on real OOP-in-Lua code (three shape "classes", each with its
  own `__index` metatable, iterated through a shared call site so every
  `shape:area()` call resolves through a different concrete type). Verified
  against `lua`, `luajit`, `crates/vm`'s interpreter, and `sol run` directly
  before wiring it in — all four agree on the numeric result.
- `scripts/benchmark.sh` only ever reported one steady-state number
  (`--warmup 3 --min-runs 10`) per benchmark. It now also runs each command
  once unwarmed (`--warmup 0 --runs 1`) and reports it as a separate
  `<name> (cold)` row, so first-launch cost and steady-state cost are both
  visible instead of only the latter.
- `benchmarks/vararg_calls.lua`: no benchmark isolated vararg/multi-result-call
  overhead (only a correctness test did). Added one exercising multi-result
  return + multiple assignment, `...` forwarded through a second vararg
  function and walked with `select`, and a call used as the sole trailing
  argument of another call (must expand to all results). Same
  cross-runtime verification as above; see `benchmarks/RESULTS.md`'s "Phase
  6 addendum — vararg / multi-result-call overhead" section for numbers.
- `scripts/test-lua55-differential.sh` only ever diffed stdout. It now also
  compares exit status and stderr, both as "did this side fail / produce
  output at all" rather than requiring literal codes/text to match (Sol's
  CLI exit codes and error-message wording are not, and are not meant to be,
  byte-for-byte clones of PUC Lua's - see the script's header comment). A
  divergent case now names which axis failed, and also gets a minimized
  entry (source fixture, manifest `category`/`requires` as a capability
  profile, which axis diverged, first lines of any stdout diff) appended to
  a single `failure-report.md` alongside the existing per-case logs.
  Verified against synthetic fake `sol`/reference binaries covering match,
  stdout-only-divergence, and fully-divergent cases (a real pinned Lua 5.5.1
  reference build isn't available in this environment - see
  `tests/lua55/README.md`).
- `scripts/lua55-dashboard.sh` (new): no release dashboard existed at all.
  Generates a markdown report with the four things this L8 item asks for:
  manifest status counts (currently 26 `pending`, 8 `host-required`, 0
  `pass`/`adapted`/`diverges`), a capability-profile histogram (which
  `requires` tag blocks the most cases), the local benchmark environment
  (rustc/cargo/OS/`lua`/`luajit` versions), and typed-regression status -
  reported honestly as "not yet automated" rather than fabricated, since
  that gate (the L8 item directly above it) doesn't exist yet. It's a
  read-only report and does not itself reclassify any manifest case.
- `scripts/typed-regression-check.sh` (new): no automated typed-path
  regression gate existed. Runs every typed-only benchmark (every `.sol`
  file with no same-named `.lua` file) through hyperfine, compares each
  against a committed baseline (`benchmarks/typed-baseline.json`), and fails
  if a benchmark's mean regresses by more than 5% *and* that regression
  exceeds the combined baseline+current standard deviation - so a noisy but
  flat result doesn't false-alarm (verified live: `matrix` came back +8.8%
  in one run and correctly stayed `ok`, while an injected fake +1240%
  baseline correctly failed with exit 1). This automates only the wall-clock
  half of the L8 item; "inspect typed IR/assembly to confirm no `LuaValue`
  boxing or dynamic dispatch was introduced" is still a manual step the
  script explicitly reminds the caller to do, which is why that checklist
  item stays `[~]` rather than `[x]`.

- `crates/sol/tests/lua55_fuzz.rs` (new): no property/fuzz test
  infrastructure existed at all for the lexer/parser/table/multi-result
  items. Rather than add `proptest`/`quickcheck` as a new dev-dependency,
  added a small dependency-free seeded splitmix64 PRNG
  (`SOL_FUZZ_SEED`/`SOL_FUZZ_CASES` env vars) driving five tests: two
  crash-safety fuzzers for `sol::lexer::lex_bytes`/`sol::parser::parse_with_mode`
  (raw random bytes including non-UTF-8, and randomized real-token "soup"),
  one table set/get model check against a plain `HashMap` (generated as Lua
  source and run through `sol::lua_runtime::run_source`, since `LuaTable`'s
  methods are private outside the crate), and two multi-result-adjustment
  property tests across randomized 0-4-value return arities (multiple
  assignment, and table-constructor last-position-expands/non-last-truncates
  semantics — the latter's rules were manually cross-checked against
  reference `lua` before being encoded as assertions). All five pass
  (`cargo test --manifest-path crates/sol/Cargo.toml --test lua55_fuzz`).
  Explicitly deferred, not part of this pass: metamethod-recursion fuzzing,
  GC-root-handling fuzzing, and a "fuzz failure becomes a permanent fixture"
  pipeline (a failing case currently just prints its generated source for
  manual promotion into `tests/lua55.rs`) — this is why the checklist item
  stays `[~]` rather than `[x]`.

Still open, not part of this addendum: there is no fuzzing/seed concept in
the differential runner (it replays fixed corpus fixtures, not generated
inputs, so the failure report's "seed" field is always `n/a`), and
metamethod-recursion/GC-root-handling fuzzing remain unaddressed — these are
now the only open L8 sub-items. See `docs/features/lua-compatibility.md`'s
L8 section for the full reconciliation.

### Addendum 2: closing the two remaining L8 fuzzing gaps, plus two bugs they found

The two items the previous addendum left open — generated-input/seed
fuzzing for the differential runner, and metamethod-recursion/GC-root-handling
property tests — are now both closed, and closing them surfaced two real,
previously-unknown crash bugs, which are fixed alongside:

- `scripts/test-lua55-differential.sh`: added an opt-in generated-input fuzz
  mode (`SOL_LUA55_DIFF_FUZZ_CASES` — default `0`, so existing invocations
  are unaffected — and `SOL_LUA55_DIFF_FUZZ_SEED`), driven by a
  dependency-free deterministic LCG (matching this repo's existing
  hand-rolled-tooling convention) that assembles 1-4 randomly-chosen blocks
  from six templates (arithmetic, concatenation, table indexing,
  multi-return, conditionals, `while` loops) into self-contained, always-
  terminating Lua programs, then runs each through both Sol and the
  reference build and diffs the same three axes (stdout, exit-status,
  stderr-presence) as the fixed-corpus loop. Every generated program ends
  with `return "SOL_LUA55_FUZZ_DONE"`, a marker stripped from Sol's stdout
  before diffing — this works around, rather than "fixes", Sol's CLI
  unconditionally echoing its `main`'s return value (a deliberate, tested,
  documented behavior — see `docs/spec/functions-and-modules.md` and
  `dynamic_lua_code_can_call_a_natively_typed_helper_function` in
  `crates/sol/tests/lua55.rs`; an initial attempt to instead strip this echo
  from `crates/sol/src/main.rs` itself was caught by the existing test suite
  and reverted). A diverging case now gets its source copied to
  `tests/lua55/fuzz-fixtures/` (new, with a README explaining it's expected
  to stay empty) and a real seed recorded in `failure-report.md`. Verified
  against the real reference Lua 5.5.1 build available in this environment
  (`/opt/homebrew/bin/lua5.5`, used only as an explicit one-off
  `LUA55_REFERENCE_BIN` override, never as the script's silent default):
  200/200 generated cases matched; the divergence/promotion path was
  separately verified with a stubbed reference binary forced to disagree.
  The fixed-corpus loop itself is deliberately left untouched.
- `crates/sol/tests/lua55_fuzz.rs`: added the two previously-deferred
  categories, bringing the file to ten tests. Metamethod recursion: random-
  depth (1-80) linear `__index` fallback chains resolve correctly; a
  random-length (2-50) *cyclic* `__index` ring errors gracefully instead of
  hanging or crashing; a random-depth (1-300) `__call` forwarding chain
  dispatches correctly end to end. GC root handling: randomly-shaped (2-6
  table) cyclic garbage rings are reclaimed under a budget too tight to hold
  more than a couple of iterations at once; a table kept reachable through a
  randomized-depth (1-15) chain of field hops survives unrelated cyclic
  garbage collection happening around it every iteration. This closes the
  L8 fuzzing checklist item fully (`[x]`).
- **Bug found and fixed**: `LuaRuntime::index`/`set_index`
  (`crates/sol/src/lua_runtime.rs`) recursed through table-to-table
  `__index`/`__newindex` fallback chains as unbounded native Rust call
  frames — a cyclic metatable chain overflowed the native stack (a hard
  process abort) instead of raising a Lua error, unlike function-valued
  `__index`/`__call` chains, which were already bounded by `call_depth`.
  Fixed with a `MAX_METATABLE_CHAIN = 2000` bound matching real Lua's
  `MAXTAGLOOP`, written as an explicit loop (not depth-counted recursion —
  an initial recursive version was itself caught overflowing the stack by
  the new cyclic-chain test on a plain 8MiB stack) so the bound costs O(1)
  native stack regardless of chain length. Verified to produce error wording
  byte-identical to real Lua 5.5.1's own message for the same cyclic case.
- **Second bug found and fixed**: designing the `__call`-chain fuzz test
  revealed that Sol's dynamic interpreter, which dispatches every nested Lua
  call as a native Rust call, could overflow the native stack and abort the
  whole process on *ordinary, non-pathological* recursive Lua code at
  roughly 400-500 levels deep — well below the interpreter's own documented
  `max_call_depth` safety-net budget (1000), which never got a chance to
  fire first. Fixed at the CLI layer (`crates/sol/src/main.rs`): `sol
  run`/`build`/`debug` now dispatch onto a worker thread with an explicit
  256MiB stack instead of the ~8MiB default, so `max_call_depth` is now what
  actually fires for runaway recursion. This does not raise Sol's expressible
  recursion depth — still capped at `max_call_depth` — only makes that cap
  safely reachable. **Still open, out of scope for this pass**: real Lua
  keeps its own heap-allocated call stack instead of recursing natively per
  call, so it handles non-tail recursion hundreds of thousands of levels
  deep (verified against `/opt/homebrew/bin/lua5.5`); matching that would
  mean changing dynamic-call dispatch to not consume native stack per level
  (e.g. a trampoline) — a materially larger change than this pass's scope.

With both items above closed, the L8 checklist's fuzzing/differential-runner
section has no remaining open sub-items; see
`docs/features/lua-compatibility.md`'s L8 section for the authoritative
checklist state.

### Addendum 3: automating the L8 IR-inspection gate, plus a vacuous-check bug it found

The L8 regression gate's other still-manual half — "inspect typed IR/
assembly to confirm no `LuaValue` boxing or dynamic dispatch was introduced
into strict kernels" — is now partly automated:
`scripts/typed-regression-check.sh` runs every typed-only benchmark through
`sol run --dump-ir` (forcing immediate promotion via
`SOL_PROMOTE_THRESHOLD=1`/`SOL_OSR_THRESHOLD=1` so every kernel actually
JIT-compiles regardless of how many times the benchmark happens to call it)
and greps the dump for `call_indirect` or a call to one of the four runtime
helpers that exist specifically to service `any`-typed/dynamic values
(`sol_dynamic_binary`, `sol_dynamic_compare`, `sol_dynamic_neg`,
`sol_truth`). A typed-only benchmark has no `any`/dynamic values by
construction, so any of these appearing is a strong, low-false-positive
signal of unwanted dynamic dispatch.

**Bug found and fixed while validating this**: the first version of this
check (developed and manually spot-checked before being wired into the
script) reported zero dispatch hits across all 13 typed benchmarks — but a
follow-up adversarial check (writing a small fixture that deliberately does
`a + b` on two non-narrowed `any` values, which must compile to a
`sol_dynamic_binary` call per `codegen.rs`) also showed zero hits, which
should have been impossible. The root cause: Cranelift's `Function` `Display`
only ever prints a callee as an opaque `u0:N` module-function-id reference —
it never prints the callee's linkage name. Grepping the dumped text for
`sol_dynamic_binary` (or any other runtime helper's name) can therefore
*never* match, regardless of whether that call is present, making the check
vacuous — it would report "clean" unconditionally. The same defect turned
out to already be present in two existing `tests/programs.rs` assertions
(`!stderr.contains("sol_alloc")` and `!stderr.contains("sol_dynamic")`),
which had been passing for the same non-reason.

Fixed properly in `crates/sol/src/jit.rs`: `dump_clif_with_legend` replaces
the bare `eprintln!` at all three `--dump-ir` call sites (real function
bodies, `__spec{tag}` speculative specializations, `__osr{from_stmt}`
variants). After dumping a function's CLIF text, it walks that function's
external-function table (`ctx.func.dfg.ext_funcs`), resolves each `u0:N`
callee back to its real declared name via
`JITModule::declarations().get_function_decl(id).linkage_name(id)`, and
prints a `"; fnN = <real name>"` legend line per callee. This makes every
runtime call in a dump identifiable by name, not just by an opaque,
build-specific numeric index.
`dumped_ir_legend_resolves_runtime_calls_to_their_real_names`
(`tests/programs.rs`, using the new `tests/fixtures/dynamic_dispatch_probe.sol`
fixture) is a permanent regression test against this specific failure mode —
it forces a genuine, non-inlined, non-narrowed `any + any` and asserts the
dump names `sol_dynamic_binary`, so this can't silently regress back into a
no-op check again. With the legend fix in place, re-running the check
against all 13 typed benchmarks still reports zero dispatch hits — now a
meaningful result rather than a foregone conclusion.

**Still open, out of scope for this pass**: detecting boxing itself.
`Box`/`Unbox` (the typed/dynamic boundary coercions) compile to inline
bit-packing/tag operations in `codegen.rs`, not a runtime call, so there is
no callee name to grep for — this would need a different detection strategy
(e.g. a CLIF-level structural check for the specific instruction sequence
`Box`/`Unbox` lower to) if it's ever automated. Reviewing generated assembly
by hand for subtler regressions in touched kernels also remains manual; the
L8 checklist item stays `[~]` (partial) for this reason, and
`scripts/typed-regression-check.sh` still prints a reminder of both at the
end of a run.

### Addendum 4: pooling call frames to shrink the dynamic-`.lua` per-call cost

The other half of the task this pass picked up — "fix the documented
dynamic-path slowdown" (`benchmarks/RESULTS.md`'s Phase 6 finding that `sol
(dynamic)` is 3.3-3.4x slower than this project's own `crates/vm`
tree-walker on `metatable_dispatch.lua`/`vararg_calls.lua`, and up to ~250x
slower than LuaJIT) — is root-caused and partly fixed.

Root cause: `LuaRuntime::run_proto` (`crates/sol/src/lua_runtime.rs`)
allocated two fresh heap-backed `Vec`s on *every single Lua function call* —
a `regs: Vec<LuaValue>` sized to the callee's register count, and a
`cells: Vec<Option<Rc<RefCell<LuaValue>>>>` built unconditionally even when
the callee captures zero registers as upvalues (the common case). For
call-heavy benchmarks (`vararg_calls.lua`: 8,000,000 calls;
`function_calls.lua`: 10,000,000 calls), this is two `malloc`/`free` pairs
per call, layered on top of the register-cell-indirection overhead the
lua-compatibility.md register-unboxing addendum already addressed.

Fixed by adding a pair of free-list pools to `LuaRuntime` —
`regs_pool: Vec<Vec<LuaValue>>` and
`cells_pool: Vec<Vec<Option<Rc<RefCell<LuaValue>>>>>` — with
`take_regs_buffer`/`take_cells_buffer` helpers that pop a previously-used
buffer (falling back to a fresh allocation only when the pool is empty),
clear it, and refill it to the new callee's exact shape before use.
`recycle_frame_buffers` pushes both buffers back to their pools. Rust's own
call stack for `run_proto`'s recursive `self.call()` is itself LIFO, so a
buffer taken by an inner call is always returned before its caller resumes —
correctness-safe for arbitrary recursion, no extra bookkeeping needed.

**A wrong turn worth recording, matching the spirit of Addendum 3's**: the
first version of this fix wrapped the entire `'exec: loop { ... }` instruction
dispatch loop in an immediately-invoked closure, so every one of its many
internal `return`/`?` exit points would funnel through a single point that
recycled the buffers before returning — avoiding a one-by-one edit of the
~30 instruction-handling match arms. It built, passed all 117 tests, and
clippy stayed clean — but before committing, a controlled A/B (same
machine, same binary swapped in place via `git stash`, not just comparing
against numbers from a different run) on benchmarks with little or no call
traffic (`loop_sum`, `matrix`, `nested_loop`, `table_array` — each
effectively one `run_proto` invocation for the whole program) showed a real,
repeatable 3-8% *regression*, not noise. The closure captures `self`/
`regs`/`cells`/`pc`/`top` by reference rather than leaving them as plain
locals of the enclosing function, and that extra indirection cost more on
loop-heavy, call-light workloads than the pooling saved on call-heavy ones.
Reverted to a simpler design: recycle only at the one actual normal-return
site (`Instr::Return`, immediately before its `return Ok(values)`); an error
exit just drops its frame without recycling it, since errors are the rare
path and this sidesteps the closure restructuring entirely. Re-measured:
the loop-heavy benchmarks returned to within noise of the unmodified binary,
and the call-heavy gains held.

Final controlled-A/B results (one machine, same binaries, `hyperfine
--warmup 2 --min-runs 8`): `vararg_calls` 1.29x faster, `fib` 1.25x faster,
`gc_alloc` 1.10x faster, `function_calls`/`function_calls_closure`/
`metatable_dispatch` ~1.07x faster, `objects` 1.04x faster;
`loop_sum`/`matrix`/`nested_loop`/`table_array`/`coroutine_resume` unchanged
within noise (as expected — they have no meaningful call traffic to
amortize, or in `coroutine_resume`'s case resume an existing fiber's frame
rather than entering `run_proto` fresh on the measured hot path). Recomputed
against `crates/vm`: `sol (dynamic)` goes from ~3.3-3.6x slower than `vm` on
the call-heaviest benchmarks to ~2.6-2.9x slower — real, but not full
parity. Full numbers and methodology: `benchmarks/RESULTS.md`'s "pooled call
frames" addendum and `docs/features/lua-compatibility.md`'s matching
addendum.

**Still open, out of scope for this pass**: per-call `Vec` allocation was
one concrete, fixable cost among several. Register-cell-indirection overhead
on every `reg_get`/`reg_set` (an `Rc` clone plus a match branch to check
whether that register is captured), `LuaValue` cloning, and the
interpreter's general dispatch cost are all untouched. The Phase 6 decision
to formally narrow the "beats LuaJIT" claim to typed `.sol` code stands —
this fix narrows the gap against `crates/vm`, it doesn't close it, and
closing it fully would need the interpreter-level or tiering work Phase 6
already scoped as the honest next investment, not another allocation-shaped
fix.

## Non-goals for this plan

- `sol build`/`sol debug` getting the same per-function partition as `sol
  run` — unchanged, tracked separately.
- Real filesystem access for `io`/`dofile` beyond the in-memory loader model
  — a deliberate deviation until a capability/sandboxing design is chosen
  (this is a security-relevant decision, not a mechanical gap, and shouldn't
  be bundled into Phase 1).
- Any change to `crates/vm`/`crates/lua-vm` (the separate browser runtime) —
  out of scope per `AGENTS.md`.
