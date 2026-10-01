# U8 — Inline caches and bounded profiling

**Status:** complete

**Purpose:** exploit dynamic facts without native-code dependency.

- [x] Cache field/index, global, arithmetic/metamethod, iterator, and call
      targets. Call-target caching for `Call`/`TailCall`/`TForCall` is
      `closure_parts_cached` (`crate/sol/src/lua_runtime/table.rs:110`), keyed
      on the callee closure's `ObjectId`+generation, caching its
      `(Rc<Proto>, Rc<[Cell<ObjectId>]>)` and skipping the heap borrow plus
      prototype-registry resolve on a hit; `globals_for_closure`'s
      `closure_globals` hash-map lookup is deliberately left uncached even on
      a hit (`CallCacheEntry`'s own doc comment, `instr.rs:70-78`) since
      caching `Globals` itself here would be a `lua_bytecode` → `lua_runtime`
      layering violation - see the benchmark discussion below for why this
      scoping turned out to matter. Field access
      (`GetField`/`SetField`) is `field_cache_get`/`field_cache_set`
      (`table.rs:249,287`) plus the raw-slot resolvers `field_probe_raw`/
      `field_write_raw` (`table.rs:339,384`), keyed on (base `ObjectId`+
      generation, hash-part index) and backed by
      `Heap::table_hash_index_of`/`table_get_at_hash_index`/
      `table_set_at_hash_index` (`crate/sol-core/src/heap.rs:979-1044`), which
      exploit `TableObject.hash`'s append-only `IndexMap` indexing (a key
      never shifts slots once inserted) to turn a cache hit into a plain
      index re-read, skipping `table_key()`'s hashing and the full
      `index_resolve` metamethod-miss path entirely. Global access
      (`GetGlobal`/`SetGlobal`) reuses the same field-cache mechanism against
      the `_ENV` table's own hash part, scoped to the plain-`_ENV` path
      (`frame.globals.has_base() == false`); Sol's own module-isolation
      `base`-chain fast path stays uncached (rarer, more complex invalidation
      story, lower expected payoff - not revisited, no measurement showed it
      mattered). The compile-time-constant-key `GetIndex`/`SetIndex`
      stretch goal and the iterator-target cache were scoped out per the
      plan's own "stretch goal only" framing - no measurement motivated
      building either, and iterator targets were not identified as a
      measured hot path this milestone. Arithmetic/metamethod caching is
      **measured, not adopted**: `find_binary_metamethod`
      (`crate/sol/src/lua_runtime/dispatch.rs:2188`) was benchmarked first
      (`--filter metatable_dispatch`) before writing a cache, per the plan's
      explicit instruction; the existing numeric fast path is already
      branch-only with no metatable touch, so a cache would only matter for
      the metamethod-fallback case, which the benchmark didn't show as
      hot enough to justify a new cache's complexity. A real, much smaller
      fix landed instead: `Option::or`'s eager evaluation meant the original
      `self.metamethod(left, name)?.or(self.metamethod(right, name)?)` always
      resolved the right operand's metamethod even when the left one already
      supplied it (the common case for Lua operator overloading); this is now
      short-circuited with an explicit `if let`, avoiding the redundant
      metatable-fetch/intern/hash-lookup on every binary operator dispatch
      without adding a cache at all.
- [x] Version shapes, metatables, globals, and modules for invalidation. Every
      cache in this milestone is verified-on-use (base object identity +
      generation re-checked on each access, cached hash index re-validated
      against the live key) rather than proactively invalidated by a
      shape/version bump, so "versioning" here means "cheap enough to check
      every time that proactive invalidation isn't needed" - the design
      choice documented in this milestone's own plan
      (`TableObject.hash` never shifting entries, `ObjectId` being
      generation-checked) rather than a separate shape-id system. `_ENV`
      rebinding is the one case with no backing table `version` bump at all
      (a bare `RefCell` write, not a table mutation) - caught because the
      global cache's guard is the `_ENV` table's own `ObjectId`, re-read
      fresh from `Globals::as_value()` on every access rather than cached
      itself, so a reassigned `_ENV` simply presents a different `ObjectId`
      and the next access misses.
- [x] Bound mono/poly/megamorphic transitions. `BoundedCache<T>`
      (`crate/sol/src/lua_bytecode/instr.rs:19-68`) caps every per-call-site
      cache at `IC_SLOTS = 4` entries, round-robin-evicting the oldest on
      overflow; a call site with more distinct identities than that just
      keeps missing and re-inserting, degrading to "always miss, same as an
      uncached interpreter" rather than growing memory without bound. Unit
      test: `bounded_cache_evicts_oldest_once_full`
      (`crate/sol/src/lua_bytecode/tests.rs`) inserts 1000 distinct entries
      and asserts `len() <= IC_SLOTS` throughout. Integration coverage per
      cache kind, including explicit megamorphic-pressure tests asserting
      correctness (not just boundedness) once eviction is happening:
      `tests/lua55_dynamic_runtime_call_cache.rs`,
      `tests/lua55_dynamic_runtime_field_cache.rs`,
      `tests/lua55_dynamic_runtime_global_cache.rs`.
- [x] Serialize type/shape/call/allocation profiles for inspection and PGO.
      Scoped conservatively (YAGNI), per the plan's own instruction: a plain
      counters struct `IcStats` (`crate/sol/src/lua_runtime/ic.rs`), mirroring
      `gc::GcStats`'s existing shape exactly, accumulates cumulative
      hits/misses/evictions per cache kind (call/field/global) across a
      runtime's lifetime, exposed read-only via `debug.icstats()`
      (`crate/sol/src/lua_runtime/natives_debug.rs`). A second native,
      `debug.icprofile()`, dumps a human-readable, recursive, per-call-site
      text report of cache occupancy (`proto.ic_profile_dump`,
      `crate/sol/src/lua_bytecode/instr.rs`), classifying each non-empty
      call site as `mono`/`poly`/`megamorphic` from how many of `IC_SLOTS`
      entries are currently cached - deliberately occupancy-only, with no
      per-call-site hit/miss counters (only the aggregate `debug.icstats()`
      counters track hits/misses), since nothing in the dynamic `.lua` path
      consumes a profile for PGO yet; building a loader/PGO-consumption path
      is explicitly deferred to whichever later milestone first needs it
      rather than built speculatively here. Overhead check (the "same
      pattern as U7's GC-stats check" the plan calls for): hyperfine
      before/after comparisons of a release build with the three
      `ic_record_*` counter calls stubbed to no-ops vs. the real
      implementation, across `hashmap_lookup` (1.00x, no measurable
      difference), `function_calls` (1.04x ± 0.04, within noise), and
      `objects` (1.05x ± 0.08, within noise, direction inverted run to run) -
      no measurable overhead from the counters. Correctness tests:
      `tests/lua55_dynamic_runtime_ic_stats.rs` (8 tests covering per-kind
      hit/miss/eviction counting, mono/poly/megamorphic text classification,
      and the `debug` capability gate both natives share with every other
      `debug.*` function).
- [x] Test alias, metatable, `_ENV`, module, and debug-visible invalidation.
      Alias: `field_cache_alias_write_through_one_reference_is_visible_through_the_cached_read`
      (write through one Lua reference to a table, confirm a cached read
      through a second reference to the same object sees the live value).
      Metatable/`__index`/`__newindex`: `field_cache_does_not_bypass_index_metamethod_after_field_is_set_nil`,
      `field_cache_set_on_new_key_still_charges_and_does_not_skip_newindex`,
      and their `global_cache_*` counterparts. `_ENV`:
      `global_cache_env_reassignment_mid_execution_is_not_served_stale` and
      `global_cache_load_with_custom_env_keeps_each_environments_global_independent`
      (a function reassigning `_ENV` mid-execution, and `load(..., env)`
      giving two closures independent environments through the same call
      site). Debug-visible invalidation: `call_cache_survives_debug_upvaluejoin_aliasing`
      (the plan's identified debug-library hazard - `debug.upvaluejoin`
      aliases upvalue cells across closures, which the call cache's identity
      check must not be fooled by, since a closure's `prototype`/upvalue-cell
      identity never itself changes). Module reload (`package.loaded[name] =
      nil` then re-`require`) was not given a dedicated cache test: `require`
      resolves and mutates `package.loaded` through native Rust table
      operations, not through a `GetField`/`GetGlobal` bytecode instruction
      the inline caches instrument, so the existing alias/newkey field-cache
      tests already cover the only mechanism that could apply if user code
      indexed `package.loaded` directly. Coroutine-sharing a cache is covered
      per kind too (`call_cache_shared_proto_across_two_coroutines_stays_correct`,
      `field_cache_survives_two_coroutines_sharing_one_call_site`,
      `global_cache_survives_two_coroutines_sharing_one_call_site`), since a
      `Proto`'s per-pc cache side table is shared across every fiber that
      runs it by construction.

**Exit gate:** forced invalidation and megamorphic workloads stay bounded and
compatible (clean sweep), while cache-heavy workloads show a **measured,
mixed** result rather than a clean sweep: a controlled multi-run (hyperfine,
8 runs/binary, pre-U8 commit `967f076` vs. this milestone's code) A/B on the
plan's own cited benchmarks found real, reproducible wins on
`coroutine_resume` (-33%) and `vararg_calls` (-8%), a within-noise result on
`objects` (+0.5%, σ overlaps), and small but reproducible *regressions* on
`function_calls` (+4%) and `function_calls_closure` (+2%) - see
[`benchmarks/RESULTS.md`](../../../benchmarks/RESULTS.md)'s U8 section for
the full numbers and discussion of why (the uncached `closure_globals`
hash-map lookup dominates the simple-repeated-call case, so the cache's own
bookkeeping is pure overhead there). Correctness: verified after every item
in this milestone via `cargo test --manifest-path crate/sol/Cargo.toml` (all
suites, including every new test file above), `cargo test --manifest-path
crate/sol-core/Cargo.toml`, `scripts/test-lua55-manifest.sh`,
`scripts/test-sol-c-api.sh`, and `scripts/test-sol-benchmarks.sh` (18
Lua/typed/dynamic-only/typed-only benchmark pairs, 0 regressions) - all
green with zero regressions.

See the [historical U8 ledger](../unified-sol-runtime-plan.md#u8--inline-caches-and-bounded-profiling).
