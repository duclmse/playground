# Lua 5.5 compatibility without regressing typed Sol

> Status: an interpreter-first subset is implemented. The checked corpus
> manifest and this document identify supported and open behavior.

**Purpose:** make a useful, measured subset of Lua 5.5 programs run unchanged as
`.lua` while retaining Sol's actual end goal: `.sol` code has static types,
unboxed data, a shared SSA optimizer, and native AOT/JIT execution. Lua
compatibility is a runtime and migration boundary, not a reason to lower every
Sol program to a boxed Lua VM.

This document refines the dynamic compatibility section of the
[feature delivery plan](delivery-plan.md) into an implementation and test
plan. It follows the intent in [faster_lua.md](../../faster_lua.md): build a
typed, specialized compiler with one optimization IR for AOT and JIT, and make
performance claims only for named workloads on named hardware.

## Current evidence and scope

The checked manifest currently classifies all 34 top-level Lua 5.5.1 cases:
26 remain semantic `pending` cases and 8 are explicitly `host-required`.
Focused fixtures now cover implemented parser/runtime slices, but no complete
upstream case has been promoted to `pass`; the result remains a baseline, not a
claim that the suite is a single ready-made Sol acceptance test. Every
`pending` row now carries a `blocked-on:` note naming the specific missing
feature or bug class observed against the pinned corpus (Phase 3 triage);
common blockers are a sandboxed `require` with no `debug` library to register,
`<close>`/finalization (Phase 4), missing `_ENV`, and no shared string
metatable — see `docs/features/lua-superset-plan.md`'s Phase 3 section for the
session-by-session detail.

The upstream suite also exercises a Lua executable, its C API, dynamically
loaded C modules, a terminal/readline fallback, host files such as `/dev/full`,
and locale-dependent collation and decimal parsing. Those are valid Lua
distribution tests, but are separate from language semantics. Sol must classify
them explicitly instead of hiding failures or declaring parser acceptance to be
compatibility.

### Compatibility contract

| Area                  | Contract                                                                                                                                                                                                                                                                             |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `.sol`                | Statically checked. Typed scalars, records, arrays, maps, and typed functions remain direct SSA values where their types permit it. Dynamic operations are explicit through `any` or an imported `.lua` module.                                                                      |
| `.lua`                | Dynamic compatibility mode. Values, globals, tables, closures, varargs, and metatables use the Lua runtime representation. It begins in the interpreter/bytecode VM.                                                                                                                 |
| Boundary              | Importing `.lua` from `.sol` exposes `any`/a runtime handle. Converting it to a typed record, map, array, scalar, or function requires an explicit checked operation. No implicit table shape conversion or speculative unboxing crosses the boundary.                               |
| Optimization          | The dynamic VM may gain guarded inline caches and profile-guided specializations. A failed guard resumes compatible bytecode/interpreter state. `.lua` is not AOT-native by default; a future frozen-module profile may opt in only after deoptimization and GC root maps are sound. |
| Unsupported host APIs | Capability-gated and reported as such. `io`, `os`, native module loading, debug hooks, and C API embedding never become silently available just because the compiler process has those privileges.                                                                                   |

`global` is an upstream Lua 5.5 extension and therefore belongs only to `.lua`
grammar. Support `global <const> name = value`, `global function name`,
`global none`, and the documented wildcard form with their Lua 5.5 scope and
const rules. Do not admit `global` into typed `.sol` source.

```mermaid
flowchart LR
  L[.lua bytes] --> P[Lua parser and dynamic bytecode]
  P --> V[LuaValue: nil/bool/number/string/table/function/thread/userdata]
  V --> I[Interpreter with budgets]
  I --> C[Guarded dynamic caches]
  S[.sol typed AST] --> T[Typed IR]
  T --> N[Unboxed JIT / AOT native code]
  I <-->|checked any boundary| T
```

## Test inventory and reporting model

Create a checked-in manifest at `tests/lua55/manifest.toml` that has one entry
per upstream case and records: upstream path and revision, category, required
host capabilities, expected output/error oracle, Sol status, and a link to the
adapted Sol fixture where one exists. Valid statuses are:

- `pass`: runs unchanged with Sol's supported capability profile and matches the
  oracle.
- `adapted`: the language assertion runs in a small Sol fixture because the
  upstream driver is testing the Lua executable or C API. The manifest names the
  removed host assumption and preserves the assertion.
- `host-required`: requires an intentionally unavailable capability; the runner
  checks that it is skipped with that reason.
- `pending`: a known semantic feature is missing. The entry names its owning
  feature area and issue/fixture.
- `diverges`: an intentional, documented difference with a migration path.

`fail` is never an accepted release status. A case that cannot yet run must be
`pending`, rather than being lost in a pile of command failures.

| Corpus group           | Representative upstream files                                                                             | What it establishes                                                       | First target                     |
| ---------------------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | -------------------------------- |
| Grammar and values     | `literals.lua`, `constructs.lua`, `locals.lua`, `goto.lua`, `attrib.lua`, `bitwise.lua`, `bwcoercion.lua` | Raw lexical input, declarations, scope, literals, operators, control flow | L1–L3                            |
| Calls and environments | `calls.lua`, `closure.lua`, `vararg.lua`, `nextvar.lua`, `errors.lua`                                     | Globals, closures, varargs, multi-results, protected errors               | L3–L4                            |
| Object protocol        | `events.lua`, `tpack.lua`, `sort.lua`                                                                     | Tables, iteration, metamethod dispatch and order                          | L3–L5                            |
| Library semantics      | `strings.lua`, `math.lua`, `utf8.lua`, `coroutine.lua`, `pm.lua`                                          | Base/table/string/math/utf8/package/coroutine behavior                    | L5–L7                            |
| GC and stress          | `gc.lua`, `gengc.lua`, `tracegc.lua`, `big.lua`, `heavy.lua`, `verybig.lua`                               | Reachability, finalization rules, memory pressure and limits              | L6–L8                            |
| Lua distribution/host  | `main.lua`, `api.lua`, `files.lua`, `memerr.lua`, `cstack.lua`, `code.lua`, `db.lua`                      | CLI, C API, loadable modules, OS/terminal/filesystem/debugger behavior    | L0 then separate capability work |

## L0 — Reproducible oracle and corpus triage

**Purpose:** distinguish an upstream environment problem from a Sol semantic
failure before implementing runtime features.

- [x] Pin the Lua 5.5.1 source tarball checksum and build a reference runner in
      a Linux container. Use its own `src/lua`, matching C headers, and the
      suite's `libs` build targets; do not compare against an unrelated system
      Lua package.
- [x] Provision the reference profile with dynamic module loading, the expected
      readline fallback, `/dev/full`, and an ISO-8859-1 Portuguese locale. Log
      the OS image, compiler, Lua configure flags, locale, and module search
      paths next to the result.
- [x] Add `scripts/test-lua55-reference.sh`, which builds or validates the
      reference prerequisites and writes per-case stdout, stderr, exit code,
      duration, and environment metadata under a caller-selected results
      directory.
- [x] Evolve `scripts/test-lua55-suite.sh` from a raw loop into a manifest-aware
      Sol runner. It must retain the existing ability to execute every top-level
      test, but summarize `pass`, `adapted`, `host-required`, `pending`, and
      unexpected failures separately.
- [x] Check in normalized text or structured snapshots for deterministic oracle
      cases. Normalize paths, executable names, and platform line endings only;
      do not normalize semantic output, errors, or ordering.
- [x] Triage all 34 files and record why every host-dependent file is adapted or
      capability-gated. Add a regression test for the manifest parser so an
      unclassified upstream file fails CI.

**Exit gate:** the reference profile completes its upstream suite subject only
to recorded platform differences, all 34 cases have a manifest row, and a CI
report can tell a missing Sol feature from an unavailable host facility.

## L1 — Byte-accurate Lua source and complete syntax front end

**Purpose:** parse the Lua 5.5 surface before attempting to execute it.

- [x] Make source loading byte-oriented. Preserve arbitrary bytes in string
      literals, comments, long brackets, and diagnostics; source positions are
      byte offsets plus line/column, not UTF-8 `String` indexes.
- [x] Add Lua lexical forms: decimal/hex/hex-float numerals, escapes and long
      strings/comments, Unicode escape handling where Lua specifies it, and all
      punctuation/operator tokens. Retain the stricter typed lexer rules for
      `.sol` where they intentionally differ.
- [~] Parse all Lua statement and expression forms: `local`, global assignment,
      `do`/`end`, labels and `goto`, numeric and generic `for`, `repeat`, table
      constructors, indexing/field access, method definitions and calls,
      anonymous/local/nested functions, vararg expressions, call-statement
      sugar, and expression-list assignment/return rules.
- [x] Parse table constructors (array, named, and computed fields), function
      call shorthand, method declarations/calls, and indirect-call syntax into
      Lua-specific AST nodes.
- [x] Parse local, nested, anonymous, and named-vararg (`...name`) functions;
      reject a vararg expression outside its enclosing vararg function.
- [x] Parse Lua labels/gotos, generic `for` loops, and multi-value returns.
      Same-block labels/gotos and general iterator triples now execute; complete
      goto scope-entry validation remains open.
- [x] Parse Lua 5.5 `global` declarations in `.lua` only, including attributes
      and const diagnostics. Reject the same spelling in `.sol` with a useful
      source span and migration suggestion.
- [~] Establish parser fixtures for each construct plus negative syntax tests
      for ambiguous long brackets, bad numerals, invalid labels, forbidden
      varargs, const global writes, and `.sol`/`.lua` mode boundaries. The
      construct, long-bracket/numeral, vararg, and source-mode coverage is in
      `lua55_parser_surface.lua`/`lua55.rs`; label scope and global-const write
      validation require L2's `_ENV` declaration metadata.

**Exit gate:** all syntax-oriented corpus cases parse to an inspectable Lua AST
or fail at the same source location as reference Lua. This gate has no claim
about runtime behavior.

## L2 — Dynamic bytecode and value model

**Purpose:** provide a correct foundation for Lua semantics that cannot be
represented by the current scalar-only `any` box.

- [~] Introduce a dedicated `LuaValue` tagged representation: `nil`, boolean,
      integer, float, interned/owned string, table, closure, native function,
      thread, and opaque userdata. Keep it distinct from typed SSA values and
      make every reference variant traceable by the collector.
- [~] Define Lua truthiness, number equality/comparison/coercion, string
      identity/content rules, number formatting, and key canonicalization with
      differential fixtures. Specify NaN and integer/float table-key behavior.
      Numeric `for` loops (`Instr::ForPrep`/`ForLoop`) now run a float-mode
      loop when any control value (start/stop/step) is a float, matching real
      Lua, instead of unconditionally requiring integer control values (this
      used to make any `for i = 1, math.huge do ... end` fail before its
      first iteration). Arithmetic still has no automatic string-to-number
      coercion at all (`"2" + "3"` errors; real Lua coerces numeral strings
      in arithmetic contexts) — open.
- [x] Add Lua bytecode registers, constants, upvalue descriptors, and source
      spans. Interpret dynamic code through this VM; do not force it through
      typed Cranelift lowering. `lua_bytecode.rs` compiles each Lua function
      to a `Proto` (register-based instructions, constants, upvalue
      descriptors, per-instruction source lines); `lua_runtime.rs`'s
      `run_proto` executes it directly, replacing the prior `Env`-chain
      AST-walking tree-walker entirely.
- [~] Give every Lua module its own `_ENV` table and resolve reads/writes via
      that environment. Implement `global` declarations through explicit
      environment slots/metadata, including their const constraints. Globals
      currently resolve through a dedicated `Globals` map
      (`GetGlobal`/`SetGlobal` bytecode ops), not an actual `_ENV` upvalue
      table — the identifier `_ENV` itself is unbound and reads as `nil`;
      code that indexes/reassigns `_ENV` directly (`_ENV[k]`, `local _ENV =
      ...`) does not work (see `closure.lua`'s corpus entry). A bare `global
      name1, name2` declaration (no `= value`) is fixed to read each name's
      current value before re-declaring it, instead of unconditionally
      overwriting it with `nil` — this was clobbering built-ins like `print`
      on every corpus file using the common `global <const> print, assert,
      ...` idiom.
- [~] Add a host-independent error object and stack trace shape. Errors must
      cross dynamic call boundaries without Rust panics or process aborts.

**Exit gate:** direct fixtures cover all value tags, globals/\_ENV isolation,
numeric corner cases, and error propagation; bytecode execution agrees with
reference Lua for the non-library portions of the L1 corpus group.

## L3 — Tables, assignment, and iteration

**Purpose:** implement the dynamic object model that most Lua code depends on.

- [~] Implement heap tables with Lua array and hash parts, `nil` deletion,
      stable object identity, length behavior, resize policy, and mutation write
      barriers.
- [x] Implement table constructors with keyed, array, and computed-key fields;
      expand the final expression according to Lua multi-result rules.
- [~] Implement assignment targets and expression lists left-to-right with Lua
      adjustment rules: only the final call/vararg expands; surplus values are
      discarded and missing values become `nil`.
- [~] Implement `next`, `pairs`, `ipairs`, raw get/set/equality/length, and the
      base/table APIs needed by the core corpus. Define invalidation behavior
      for table mutation during iteration and test it against reference Lua.
      Iterator triples and numeric integer/float key canonicalization are
      implemented; mutation-during-iteration oracle coverage remains open.
- [ ] Keep typed `Map<K,V>`, records, and arrays separate. A dynamic
      table becomes one only through checked conversion that validates keys,
      fields, values, and absence/presence rules.

**Exit gate:** adapted fixtures extracted from `constructs`, `nextvar`, `tpack`,
and `sort` pass, including table identity, deletion, iteration, and multiple
assignment/return cases.

## L4 — Functions, closures, varargs, and protected execution

**Purpose:** make normal dynamic Lua programs executable without weakening typed
typed top-level function values.

- [~] Compile Lua closures with heap environment cells for captured locals.
      Closing an upvalue at scope exit must preserve a single shared cell across
      all closures that captured it. Fixed two register-allocation bugs in
      method-call codegen (`lua_bytecode.rs`): method calls with one or more
      explicit fixed arguments (e.g. `t:greet("x")`) clobbered the `self`
      register because fixed arguments started at `base` instead of
      `base + 1`; and a method call used as a trailing multi-value call
      argument (e.g. `f(s:format(...))`) panicked a `debug_assert_eq!` because
      `compile_method_base` allocated its base register after evaluating the
      receiver expression instead of before. Also fixed `function t.f(...)`/
      `function t:f(...)` declarations (both nested and top-level/hoisted)
      compiling the dotted/colon name as a literal global variable (e.g. a
      global literally named `"t.f"`) instead of assigning into `t`'s field;
      top-level dotted/method declarations are no longer hoisted ahead of the
      statement that creates their base table, matching real Lua's
      sequential-sugar semantics.
- [~] Implement Lua call frames, recursive calls, proper vararg packs, and
      multi-result propagation through call, return, assignment, table
      construction, and parenthesized-expression truncation sites.
- [~] Implement `pcall`, `xpcall`, `error`, `assert`, and `select`, preserving
      error values and a bounded, useful stack trace. Add recursion/instruction
      budgets before host-exposed sandbox use. Recursion, instruction/backedge,
      and heap table/closure allocation budgets are enforced; exact byte and
      environment accounting awaits the dynamic GC allocator. `assert(v)` with
      no explicit message now raises the literal `"assertion failed!"` on a
      falsy `v` (previously it stringified the falsy `v` itself as the error
      message, e.g. `assert(false)` raised `"false"`). `LuaError` is still
      string-only (`{ message: String, stack: Vec<String> }`); raising a
      non-string value (table/number/boolean) loses its original type/identity
      once caught — a real gap, not yet fixed.
- [x] Specify tail-call behavior. Calls in tail position currently use ordinary
      bounded frames (no frame elision), preserving protected-call errors and
      stack traces. Proper-tail-call optimization remains a performance/fidelity
      follow-up and must retain those observables when added.
- [ ] Implement the `.sol` bridge for calling dynamic functions as `any` and
      checked conversion to a typed function signature. Enforce arity/result
      checks at the bridge; do not let `LuaValue` appear in typed IR absent an
      explicit dynamic operation.
- [x] Split `.lua` compilation per function instead of per file: one dynamic
      construct anywhere in a file no longer forces every function in that
      file into the bytecode interpreter. `typeck::check_partitioned`
      classifies each top-level function (including the synthesized `main`)
      as native-candidate or dynamic, then demotes any native-candidate that
      (directly or transitively) calls a dynamic function to a fixed point,
      via `jit::called_functions` over the native call graph. Dynamic code
      may call into surviving native functions through a one-directional
      bridge (`lua_runtime::LuaValue::Native`/`NativeBridge`); native code can
      never call back into the dynamic runtime. Only scalar (`i64`/`f64`
      /`bool`) parameters and returns may cross the bridge — non-scalar
      signatures (strings, tables, structs, arrays, maps, functions) are
      rejected at compile time with a clear error rather than silently boxed
      or miscompiled, preserving the "typed hot paths are never implicitly
      weakened" rule above. `sol run` uses this path (see
      `main.rs::run_lua_partitioned`); `sol build`/`sol debug` remain
      all-or-nothing pending follow-up work.

**Exit gate:** `calls`, `closure`, `vararg`, and relevant `errors` assertions
pass unchanged or as manifest-linked semantic fixtures, including shared
upvalues and final-expression result expansion.

## L5 — Metatables and essential libraries

**Purpose:** provide the Lua object protocol and the portable library subset.

- [~] Implement per-value/type metatables and table metatables. Table
      metatables dispatch
      `__index`, `__newindex`, `__call`, `__tostring`, `__len`, and `__pairs`;
      arithmetic, bitwise, concatenation, comparison, and equality lookup is
      also implemented. Per-type metatables and remaining edge-order rules are
      open: strings are indexable only via a special case in `index()` that
      routes directly to the `string` global table, not a real metatable
      object, so `getmetatable("")` returns `nil` instead of a shared,
      mutable `{__index = string}` table — code that mutates the shared
      string metatable directly (`getmetatable(""):__band = ...`, used by
      `bwcoercion.lua` to add bitwise metamethods to all strings) does not
      work yet.
- [~] Use metatable/table version counters for dynamic inline caches. Mutation
      counters are maintained now. Each future cache
      guards receiver kind, table shape, metatable identity, and version; any
      miss or mutation takes the generic bytecode path.
- [~] Implement deterministic base, table, string, math, and utf8 slices with
      per-function tests. Focused base/string support plus table
      `concat`/`insert`/`remove`/`pack`/`unpack`/`sort`/`create` and core numeric
      math functions (including `sqrt`/`sin`/`cos`/`tan`/`exp`/`log` and the
      `pi`/`huge`/`maxinteger`/`mininteger` constants) are present, along with
      `tonumber`, `string.byte`/`char`, and `utf8.len`/`char`/`codepoint`. A
      real Lua pattern-matching engine (`crates/sol/src/lua_pattern.rs`:
      character classes, `[...]` sets, `^`/`$` anchors, `()` captures
      including position captures, `%1`-`%9` back-references, `*`/`+`/`-`/`?`
      quantifiers, `%b`/`%f`) backs `string.find`/`match`/`gmatch`/`gsub`
      (string/table/function replacements). `string.format` covers
      `d`/`i`/`u`/`x`/`X`/`o`/`c`/`f`/`F`/`e`/`E`/`g`/`G`/`s`/`q`/`%` with
      flags/width/precision (note: `%e`/`%g` use Rust's float formatter under
      the hood, so extreme-precision rounding may not bit-match C's libm).
      `collectgarbage` is a deterministic approximation over the existing
      allocation budget (`"count"` reports bytes used). `"collect"`/`"step"`
      run a full trial-deletion cycle-collector pass (see L6) on top of the
      `Rc`-based value graph, reclaiming table/closure reference cycles and
      running `__gc` finalizers; `"stop"`/`"restart"`/`"isrunning"`/
      `"incremental"`/`"generational"` are accepted but remain no-ops, since
      there is no incremental/generational scheduling to toggle.
      `string.pack`/`unpack`/`packsize` are implemented
      (`crates/sol/src/lua_pack.rs`: endianness `<`/`>`/`=`, alignment
      `!`/`!n`, integers `b`/`B`/`h`/`H`/`i`/`I`/`l`/`L`/`j`/`J`/`T`, floats
      `f`/`d`/`n`, strings `s`/`z`/`c`, padding `x`); the align-without-storing
      `Xop` option is not implemented and errors clearly if used. Broader
      math/utf8 edge cases remain.
- [x] Implement a sandboxed `package`/`require`: an explicit host-provided
      loader (`LuaRuntime::add_module`) registers exact-name in-memory module
      sources; `package.loaded` caches each module's result so repeated
      `require` calls return the same value; a cyclic `require` observes a
      deterministic partial-initialization sentinel instead of reloading or
      overflowing the call stack; each loaded module gets its own global
      environment (`_NAME` set, globals isolated from the requiring module and
      from other modules). Native/filesystem loaders stay off: `require` is
      rejected until the `package` capability is explicitly enabled by
      registering a module. Path-based search and a stable module-interface
      format remain open.
- [~] Split `io`, `os`, `debug`, native module loading, and locale APIs into
      declared capability profiles. `LuaCapabilities` declares `package`, `io`,
      `os`, `debug`, and `native_modules` as independent flags. `package`
      denial (no registered module) vs. allowed (an explicit `add_module` call)
      behavior is tested; `os` (`time`/`clock`/`difftime`/`date`/`getenv`/
      `exit`) and `io` (`write`/`read`) are now implemented and independently
      capability-gated too, with denial/allowed tests for each
      (`LuaRuntime::with_capabilities`; the `sol` CLI enables `os`+`io` by
      default since it is a trusted native tool, unlike library embedders
      which keep the sandboxed-by-default profile). `os.date` uses UTC-only
      hand-rolled calendar math (no timezone database), so `os.date(...)` and
      `os.date("!"...)` currently render identically, and its strftime-subset
      only covers `%Y %y %m %d %H %M %S %p %A %a %B %b %j %c %%`. `io.write`
      does not yet return a file-handle object for chaining. `debug` and
      `native_modules` still gate nothing observable.
- [~] Implement `load`/`loadstring`/`dofile` (compile a Lua string/registered
      module into a callable closure at runtime). `load`/`loadstring` compile
      arbitrary source and return `(nil, error_string)` on failure, matching
      Lua's `load` contract; `dofile` is gated behind the same `package`
      capability and explicit in-memory loader `require`/`add_module` use (no
      raw filesystem access yet - a deliberate deviation until a real
      filesystem capability is designed).

**Exit gate:** the object-protocol assertions from `events`, `sort`, and table
tests pass; portable portions of `strings`, `math`, and `utf8` pass against the
oracle; every omitted library entry has a documented capability status.

## L6 — Precise roots, GC semantics, and finalization

**Purpose:** make the new reference-heavy runtime safe before optimizing it.

- [ ] Implement the precise-GC work required for dynamic values: layout descriptors for
      tables, strings, closures, upvalue cells, iterator state, errors, and
      coroutine frames; compiler/VM root stacks; and write barriers on every
      reference store.
- [~] Weak-table (`__mode`) behavior and table/function-valued table keys are
      implemented (`lua_runtime.rs`: `LuaKey` gained `Table`/`Closure`/
      `NativeFunction`/`Native`/`GMatchIterator` variants with identity-based
      `Hash`/`Eq`; `setmetatable` registers a weak-mode table into
      `LuaRuntime::weak_tables`; `collectgarbage("collect"/"step")` sweeps
      that registry via `sweep_weak_tables`/`prune_weak_table`, removing
      entries whose reference-typed key/value has no strong reference left
      outside the table itself). This is a registry-based sweep over
      explicitly weak-registered tables, not a general heap/root scan - see
      `docs/features/lua-superset-plan.md`'s Phase 4a.

      Cycle collection and `__gc` finalizers are now implemented on top of
      the `Rc`-based value graph (Phase 4b,
      `LuaRuntime::collect_cycles`/`track_table`/`track_closure`): every
      ordinary table/closure allocation is registered as a candidate, and
      `collectgarbage("collect"/"step")` runs a CPython-style trial-deletion
      pass that reclaims reference cycles (a self-referential or
      mutually-referential table/closure pair no longer leaks for the
      process lifetime) without needing to enumerate program roots. A
      table's `__gc` metamethod, if its metatable defines one, is called
      once right before the table is cleared during sweep, with its fields
      still intact; finalizer errors are discarded so a broken `__gc` can't
      fail `collectgarbage()` itself. The one deliberate gap versus real
      Lua: there is no resurrection support — an object referenced from
      inside its own `__gc` call is not kept alive for one more cycle, it is
      cleared immediately afterward regardless.
- [x] Add stress modes that collect at allocation points and after every dynamic
      bytecode instruction (Phase 4b addendum, `lua_runtime.rs`:
      `LuaRuntime::set_gc_stress`/`gc_stress` field). When enabled, both
      `tick()` (the once-per-dispatched-instruction chokepoint) and
      `charge_allocation()` (the per-allocation-site chokepoint) run a full
      weak-table sweep + trial-deletion cycle collection, rather than only on
      an explicit `collectgarbage()` call. This is sound at arbitrary
      mid-execution points because trial deletion's residual count
      (`strong_count - 1 - inter_candidate_edges`) never depends on
      enumerating roots — any live register/local holding a strong reference
      to a candidate surfaces as a positive residual automatically. Exposed
      to the `sol` CLI via `SOL_LUA_GC_STRESS=1`
      (`main.rs::lua_gc_stress_enabled`, mirroring the existing
      `SOL_LUA_*_BUDGET` env vars). Four fixtures in
      `crates/sol/tests/lua55.rs` re-run the self-cycle, table+closure-cycle,
      and finalizer scenarios with stress mode collecting on its own instead
      of an explicit `collectgarbage()` call, plus a fixture proving a
      still-reachable local survives 200 stress-collected allocations of
      unrelated cyclic garbage around it. Not yet done: collector
      statistics (e.g. pass counts/bytes reclaimed) are not yet surfaced to
      the benchmark harness (see the exit gate below) — memory-checker
      integration (ASan/valgrind) is also not wired into these fixtures, only
      Sol's own budget accounting.
- [ ] Run `gc`, `gengc`, and `tracegc` in their own capability category; map
      their implementation-dependent expectations to semantic invariants and
      retain exact upstream assertions where Sol claims identical behavior.

**Exit gate:** dynamic stress fixtures have no dangling references or missed
roots (done — see the stress-mode checklist item above), collector
statistics are exposed to the benchmark harness (**not done**), and every
supported GC observable has a reference-backed test (done for weak tables,
cycle collection, and finalizers, both under normal and stress-mode
collection; `gc`/`gengc`/`tracegc` themselves remain blocked on the
unrelated debug-library/native-module gaps tracked in
`tests/lua55/manifest.toml`).

## L7 — Coroutines and resumable execution

**Purpose:** add Lua cooperative concurrency only after stack and GC ownership
are explicit.

- [x] Implement `coroutine.create`/`resume`/`yield`/`status`/`wrap`/`running`/
      `isyieldable` and error propagation. Rather than the originally-planned
      heap-owned explicit VM frame representation (which, on inspection,
      would require rewriting the interpreter's recursive `run_proto`/`call`
      structure into an explicit frame-stack machine), each coroutine is a
      real stackful fiber: a separate heap-allocated OS stack
      (`corosensei::Coroutine`, 1 MiB `mmap` + guard page) that a `resume`
      call context-switches onto and a `yield` call context-switches back
      from, entirely within one OS thread. This was chosen over both the
      frame-rewrite and an OS-thread-plus-condvar handoff because a fiber
      switch is just a stack-pointer/register swap - no allocation, no OS
      scheduler, no mutex - making it the fastest of the options considered
      (`crates/sol/src/lua_runtime.rs`: `LuaCoroutine`, `CoroLink`,
      `LuaRuntime::new_coroutine`/`resume_coroutine`/`call_coroutine_wrapper`;
      see `docs/features/lua-superset-plan.md` Phase 5). `LuaValue::Thread`
      holds the coroutine handle `type()` reports as `"thread"`;
      `LuaValue::CoroutineWrapper` (from `coroutine.wrap`) reports as
      `"function"` and propagates an internal error as a real Lua error
      instead of `coroutine.resume`'s `(false, message)` pair, matching Lua.
- [x] Yieldability works from any Lua call depth for free, as a consequence of
      the fiber approach: `coroutine.yield` inside a helper function, inside
      `pcall`, and inside a native builtin's Lua-callback argument (e.g.
      `string.gsub`'s replacement function) all context-switch correctly with
      zero special-casing in `run_proto`/`call`/`pcall`/`gsub_replacement` -
      covered by
      `dynamic_lua_runtime_coroutine_yields_across_nested_calls_pcall_and_native_callbacks`
      in `crates/sol/tests/lua55.rs`. Calling `coroutine.yield` outside any
      coroutine raises the Lua-compatible "attempt to yield from outside a
      coroutine" error.
- [ ] Keep coroutines on bytecode initially. A future dynamic JIT call either
      resumes through a compatible trampoline with complete root maps or
      deoptimizes to the saved interpreter frame before yielding. (Dynamic
      JIT does not exist yet for the `.lua` path, so this is unchanged.)
- [~] `crates/sol/tests/lua55.rs` covers create/resume/yield round trips
      (multiple yields, values flowing both directions), status transitions
      (`suspended`/`running`/`normal`/`dead`, including a coroutine observing
      its own status and `coroutine.running`/`isyieldable` inside vs. outside
      a coroutine), `coroutine.wrap` error propagation, and rejecting resume
      of a dead, running, or normal coroutine. Deterministic scheduling
      fixtures beyond these, dedicated repeated resume/yield stress, and
      GC-during-suspension tests remain open.

      Known, deliberate limitations: coroutines are not tracked by the
      Phase 4b cycle collector (`collect_cycles`), so a reference cycle
      routed through a coroutine leaks, the same conservative class of gap as
      other untracked types; there is no `coroutine.close`; threads and
      wrapper functions cannot yet be used as table keys (`LuaValue::key()`
      unchanged); and there is no "main coroutine" sentinel value -
      `coroutine.running()` returns `(nil, true)` for the main chunk instead
      of a real thread value.

**Exit gate:** portable assertions from `coroutine.lua` pass and a suspended
coroutine remains valid across full collections, protected errors, and module
calls. (`tests/lua55/manifest.toml`'s `coroutine.lua` entry stays `pending`:
the upstream test also exercises `<close>` to-be-closed variables, which is a
separate, unimplemented feature.)

## L8 — Optimization, differential testing, and release gates

**Purpose:** improve dynamic execution only after correctness is measurable,
while proving the typed path retains its defining advantage.

- [~] Add a differential runner that executes each manifest fixture on reference
      Lua and Sol, compares normalized stdout/stderr/exit status, and emits a
      minimized failure report with source, seed, and capability profile.
      `scripts/test-lua55-differential.sh` (Phase 6) runs every manifest
      case through both Sol and a pinned reference Lua 5.5.1 build, preserving
      per-case logs in `SOL_LUA55_DIFF_RESULTS_DIR`. It now compares all three
      axes named in this item: stdout byte-for-byte, exit status as "did this
      side fail at all" (Sol's CLI and PUC Lua's `lua` don't share a nonzero
      exit-code convention, so requiring the literal codes to match would
      test an undocumented implementation detail), and stderr the same way
      ("did this side produce anything on stderr at all", not literal text -
      Sol's error-message wording is not a byte-for-byte clone of PUC Lua's C
      error/traceback formatting). A divergence report names which axis
      failed (e.g. `stdout`, `exit-status(sol=1 ref=0)`,
      `stderr-presence(sol=0 ref=1)`) per case. Still unimplemented: there is
      no fuzzing/seed concept (it replays fixed corpus fixtures, not
      generated inputs) and no separate "minimized failure report" artifact
      beyond the per-case logs already written, so that half of this item
      remains open.
- [ ] Add property tests and fuzzing for lexer/parser round trips, table
      operations, multi-result adjustment, metamethod recursion, and GC root
      handling. Differential fuzz failures become permanent fixtures.
- [x] Benchmark dynamic table array/hash reads, polymorphic field access,
      closure allocation/calls, vararg/multi-result calls, metatable dispatch,
      GC pressure, and coroutine resume. Report interpreter cold start and
      steady-state cache/JIT results separately against the pinned Lua 5.5
      reference and any additional named runtimes. `benchmarks/*.lua` covers
      closure allocation/calls (`function_calls_closure`), table array/hash
      reads (`table_array`, `hashmap_lookup`), GC pressure (`gc_alloc`),
      coroutine resume (`coroutine_resume`), and, as of this update,
      polymorphic field access and metatable dispatch together
      (`metatable_dispatch.lua`: three unrelated "classes" sharing no common
      base, each with its own `__index` metatable, called through a single
      shared call site so every invocation re-resolves `area` through a
      different object's metatable rather than hitting a monomorphic
      target) and, as of this update, vararg/multi-result-call overhead
      specifically (`vararg_calls.lua`: multi-result return consumed by
      multiple assignment, `...` forwarded through a second vararg function
      and walked with `select`, and a call used as the sole trailing
      argument of another call so it must expand to all of its results).
      `scripts/benchmark.sh` now runs every benchmark twice and reports both
      numbers as separate rows: `<name>` (the existing `--warmup 3
      --min-runs 10` steady-state mean) and `<name> (cold)` (`--warmup 0
      --runs 1`, the first unwarmed process launch - a single sample, so its
      stddev is always 0) — see `benchmarks/RESULTS.md`. This closes the
      last concrete benchmark-fixture-coverage gap; still open, and
      unrelated to fixture coverage: the differential runner only diffs
      stdout (see the L6 item below), and there is no fuzzing/property-test
      infrastructure or release dashboard (see the two `[ ]` items below).
- [ ] Before and after every dynamic optimization, run the existing typed Sol
      numeric, allocation, callback, table/array, JIT, and AOT benchmarks.
      Reject a material typed-path regression (initial budget: 5% outside
      measurement noise) and inspect typed IR/assembly to confirm no `LuaValue`
      boxing or dynamic dispatch was introduced into strict kernels.
- [ ] Add a release dashboard with corpus counts by manifest status, capability
      profile, benchmark environment, and typed regression status. Only move a
      case from `pending` to `pass` with an oracle-backed test.

**Exit gate:** all release-supported entries pass under the documented profile,
no unclassified corpus failures remain, and published performance claims state
the workload, machine, compiler revision, warm-up policy, and comparison.

### Addendum: CLI instruction/allocation budget was blocking any realistic benchmark

Attempting a first honest benchmark of the dynamic `.lua` path (not the typed
`.sol` native path `scripts/benchmark.sh` measures by default) surfaced that
`sol run` silently inherited `LuaRuntime::new()`'s sandboxed-embedder defaults
(1,000,000 instructions / 1,000 call depth / 64MiB allocation - sized for
untrusted embedded code), unlike `os`/`io` capabilities, which the CLI already
relaxes as a "trusted native tool." 8 of 10 `benchmarks/*.lua` files
(`fib`, `function_calls`, `function_calls_closure`, `gc_alloc`, `loop_sum`,
`matrix`, `nested_loop`, `objects`, `table_array`) failed outright with
`Lua instruction budget exhausted` or `Lua allocation budget exhausted` before
producing any output at all.

Fixed by adding `LuaRuntime::with_capabilities_and_budgets` and
`run_program_with_natives_and_budgets` (`lua_runtime.rs`), and wiring opt-in
`SOL_LUA_INSTRUCTION_BUDGET` / `SOL_LUA_CALL_DEPTH_BUDGET` /
`SOL_LUA_ALLOCATION_BUDGET` env vars into both `run_lua_partitioned` call
sites in `main.rs`, following the existing `SOL_PROMOTE_THRESHOLD`/
`SOL_OSR_THRESHOLD` env-var-override precedent (`tier.rs`). Defaults are
unchanged when the vars are unset, so library embedders (including the
browser/sandboxed use case) are unaffected - only the CLI's opt-in override
path is new. Regression coverage:
`dynamic_lua_runtime_capabilities_and_budgets_can_be_overridden_together`
(`tests/lua55.rs`).

With generous overrides, all 10 benchmarks ran to completion. Single-run,
one machine, release build, no warm-up (a first look, not a release-grade
measurement - a real L8 pass still needs hyperfine's warm-up + ≥10-run
methodology): Sol's dynamic interpreter was consistently slower than
reference Lua 5.4 (`lua`) by roughly 3x-47x depending on workload (worst on
function-call-heavy code: `function_calls`/`function_calls_closure` at
~46x; best on the two already-small/near-noise cases,
`hashmap_lookup`/`string_concat`), and far behind LuaJIT (up to ~870x on
`function_calls`). This is a naive tree-walking/bytecode interpreter with no
tiering, expected for this stage of the plan - Phase 4-6 (precise GC,
coroutines, then this L8 phase's own optimization work) haven't started yet.
A second, independent, unrelated-to-this-fix observation from the same run:
Sol's dynamic float-to-string conversion for large magnitudes prints full
decimal digit expansion (e.g. `5333329333341399000`) where reference Lua's
`%.14g`-based formatting uses scientific notation (`5.333329333341399e+18`)
- a real output-format divergence, not yet triaged into the manifest since it
wasn't hit by any of the 26 corpus cases audited in Phase 3.

**Update (Phase 6/L8)**: this single-run, no-warm-up estimate is now
superseded by a real hyperfine run (warmup 3, ≥10 runs) covering all ten
original benchmarks plus `coroutine_resume.lua` - see `benchmarks/RESULTS.md`'s
"Phase 6 (L8)" section for the full table and `docs/features/lua-superset-plan.md`'s
Phase 6 section for the performance-claim decision it fed into. The
directional finding held up and sharpened: `sol (dynamic)` isn't just far
from LuaJIT (26-334×, worse than this addendum's original 3-47x/870x
estimate on the heaviest workloads once measured properly with warm-up), it
is also 1.3-4.2× slower than this project's own separate, unoptimized
`crates/vm` tree-walking interpreter on every workload but one near-noise
outlier - a stronger signal that the dynamic path needs interpreter-level
work before a JIT would be the right next investment.

### Addendum: register-slot-reuse closure bug, and selective register unboxing

While investigating the interpreter overhead behind the benchmark numbers
above, found that `crates/sol/src/lua_bytecode.rs`'s register allocator
recycled temporary/local register numbers (`FuncState::end_statement`/
`pop_scope` reset `next_reg` downward) with no awareness of whether a given
register number had ever been captured as a `ParentLocal` upvalue by an
escaping closure. Because a captured register's `Rc<RefCell<LuaValue>>` cell
is shared by reference with the closure for as long as the closure lives
(potentially past the scope/frame that created it - e.g. a closure stored in
a table), a later, unrelated write to the same register number (an ordinary
expression temporary in a sibling or later statement) silently corrupted the
escaped closure's captured value. Reproduced against the unmodified binary
with a `do local x = 99; caps[1] = function() return x end end` followed by
unrelated arithmetic reusing `x`'s register number - `caps[1]()` returned the
unrelated temporary's value instead of `99`.

Fixed by tracking, per `FuncState`, the set of registers ever captured
(`captured: HashSet<Reg>`) and a monotonically increasing `retired_floor`
that every `next_reg`-lowering site (`reset_to`, `pop_scope`,
`end_statement`) now clamps against, so a captured register's number is
permanently retired from reuse for the rest of that function's compilation
once discovered in `resolve()`. This is deliberately conservative (may retire
a few more registers than strictly necessary) but guarantees no future reuse
can alias a still-live captured cell. Regression coverage:
`dynamic_lua_runtime_closures_survive_register_slot_reuse_after_their_scope_ends`
(`tests/lua55.rs`).

This same "which registers are ever captured" analysis is also the
prerequisite for closing part of the interpreter-overhead gap measured above.
Per `lua_bytecode.rs`'s top-of-file design note, every VM register was an
`Rc<RefCell<LuaValue>>` cell - one heap allocation per register per call,
plus an `Rc` deref and `RefCell` borrow-check on every single register read
or write, dominating interpreter overhead. Since the compiler now already
knows exactly which registers are ever captured
(`Proto::captured_registers: Vec<bool>`), `lua_runtime.rs`'s `run_proto` was
changed to a dual representation: an uncaptured register is a plain
`LuaValue` in a flat `Vec<LuaValue>` (`regs`); only the (typically small)
subset of registers actually captured by a nested closure get a real
`Rc<RefCell<LuaValue>>` cell (`cells: Vec<Option<Rc<RefCell<LuaValue>>>>`).
`NewLocal` and per-iteration loop-variable writes give a captured register a
*fresh* cell identity (so a closure created in one iteration keeps its own
cell after a later iteration reuses the same register number); everything
else reads/writes through the plain slot with no allocation, no `Rc`, and no
borrow-check.

Same methodology as the benchmark table above (single machine, release
build, no warm-up - a first look, not a hyperfine-grade measurement), same
budget overrides:

| benchmark | before (vs `lua`) | after (vs `lua`) |
|---|---|---|
| fib | ~32x | ~18x |
| function_calls | ~46x | ~13x |
| function_calls_closure | ~47x | ~13x |
| objects | ~27x | ~13x |
| gc_alloc | ~11x | ~10x |
| loop_sum | ~18x | ~10x |
| table_array | ~14x | ~8x |
| nested_loop | ~18x | ~12x |
| matrix | ~11x | ~9x |
| hashmap_lookup / string_concat | noise-level | noise-level |

A meaningful across-the-board improvement (roughly 1.5x-3.7x faster than
before this change) without closing the full gap to reference Lua - the
interpreter is still a naive tree-walker with no tiering for the dynamic
`.lua` path, and this fix only removed *unnecessary* boxing, not fixed-cost
interpretation overhead (dispatch, `LuaValue` cloning, dynamic type checks
per operation). Further gains need the tiering/JIT work already scoped for
this phase, not another register-representation change.

## Delivery order and dependencies

1. Complete L0 before interpreting the headline “34 tests” number; it is the
   source of trustworthy oracles and honest exclusions.
2. Deliver L1–L4 in order. Tables depend on dynamic values; closures and
   multiple returns depend on bytecode frames and table construction semantics.
3. Deliver L5 and the precise-GC subset in L6 before coroutines. Metatables add runtime
   indirection and GC adds reference density, both of which coroutine suspension
   must preserve.
4. Treat L7 and L8 as separate releases. No dynamic JIT cache, native code, or
   broader capability profile is a prerequisite for claiming a correct
   interpreter compatibility tier.

The manifest, reproducible reference runner, and initial Lua 5.5 `global`
fixture are implemented. Continue from the earliest unchecked dependency in
this document; keep every added behavior measurable and keep `.sol`'s typed
compiler independent throughout.
