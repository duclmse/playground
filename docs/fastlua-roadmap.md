# fastlua implementation plan & milestone checklist

Detailed, checkbox-level breakdown of `faster_lua.md`'s architecture into the
M1-M7 roadmap summarized in `docs/fastlua.md`. Each milestone doc (below)
lists its goal, which `faster_lua.md` sections it covers, its dependencies, a
concrete task checklist, and the files it touches. M1 is done; this is the
strict, ordered plan for the rest, per the standing project goal ("check
faster_lua.md and start creating plan for intensive compiler and vm, follow
it strictly to implement").

**How to use this**: work milestones in order (each depends on the last).
Within a milestone, tasks are roughly ordered too, but not strictly
sequential - independent items can be done in any order. Check items off as
they land; update the "Early results" section of `docs/fastlua.md` and
`benchmarks/RESULTS.md` whenever a milestone changes measured performance.

## Milestones

- [fastlua-roadmap/m0.md](fastlua-roadmap/m0.md) — Correctness fixes (done)
- [fastlua-roadmap/m1.md](fastlua-roadmap/m1.md) — Minimal typed compiler (done)
- [fastlua-roadmap/m2.md](fastlua-roadmap/m2.md) — Optimizer passes (done)
- [fastlua-roadmap/m3.md](fastlua-roadmap/m3.md) — Structs + escape analysis (done)
- [fastlua-roadmap/m4.md](fastlua-roadmap/m4.md) — GC (done, descoped)
- [fastlua-roadmap/m5.md](fastlua-roadmap/m5.md) — Gradual typing + dynamic mode (done)
- [fastlua-roadmap/m6.md](fastlua-roadmap/m6.md) — Tiered execution (done)
- [fastlua-roadmap/m7.md](fastlua-roadmap/m7.md) — SIMD, PGO, AOT, FFI, tooling (done, scoped)

## Summary checklist (one line per milestone)

- [x] M1: shipped and measured (`docs/fastlua.md`, `benchmarks/RESULTS.md`)
- [x] M0: bounds checking, div-by-zero, and integer-overflow behavior
      closed/documented; negative-length arrays still open
- [x] M2: fastlua-level optimizer passes (opt_level=speed, constant folding,
      bounds-check elimination, inlining) - shipped and measured, honest flat
      result explained
- [x] M3: structs + escape analysis + scalar replacement - shipped and
      IR-verified; real (non-leaked) array allocation deferred to M4
- [x] M4: GC - shipped, descoped to conservative mark-sweep over a chunked
      bump arena (not generational) - see M4 doc for the scope call
- [x] M5: gradual typing + boxed dynamic-value fallback - shipped, scoped to
      `i64`/`f64`/`bool` payloads only
- [x] M6: bytecode tier 0 + hot counters + OSR + speculative `any`-parameter
      specialization (inline-cache guard + guard-before-execute "deopt") -
      shipped; tier 1/baseline and tier 2/optimizing deliberately collapsed
      into one native tier - see M6 doc for why
- [x] M7: SIMD (elementwise `f64` vectorization), profile-guided warm-start,
      AOT binaries (`fastlua build`), FFI (`extern function`), profiling
      tools, and 3 new benchmark categories - shipped; 5 of the doc's 15
      benchmark categories are structurally unreachable given fastlua's
      language (no strings/hashmaps/coroutines/JSON/HTTP) - see M7 doc
