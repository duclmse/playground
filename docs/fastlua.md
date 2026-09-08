# fastlua

`crates/fastlua` is a typed, Lua-like language compiled straight to native
code via [Cranelift](https://cranelift.dev/). It's a from-scratch answer to
`faster_lua.md` (repo root): can a Lua-like language beat LuaJIT on typed
numeric/data-oriented workloads by leaning on static types and native
compilation instead of trying to out-optimize a dynamic interpreter?

This is a **separate initiative** from `crates/vm`/`crates/lua-vm` (the
browser debugger's Lua-*compatible* VM - see `docs/architecture.md`).
Nothing there changes; fastlua doesn't run real Lua programs and isn't
trying to.

## Status: M2 (M0 mostly folded in)

M1 - lexer → parser → typed AST → type-check → Cranelift IR → JIT → run -
is implemented for a minimal subset: `i64`/`f64`/`bool`, functions, `if`,
`while`, numeric `for`, and `Array<T>`. No structs, no dynamic typing, no
JIT tiers (every function compiles to native code once, ahead of running
it). M2 added real optimization work on top: `opt_level = "speed"`
(Cranelift's own mid-end passes), array bounds checking (an M0 correctness
fix - M1 shipped with none at all), a narrow bounds-check-elimination
pattern, AST-level constant folding, and function inlining. See "Roadmap"
below for what's next.

### Early results

From `benchmarks/RESULTS.md` (hyperfine, whole-process wall time - see that
file for full methodology, the M1→M2 comparison, and caveats): on the two
workloads fastlua currently has equivalent programs for, it beats not just
reference Lua (3.8-8.5×) and this repo's own dynamic VM (19-40×), but
**LuaJIT** (1.15-1.30×). Encouraging, not conclusive: two workloads, one
machine. Notably, M2's optimizer work didn't move either benchmark's
wall-clock time (within measurement noise) - both are documented, understood
non-findings (a memory-bandwidth-bound loop doesn't care about one
well-predicted bounds check; neither benchmark calls anything inlining could
help), not a sign the passes don't work - see `benchmarks/RESULTS.md` for
the direct A/B evidence.

## Language reference (M1)

```
function sum(a: Array<f64>): f64
    local s: f64 = 0.0
    for i = 0, #a - 1 do
        s = s + a[i]
    end
    return s
end

function main(): f64
    local a = new_array_f64(5)
    for i = 0, 4 do
        a[i] = i + 1
    end
    return sum(a)
end
```

- **Types**: `i64`, `f64`, `bool`, `Array<T>`. Nothing else yet (no
  i8/i16/i32/u8-u64/f32/string/struct/enum - see "Roadmap").
- **`local x: T = expr`**: type annotation is optional - `local x = 10`
  infers `i64`, `local y = 20.0` infers `f64` (the inferred type is simply
  the initializer's own type).
- **Every function must declare a return type.** M1 has no `void`
  functions. The entry point `fastlua run` looks for is always a niladic
  `main`, whose return value is printed - there's no `print`/I/O builtin
  in the language itself in M1, so this is the only way a program produces
  an observable result.
- **Implicit `i64 -> f64` widening**: wherever an `i64` value appears where
  `f64` is expected (arithmetic with an `f64` operand, assignment to an
  `f64`-typed slot, an `f64` call argument/return), it's silently widened.
  Every other type mismatch is a hard compile error - in particular, never
  narrows `f64 -> i64`, and never coerces `Bool`/`Array`.
  `%` (modulo) is integer-only (Cranelift has no float-remainder
  instruction, and it's rare enough on floats not to be worth a runtime
  helper yet).
- **`and`/`or` do not short-circuit** in M1 - both operands are always
  evaluated (a plain bitwise and/or on their boolean representation). A
  documented simplification, not an oversight.
- **Arrays**: `new_array_i64(n)` / `new_array_f64(n)` are the only way to
  construct one (not user-callable-looking generics - monomorphic builtins,
  matching `faster_lua.md` §8's "monomorphization over generic dispatch"
  philosophy). `a[i]` reads/writes, `#a` is the length. **Bounds-checked**
  (out-of-range or negative indices trap the process - a real crash, not a
  catchable error, since M1/M2 have no exception mechanism yet). **No GC**
  - every array is a pointer to a small heap header (`runtime.rs`'s
  `ArrayHeader`) allocated via `Box::leak`, for the life of the process.
  This is called out here deliberately: it's M1's explicit, documented
  scope (matching `faster_lua.md`'s own "Phase 1: No GC. No objects."
  recommendation), not something to be surprised by later - a real GC is
  M4 below.
- **Integer arithmetic wraps** on overflow (`iadd`/`isub`/`imul` - matching
  typical systems-language behavior); **integer division/modulo by zero
  traps** the process (confirmed empirically: Cranelift's `sdiv`/`srem`
  already do this on this target, no extra check needed in `codegen.rs`).
- **Comments**: `-- like this`, to the end of the line (matches Lua).

## Architecture

```
source (.fl)
   │
lexer.rs (logos)
   │
parser.rs (hand-written recursive-descent + Pratt expressions)
   │
ast.rs (untyped AST)
   │
typeck.rs (type inference + checking)
   │
types.rs (typed AST - every local resolved to a LocalId)
   │
codegen.rs (cranelift-frontend's FunctionBuilder - does SSA
            construction/phi-node placement for us)
   │
jit.rs (cranelift-jit's JITModule - compiles + finalizes)
   │
native machine code, called directly
```

**Why Cranelift, not a hand-written backend or LLVM**: pure Rust (no C++
toolchain/bindgen to wire into this repo), a first-class JIT story
(`cranelift-jit`), and `cranelift-frontend`'s `FunctionBuilder` implements
SSA construction (automatic phi-node insertion) for us - the central
architectural requirement `faster_lua.md` §5 asks for, without hand-writing
dominance-frontier computation.

### Cranelift API notes (things that weren't obvious from the docs)

Pinned to `cranelift-{codegen,frontend,jit,module} = "0.134"`. A few API
specifics worth recording since they're easy to get wrong from older
examples/tutorials found online:

- `FunctionBuilder::declare_var(ty)` **returns** its own `Variable` (it no
  longer takes a caller-chosen one). fastlua keeps its own `LocalId ->
  Variable` mapping (`codegen.rs`'s `vars: Vec<Variable>`, built once per
  function) rather than assuming any particular numbering.
- `FunctionBuilder::finalize` now takes a `TargetFrontendConfig` argument
  (`module.target_config()`).
- `MemFlags` is not directly constructible in this version (no
  `MemFlags::new()`) - it's an *interned index* into a per-`Function`
  `MemFlagsSet`. But `InstBuilder::load`/`store` actually accept anything
  `Into<MemFlagsData>`, and `MemFlagsData` (the plain descriptor, e.g.
  `MemFlagsData::trusted()`) implements that trivially - so there's no need
  to intern anything into the `MemFlagsSet` at all; just pass
  `MemFlagsData::trusted()` directly at each `load`/`store` call site.
- `icmp_imm`/`imul_imm` (and friends) are deprecated in favor of explicit
  `_s`/`_u` (signed/unsigned) variants.
- `InstBuilder::jump` takes `&[BlockArg]`, not `&[Value]` - wrap a value as
  `BlockArg::Value(v)` (or `.into()`) when jumping to a block with
  parameters (`codegen.rs`'s inliner uses this to pass an inlined
  function's return value into its call site's merge block).
- `JITBuilder::with_flags(&[("opt_level", "speed")], ...)` is the
  supported way to turn on Cranelift's own optimizer - `JITBuilder::new`
  defaults `opt_level` to `"none"` and never mentions this in its own
  docs; `with_flags` also sets `use_colocated_libcalls`/`is_pic` correctly
  for JIT use, which hand-building an `isa`/`Flags` pair and calling
  `JITBuilder::with_isa` directly does not.

## Roadmap

See [fastlua-roadmap.md](./fastlua-roadmap.md) for the detailed,
checklist-level plan (including a known correctness gap - M1's array
indexing has no bounds checking at all - queued as M0). Summary:

M1 is deliberately the smallest useful slice - `faster_lua.md`'s own
closing section ("If I were building it with you") recommends starting
here before anything else. The rest of the document's architecture maps to
these future milestones:

| # | Milestone | Scope |
|---|---|---|
| **M1** | **Minimal typed compiler (done)** | This document's scope above. |
| **M2** | **Optimizer passes (done)** | `opt_level = "speed"`, array bounds checking + a narrow elimination pattern, AST-level constant folding, function inlining. See `benchmarks/RESULTS.md` for the honest before/after. |
| M3 | Structs + real array allocation | Fixed-layout structs, a real array runtime (not a leaked `Vec`), escape analysis + scalar replacement so non-escaping structs/arrays skip allocation entirely. |
| M4 | GC | Generational, bump-allocated young gen + incremental old gen - needed once M3 introduces real heap allocation the language controls. |
| M5 | Gradual typing + dynamic mode | `any`-typed values, runtime type checks, a boxed `Value` fallback path only for genuinely dynamic code - most code stays fully typed/unboxed. |
| M6 | Baseline + optimizing JIT tiers | Bytecode interpreter tier 0, hot counters, tier-1 baseline JIT, tier-2 optimizing JIT with inline caches/speculation/deopt - only worth building once dynamic-mode code exists to benefit from it; the typed/AOT path from M1 doesn't need this. |
| M7 | SIMD, PGO, polish | Vectorization, profile-guided recompilation, FFI. |

## Trying it

```
cargo run -p fastlua -- run crates/fastlua/tests/fixtures/sum_array.fl
cargo test -p fastlua
scripts/benchmark.sh --export-markdown benchmarks/RESULTS.md   # fastlua included wherever a matching .fl exists
```
