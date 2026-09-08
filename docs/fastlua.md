# fastlua

`crates/fastlua` is a typed, Lua-like language compiled straight to native code
via [Cranelift](https://cranelift.dev/). It's a from-scratch answer to
`faster_lua.md` (repo root): can a Lua-like language beat LuaJIT on typed
numeric/data-oriented workloads by leaning on static types and native
compilation instead of trying to out-optimize a dynamic interpreter?

This is a **separate initiative** from `crates/vm`/`crates/lua-vm` (the browser
debugger's Lua-_compatible_ VM - see `docs/architecture.md`). Nothing there
changes; fastlua doesn't run real Lua programs and isn't trying to.

## Status: M4 (M0/M2 folded in)

M1 - lexer → parser → typed AST → type-check → Cranelift IR → JIT → run - is
implemented for `i64`/`f64`/`bool`, functions, `if`/`while`/numeric `for`, and
`Array<T>`. M2 added real optimization work: `opt_level = "speed"` (Cranelift's
own mid-end passes), array bounds checking (an M0 correctness fix - M1 shipped
with none at all), a narrow bounds-check- elimination pattern, AST-level
constant folding, and function inlining. M3 added fixed-layout `struct`s and
escape analysis + scalar replacement - verified at the IR level to remove heap
allocation entirely for a struct that never leaves its function. M4 replaced the
leak-everything runtime with a real garbage collector - see "GC (M4)" below for
what it is and, just as importantly, what it deliberately isn't yet. No dynamic
typing, no JIT tiers yet (every function still compiles to native code once,
ahead of running it). See "Roadmap" below for what's next.

### Early results

From `benchmarks/RESULTS.md` (hyperfine, whole-process wall time - see that file
for full methodology, the M1→M2 comparison, and caveats): on the two workloads
fastlua currently has equivalent programs for, it beats not just reference Lua
(3.8-8.5×) and this repo's own dynamic VM (19-40×), but **LuaJIT** (1.15-1.30×).
Encouraging, not conclusive: two workloads, one machine. Notably, M2's optimizer
work didn't move either benchmark's wall-clock time (within measurement noise) -
both are documented, understood non-findings (a memory-bandwidth-bound loop
doesn't care about one well-predicted bounds check; neither benchmark calls
anything inlining could help), not a sign the passes don't work - see
`benchmarks/RESULTS.md` for the direct A/B evidence.

M3's scalar replacement has its own, more direct verification: dumping the
emitted Cranelift IR (`FASTLUA_DUMP_CLIF=1 fastlua run ...`) for a `Point`
struct that never escapes its function shows _zero_ allocation, load, or store
instructions - just `f64const`/`fmul`/`fadd`/`return` - versus a real `call` to
the allocator when the same struct is passed to another function
(`crates/fastlua/tests/programs.rs`'s
`non_escaping_struct_local_has_no_allocation_in_the_emitted_ir` pins this).

M4's GC went through three results worth keeping, in order. The first cut
(`HashMap`-based bookkeeping, one `std::alloc`/`dealloc` call per object) lost
to LuaJIT on `benchmarks/gc_alloc.fl` (pure allocation churn) by 7.9× - a real,
honest weak spot. A follow-up rewrite to a chunked bump allocator (same file, no
hashing or per-object `malloc`/`free` on the hot path) closed that to roughly
parity. That rewrite then exposed a *different* cost on `table_array` (one
32 MB allocation): 1.26× slower than LuaJIT, versus *beating* it pre-GC -
`FASTLUA_GC_DEBUG` traced it to the array's entire 4,000,000-word data buffer
being conservatively scanned after briefly existing as a stack root
mid-allocation. Fixing that (marking a scalar array's data buffer as
provably pointer-free, so the collector never scans its contents at all - see
"GC (M4)" below) brought `table_array` back to *beating* LuaJIT (1.46×),
matching or exceeding the pre-GC number. Removing provably-redundant
bounds checks from the hottest per-allocation function (`Chunk::carve`)
looked like a further win but, on rigorous re-measurement (10 rounds,
30+ samples each), turned out to be a genuine dead heat with LuaJIT - an
earlier draft of this section overstated it, corrected here rather than
left standing. A matching attempt at marking *structs* atomic (not just
arrays) measured out as a net loss and was reverted - both real, recorded
non-findings, not silently dropped. What actually broke the tie: `gc.rs`
stopped bulk-zeroing a whole chunk on every reuse, since struct/header
allocations never needed it (only array data does - see "GC (M4)" below)
- fastlua now wins 10/10 rounds on both wall-clock and CPU time, a clear,
non-overlapping gap. See `benchmarks/RESULTS.md`'s M4 section for every
number and the bugs (and non-wins) caught along the way.

## GC (M4)

A real collector, replacing M1-M3's `Box::leak`/`mem::forget`-everything
runtime: `crates/fastlua/src/gc.rs`. **What it is**: a conservative
(stack-scanning) mark-sweep collector over a **chunked bump arena** - a handful
of large chunks, each bump-allocated into sequentially
(`ptr = bump; bump += size`, no hashing, no per-object `malloc`/`free`); a chunk
found to contain no reachable object after a collection has its bump pointer
reset and is reused whole. Root-finding walks the native call stack (plus
explicitly flushed callee-saved registers - see the file's doc comment for a
real bug this caught) rather than using precise Cranelift stack maps; the
root-scanning technique is the same one the Boehm-Demers-Weiser collector uses
in production, not a toy shortcut. An array's data buffer is allocated via
`fastlua_gc_alloc_atomic`, not `fastlua_gc_alloc` - its elements are always
plain `i64`/`f64` scalars, provably never GC pointers, so a collection marks
such a block reachable when found but never reads its contents, avoiding an
O(payload size) conservative scan (array headers and struct instances, which
can hold real pointers, still go through the normal, fully-traced path).
Struct payloads and array headers also skip zero-initialization - every
caller always overwrites every byte immediately (`typeck.rs` enforces this
for structs), so pre-zeroing is pure waste; array *data* buffers still get
zeroed, since fastlua's language semantics promise unwritten elements read
as zero. **What it deliberately isn't** (yet): true generations (a chunk with even one
long-lived survivor keeps *all* its dead space until the whole chunk dies
together - a documented fragmentation trade-off, not an oversight), promotion,
or write barriers - descoped with reasoning recorded in
`docs/fastlua-roadmap.md`'s M4 section, since getting a *correct* and *fast*
collector working was already the substantial part of this milestone.
`FASTLUA_GC_STATS=1 fastlua run <file.fl>` prints live block/byte counts at
exit (forcing a final collection first for accuracy); `FASTLUA_GC_DEBUG=1`
traces every collection cycle to stderr, including how many words its
non-atomic tracing touched - both the same env-gated-diagnostic pattern as
`FASTLUA_DUMP_CLIF`.

## Language reference (M1-M3)

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

- **Types**: `i64`, `f64`, `bool`, `Array<T>`, and named `struct`s. Nothing else
  yet (no i8/i16/i32/u8-u64/f32/string/enum/generics - see "Roadmap").
- **`struct Name { field: Type, ... }`** (top-level, alongside functions):
  `Name { field = expr, ... }` constructs one (fields in any order - they're
  reordered to declaration order internally), `value.field` reads and writes.
  Every field must be given in a literal - no partial initialization/defaults in
  M3. Struct-typed locals that never leave their function (never returned,
  passed to a call, stored into an array/another struct, or reassigned as a
  whole) are compiled with **zero heap allocation** at all - see "Early results"
  above - everything else is a small, `fastlua_alloc`-backed heap block,
  garbage-collected since M4 (see "GC (M4)" below).
- **`local x: T = expr`**: type annotation is optional - `local x = 10` infers
  `i64`, `local y = 20.0` infers `f64` (the inferred type is simply the
  initializer's own type).
- **Every function must declare a return type.** M1 has no `void` functions. The
  entry point `fastlua run` looks for is always a niladic `main`, whose return
  value is printed - there's no `print`/I/O builtin in the language itself in
  M1, so this is the only way a program produces an observable result.
- **Implicit `i64 -> f64` widening**: wherever an `i64` value appears where
  `f64` is expected (arithmetic with an `f64` operand, assignment to an
  `f64`-typed slot, an `f64` call argument/return), it's silently widened. Every
  other type mismatch is a hard compile error - in particular, never narrows
  `f64 -> i64`, and never coerces `Bool`/`Array`. `%` (modulo) is integer-only
  (Cranelift has no float-remainder instruction, and it's rare enough on floats
  not to be worth a runtime helper yet).
- **`and`/`or` do not short-circuit** in M1 - both operands are always evaluated
  (a plain bitwise and/or on their boolean representation). A documented
  simplification, not an oversight.
- **Arrays**: `new_array_i64(n)` / `new_array_f64(n)` are the only way to
  construct one (not user-callable-looking generics - monomorphic builtins,
  matching `faster_lua.md` §8's "monomorphization over generic dispatch"
  philosophy). `a[i]` reads/writes, `#a` is the length. **Bounds-checked**
  (out-of-range or negative indices trap the process - a real crash, not a
  catchable error, since there's no exception mechanism yet). Every array is a
  pointer to a small heap header (`runtime.rs`'s `ArrayHeader`),
  garbage-collected since M4 (see "GC (M4)" below) - M1-M3 leaked every
  allocation for the life of the process; that's no longer true.
- **Integer arithmetic wraps** on overflow (`iadd`/`isub`/`imul` - matching
  typical systems-language behavior); **integer division/modulo by zero traps**
  the process (confirmed empirically: Cranelift's `sdiv`/`srem` already do this
  on this target, no extra check needed in `codegen.rs`).
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
(`cranelift-jit`), and `cranelift-frontend`'s `FunctionBuilder` implements SSA
construction (automatic phi-node insertion) for us - the central architectural
requirement `faster_lua.md` §5 asks for, without hand-writing dominance-frontier
computation.

### Cranelift API notes (things that weren't obvious from the docs)

Pinned to `cranelift-{codegen,frontend,jit,module} = "0.134"`. A few API
specifics worth recording since they're easy to get wrong from older
examples/tutorials found online:

- `FunctionBuilder::declare_var(ty)` **returns** its own `Variable` (it no
  longer takes a caller-chosen one). fastlua keeps its own `LocalId -> Variable`
  mapping (`codegen.rs`'s `vars: Vec<Variable>`, built once per function) rather
  than assuming any particular numbering.
- `FunctionBuilder::finalize` now takes a `TargetFrontendConfig` argument
  (`module.target_config()`).
- `MemFlags` is not directly constructible in this version (no
  `MemFlags::new()`) - it's an _interned index_ into a per-`Function`
  `MemFlagsSet`. But `InstBuilder::load`/`store` actually accept anything
  `Into<MemFlagsData>`, and `MemFlagsData` (the plain descriptor, e.g.
  `MemFlagsData::trusted()`) implements that trivially - so there's no need to
  intern anything into the `MemFlagsSet` at all; just pass
  `MemFlagsData::trusted()` directly at each `load`/`store` call site.
- `icmp_imm`/`imul_imm` (and friends) are deprecated in favor of explicit
  `_s`/`_u` (signed/unsigned) variants.
- `InstBuilder::jump` takes `&[BlockArg]`, not `&[Value]` - wrap a value as
  `BlockArg::Value(v)` (or `.into()`) when jumping to a block with parameters
  (`codegen.rs`'s inliner uses this to pass an inlined function's return value
  into its call site's merge block).
- `JITBuilder::with_flags(&[("opt_level", "speed")], ...)` is the supported way
  to turn on Cranelift's own optimizer - `JITBuilder::new` defaults `opt_level`
  to `"none"` and never mentions this in its own docs; `with_flags` also sets
  `use_colocated_libcalls`/`is_pic` correctly for JIT use, which hand-building
  an `isa`/`Flags` pair and calling `JITBuilder::with_isa` directly does not.

## Roadmap

See [fastlua-roadmap.md](./fastlua-roadmap.md) for the detailed, checklist-level
plan (including a known correctness gap - M1's array indexing has no bounds
checking at all - queued as M0). Summary:

M1 is deliberately the smallest useful slice - `faster_lua.md`'s own closing
section ("If I were building it with you") recommends starting here before
anything else. The rest of the document's architecture maps to these future
milestones:

| #      | Milestone                            | Scope                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------ | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **M1** | **Minimal typed compiler (done)**    | This document's scope above.                                                                                                                                                                                                                                                                                                                                                                                 |
| **M2** | **Optimizer passes (done)**          | `opt_level = "speed"`, array bounds checking + a narrow elimination pattern, AST-level constant folding, function inlining. See `benchmarks/RESULTS.md` for the honest before/after.                                                                                                                                                                                                                         |
| **M3** | **Structs + escape analysis (done)** | Fixed-layout structs, escape analysis + scalar replacement of aggregates - IR-verified to remove heap allocation entirely for a non-escaping struct. A real (non-leaked) array runtime is deferred to M4, which needs a GC anyway.                                                                                                                                                                           |
| **M4** | **GC (done, descoped)**              | Conservative (stack-scanning) mark-sweep over a chunked bump arena, not the originally-planned generational/write-barrier design - see `docs/fastlua-roadmap.md`'s M4 section for the scope call and reasoning, and `benchmarks/RESULTS.md` for the full before/after (an initial `HashMap`-based cut lost to LuaJIT 7.9× on pure allocation churn; the bump-arena rewrite closed that to roughly parity; an atomic-allocation fix for scalar array data then fixed a second regression it exposed, restoring fastlua's pre-GC win on `table_array`). |
| M5     | Gradual typing + dynamic mode        | `any`-typed values, runtime type checks, a boxed `Value` fallback path only for genuinely dynamic code - most code stays fully typed/unboxed.                                                                                                                                                                                                                                                                |
| M6     | Baseline + optimizing JIT tiers      | Bytecode interpreter tier 0, hot counters, tier-1 baseline JIT, tier-2 optimizing JIT with inline caches/speculation/deopt - only worth building once dynamic-mode code exists to benefit from it; the typed/AOT path from M1 doesn't need this.                                                                                                                                                             |
| M7     | SIMD, PGO, polish                    | Vectorization, profile-guided recompilation, FFI.                                                                                                                                                                                                                                                                                                                                                            |

## Trying it

```
cargo run -p fastlua -- run crates/fastlua/tests/fixtures/sum_array.fl
cargo test -p fastlua
scripts/benchmark.sh --export-markdown benchmarks/RESULTS.md   # fastlua included wherever a matching .fl exists
FASTLUA_DUMP_CLIF=1 cargo run -p fastlua -- run <file.fl>       # dump each function's Cranelift IR to stderr
FASTLUA_GC_STATS=1 cargo run -p fastlua -- run <file.fl>        # print live block/byte counts at exit
FASTLUA_GC_DEBUG=1 cargo run -p fastlua -- run <file.fl>        # trace every collection cycle to stderr
```
