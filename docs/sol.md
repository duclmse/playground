# Sol

`crates/sol` is a typed, Lua-compatible language compiled straight to native code
via [Cranelift](https://cranelift.dev/). It's a from-scratch answer to
`faster_lua.md` (repo root): can a Lua-like language beat LuaJIT on typed
numeric/data-oriented workloads by leaning on static types and native
compilation instead of trying to out-optimize a dynamic interpreter?

This is a **separate initiative** from `crates/vm`/`crates/lua-vm` (the browser
debugger's Lua-_compatible_ VM - see `docs/architecture.md`). Nothing there
changes; Sol accepts both `.lua` and `.sol` source files. Lua's fully dynamic
runtime features remain an incremental compatibility target.

## Status

The typed pipeline—lexer, parser, typed AST, validation, optimization,
bytecode, Cranelift JIT/OSR, and AOT—is implemented. Its language surface
includes scalars and strings, arrays, nominal structs, structural records,
scalar-valued maps, typed iteration, type aliases, stateless function values,
nonescaping captured functions, and multi-file typed modules. Safety checks,
escape analysis/scalar replacement, conservative garbage collection, gradual
`any` values with explicit `is`/`as` narrowing, and a specialized array `map`
path are integrated across the applicable execution tiers.

The native toolchain provides `sol build`, scalar FFI, elementwise `f64`
vectorization, profile-guided warm start, introspection flags, wall-clock
profiling, and call-boundary debugging. Uninstrumented execution uses a
different generic monomorphization and contains no debug/profile runtime
branch.

Lua compatibility is an interpreter-first, byte-oriented dynamic runtime kept
separate from typed Sol. A useful subset of tables, closures/upvalues,
varargs/multiple results, iteration, protected calls, metatables, budgets,
coroutines (stackful fibers), and portable libraries is implemented. Complete
libraries, precise dynamic GC, escaping typed closure environments, richer
static algebraic types, and the typed/dynamic module bridge remain open. See
the [feature documentation](features/README.md) for implementation status and
the [language specification](spec/README.md) for observable behavior.

### Feature matrix

This matrix records implemented execution paths; “separate” means the `.lua`
runtime provides Lua semantics without routing values through typed Sol's
unboxed representation.

| Feature | Parser | Bytecode | JIT | OSR | AOT | `any` boundary | `.lua` compatibility |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Scalars, control flow, functions | yes | yes | yes | yes | yes | scalar boxing | separate dynamic values |
| `Array<i64/f64>` and `ipairs` | yes | yes | yes | yes | yes | identity-preserving reference box | Lua tables / `ipairs` |
| Nominal structs | yes | yes | yes | yes | yes | identity-preserving reference box | Lua tables |
| Structural records | yes | yes | yes | yes | yes | identity-preserving reference box | Lua tables |
| `Map<i64, i64/f64/bool>`, literals, `pairs` | yes | yes | yes | yes | yes | identity-preserving reference box | Lua tables / iterator triples |
| Nonescaping captured closures | yes | direct lifted call | yes | yes | yes | not yet | separate heap closures |
| Typed modules and exported records | yes | yes | yes | yes | yes | `.lua` boundary not yet | `require` is separate |
| Type aliases and `is`/`as` narrowing | yes | yes | yes | yes | yes | checked tags | separate dynamic types |
| `map(Array<i64>, fn(i64)->i64)` | yes | specialized | specialized | specialized | specialized | no boxing | Lua library `map` absent |
| Source diagnostics | token spans | function declaration ranges | function declaration ranges | function declaration ranges | function declaration ranges | n/a | parser locations |
| Debug/profiling hooks | n/a | call + source line | promotion events | OSR events | n/a | visible dynamic ops | not yet |

Typed map traversal order is intentionally unspecified. `ipairs(Array<T>)`
uses Sol's zero-based array indices. Missing typed-map keys currently read as
the value type's zero value. Maps support unboxed `i64`, `f64`, and `bool`
values. Keys remain `i64`, and pointer-bearing keys/values remain rejected until
precise GC entry layouts and write barriers make them safe.

### Early results

From `benchmarks/RESULTS.md` (hyperfine, whole-process wall time - see that file
for full methodology, the initial-to-optimized comparison, and caveats): on the two workloads
sol currently has equivalent programs for, it beats not just reference Lua
(3.8-8.5×) and this repo's own dynamic VM (19-40×), but **LuaJIT** (1.15-1.30×).
Encouraging, not conclusive: two workloads, one machine. Notably, the optimizer
work didn't move either benchmark's wall-clock time (within measurement noise) -
both are documented, understood non-findings (a memory-bandwidth-bound loop
doesn't care about one well-predicted bounds check; neither benchmark calls
anything inlining could help), not a sign the passes don't work - see
`benchmarks/RESULTS.md` for the direct A/B evidence.

**This claim is specifically about typed `.sol` programs, not `.lua`
compatibility mode.** Sol's separate dynamic `.lua` interpreter
(`lua_runtime.rs`) is, as of Phase 6's first hyperfine-grade measurement of
it, **26-334× slower than LuaJIT** across call/allocation/control-flow-heavy
workloads, and 1.3-4.2× slower than even this project's own separate,
unoptimized `crates/vm` tree-walking interpreter - see
`benchmarks/RESULTS.md`'s "Phase 6 (L8)" section for the full table. This is
expected: the dynamic path is a first-cut bytecode/tree-walking interpreter
with no tiering, and closing that gap (were it ever prioritized) would mean
building interpreter-level performance work first, not jumping straight to a
tracing/method JIT on top of an interpreter that isn't yet competitive with
this repo's own naive one. The "beats LuaJIT" headline is not, and should not
be read as, a claim about ordinary dynamic Lua code run through `sol run
foo.lua`.

Scalar replacement has its own, more direct verification: dumping the
emitted Cranelift IR (`SOL_DUMP_CLIF=1 sol run ...`) for a `Point`
struct that never escapes its function shows _zero_ allocation, load, or store
instructions - just `f64const`/`fmul`/`fadd`/`return` - versus a real `call` to
the allocator when the same struct is passed to another function
(`crates/sol/tests/programs.rs`'s
`non_escaping_struct_local_has_no_allocation_in_the_emitted_ir` pins this).

The GC went through three results worth keeping, in order. The first cut
(`HashMap`-based bookkeeping, one `std::alloc`/`dealloc` call per object) lost
to LuaJIT on `benchmarks/gc_alloc.sol` (pure allocation churn) by 7.9× - a real,
honest weak spot. A follow-up rewrite to a chunked bump allocator (same file, no
hashing or per-object `malloc`/`free` on the hot path) closed that to roughly
parity. That rewrite then exposed a *different* cost on `table_array` (one
32 MB allocation): 1.26× slower than LuaJIT, versus *beating* it pre-GC -
`SOL_GC_DEBUG` traced it to the array's entire 4,000,000-word data buffer
being conservatively scanned after briefly existing as a stack root
mid-allocation. Fixing that (marking a scalar array's data buffer as
provably pointer-free, so the collector never scans its contents at all - see
"Memory management" below) brought `table_array` back to *beating* LuaJIT (1.46×),
matching or exceeding the pre-GC number. Removing provably-redundant
bounds checks from the hottest per-allocation function (`Chunk::carve`)
looked like a further win but, on rigorous re-measurement (10 rounds,
30+ samples each), turned out to be a genuine dead heat with LuaJIT - an
earlier draft of this section overstated it, corrected here rather than
left standing. A matching attempt at marking *structs* atomic (not just
arrays) measured out as a net loss and was reverted - both real, recorded
non-findings, not silently dropped. What actually broke the tie: `gc.rs`
stopped bulk-zeroing a whole chunk on every reuse, since struct/header
allocations never needed it (only array data does - see "Memory management" below)
- Sol now wins 10/10 rounds on both wall-clock and CPU time, a clear,
non-overlapping gap. See `benchmarks/RESULTS.md`'s collector section for every
number and the bugs (and non-wins) caught along the way.

## Memory management

A real collector, replacing the initial `Box::leak`/`mem::forget`-everything
runtime: `crates/sol/src/gc.rs`. **What it is**: a conservative
(stack-scanning) mark-sweep collector over a **chunked bump arena** - a handful
of large chunks, each bump-allocated into sequentially
(`ptr = bump; bump += size`, no hashing, no per-object `malloc`/`free`); a chunk
found to contain no reachable object after a collection has its bump pointer
reset and is reused whole. Root-finding walks the native call stack (plus
explicitly flushed callee-saved registers - see the file's doc comment for a
real bug this caught) rather than using precise Cranelift stack maps; the
root-scanning technique is the same one the Boehm-Demers-Weiser collector uses
in production, not a toy shortcut. An array's data buffer is allocated via
`sol_gc_alloc_atomic`, not `sol_gc_alloc` - its elements are always
plain `i64`/`f64` scalars, provably never GC pointers, so a collection marks
such a block reachable when found but never reads its contents, avoiding an
O(payload size) conservative scan (array headers and struct instances, which
can hold real pointers, still go through the normal, fully-traced path).
Struct payloads and array headers also skip zero-initialization - every
caller always overwrites every byte immediately (`typeck.rs` enforces this
for structs), so pre-zeroing is pure waste; array *data* buffers still get
zeroed, since sol's language semantics promise unwritten elements read
as zero. **What it deliberately isn't** (yet): true generations (a chunk with even one
long-lived survivor keeps *all* its dead space until the whole chunk dies
together - a documented fragmentation trade-off, not an oversight), promotion,
or write barriers. The scope decision is recorded in [the memory-management
feature document](features/memory-management.md); getting a *correct* and
*fast* collector working was already the substantial part of this work.
`SOL_GC_STATS=1 sol run <file.sol>` prints live block/byte counts at
exit (forcing a final collection first for accuracy); `SOL_GC_DEBUG=1`
traces every collection cycle to stderr, including how many words its
non-atomic tracing touched - both the same env-gated-diagnostic pattern as
`SOL_DUMP_CLIF`.

## Gradual typing

`any` (faster_lua.md §3-4): a value whose type is checked at runtime instead
of compile time, for the boundaries where fully static typing doesn't fit -
`function foo(x: any): any`, or `local x: any = expr`. Scalars, nil, strings,
arrays, records, maps, and stateless function pointers can be boxed; reference
payloads keep their original pointer identity. A concrete value flowing into an `any`-typed slot
gets boxed automatically (a small heap allocation - `value.rs`'s `{tag: i64,
payload: i64}` pair, via the same allocator struct literals use); an `any`
value flowing into a concrete-typed slot gets unboxed with a runtime type
check that **traps** (a real crash, like an out-of-bounds array access) if
the actual type doesn't match. Dynamic arithmetic, comparison, truth,
negation, and concatenation have explicit slow paths. `if x is Point then`
refines `x` on the true branch; `x as Point` is the explicit checked-cast form.
Dynamic indexing/calls/fields still require narrowing first. Strict code is
completely unaffected: boxing/unboxing only ever gets inserted where `any`
appears in the source, so a program that never writes `any` compiles to
*exactly* the same code as if gradual-typing support did not exist - see
[the gradual-typing feature document](features/gradual-typing.md) for how this
is verified. The trade-off is real and measured, not hidden:
`benchmarks/RESULTS.md` shows
a workload with an `any`-typed function boundary running ~7-10× slower than
the identical strict version, almost entirely the cost of one heap allocation
per call - the honest price of opting into dynamic behavior.

## Tiered execution

Every function starts out **interpreted**, not compiled: `bccompile.rs`
lowers each typed function to fixed-width 32-bit bytecode (`bytecode.rs` -
Lua's own iABC/iABx/iAsBx instruction shapes) whose registers are exactly
the typed AST's own `LocalId`s, and `interp.rs` executes that bytecode
directly. Two independent hot counters (`SOL_PROMOTE_THRESHOLD`,
default 200 calls; `SOL_OSR_THRESHOLD`, default 50 loop backedges)
decide when to compile a native version: a function crossing the call
threshold gets promoted (`jit.rs::promote`) via the same typed
Cranelift pipeline that used to run for every function up front, and a
loop crossing the backedge threshold in a function that hasn't promoted
yet gets **on-stack replacement** (`jit.rs::promote_osr`) - a synthetic
native entry point that takes every local as a parameter and jumps
straight into that loop's body, so an already-running interpreted loop
can switch to native mid-iteration rather than waiting for its enclosing
function to return. OSR isn't a nice-to-have here: a `main`-shaped
program (called once, its real work all inside one loop) never crosses
the call-count threshold at all, and `benchmarks/RESULTS.md`'s tiering section
shows the 12x regression that results without it.

There is deliberately only one native tier, not two: `faster_lua.md`'s
"tier 1 baseline JIT" / "tier 2 optimizing JIT" split exists to trade
compile speed for code quality once, then again once more information is
available. Since `jit.rs` already runs the full pipeline (GVN, LICM,
escape analysis, bounds-check elimination, inlining) on every promotion,
a deliberately-worse "baseline" compile would have nothing to be a faster
version *of* - it would just be slower to write and slower to run, with
no follow-up recompile currently designed to improve on it.

**Speculative `any`-parameter specialization** handles
`faster_lua.md`'s deoptimization/inline-caches/speculative-optimization
items (§18-20, §40), deliberately scoped down rather than built as a
general VM deopt mechanism - `any` is checked with a hard runtime
trap, not a speculative guess, so there's no arbitrary native PC to
reconstruct interpreter state from. `jit::speculative_candidate` looks for
a hot function's `any`-typed parameter used *exclusively* as the
immediate operand of one `Unbox` targeting one concrete type throughout
the whole function body (the shape ordinary code like `local y: i64 = x`
already produces) - anything looser (returned as `any`, passed to another
`any` slot, narrowed to more than one type, or reassigned) is left
un-specialized rather than guessed at. Once eligible and called often
enough with a matching argument (`SOL_SPECULATIVE_THRESHOLD`, default
30), `jit::specialize` compiles a variant with that parameter narrowed to
its concrete type, and `interp::Runtime::try_speculative` becomes the
inline cache: it checks the actual argument's tag against the guarded
type *before every call*, dispatching to the specialized native variant
only on a match and falling through to the general (interpreted or
generically-typed native) path otherwise - the "deopt" is just that
fallthrough, not mid-execution state reconstruction, since the guard
always runs before any side effects. The guard is load-bearing for
*correctness*, not just performance: skipping it would let a
wrong-shaped `any` argument's boxed pointer be reinterpreted as a raw
scalar instead of trapping. See
`tests/fixtures/speculative_any_param.sol` and its two tests in
`tests/programs.rs` for both a correctness check (interpreted-general and
guarded-native calls agree on the arithmetic) and proof the specialized
variant is actually compiled (via `SOL_DUMP_CLIF`).

## AOT compilation

`sol build program.sol -o program` compiles straight to a standalone
executable instead of running in-process: `aot.rs` uses
`cranelift-object`'s `ObjectModule` (vs. `run`'s `cranelift-jit`
`JITModule`) to compile every function eagerly - no tiering, since a
standalone binary has no interpreter to fall back to - then links the
resulting object file against this crate's own `staticlib` build (the
crate now also builds as one, exposing `runtime.rs`'s alloc/GC/print
functions as `#[no_mangle]` symbols) via the system `cc`. A small
hand-written C-ABI `main` calls the compiled sol `main` and prints its
result. The produced binary is genuinely standalone (no dependency on the
sol toolchain at runtime) and, with no interpreter warmup to pay,
slightly *beats* the tiered JIT on `benchmarks/table_array.sol` while
staying ~1.5x faster than LuaJIT. Only verified on macOS ARM64 so far -
Linux linking may need extra system libraries not yet checked.

## Profile-guided warm-start

`sol run --profile-out app.prof app.sol` records which functions
actually got promoted/speculatively-specialized during that run (reusing
the existing tiering state - no new instrumentation); `sol run
--profile-in app.prof app.sol` (a later run of the same program) preloads
that list at startup, skipping the interpreted warm-up that produced it
the first time. A real, working slice of `faster_lua.md` §23's larger PGO
vision - not the full one (no type-distribution/branch-probability/
allocation-site profiling yet, and it doesn't feed `sol build`'s AOT
compile) - see [the native-toolchain feature document](features/native-toolchain.md)
for the fuller design that
would.

## Loop vectorization

`codegen.rs`'s `try_vectorize_elementwise_loop` recognizes exactly `for i =
start, stop do out[i] = a[i] OP b[i] end` (step 1, `out`/`a`/`b` all
`Array<f64>`, `OP` one of `+ - * /`) and emits Cranelift's own vector type
(`F64X2`, portable IR - no hand-rolled per-ISA intrinsics) 2 lanes at a
time, with a scalar tail for any odd leftover element and one whole-range
bounds check up front instead of a per-element one. A narrow pattern
match, not general auto-vectorization - matching the optimizer's bounds-check-
elimination precedent. Measured a real but modest ~1.06-1.10× on
`benchmarks/vector_add.sol`: that loop is memory-bandwidth-bound, not
compute-bound, so halving the arithmetic instruction count doesn't halve
wall-clock time - see `benchmarks/RESULTS.md`'s vectorization section for the honest
accounting.

## Debugging and profiling

`interp::Runtime`/`tier::Engine` are generic over `H: interp::Hooks`
(default `()`) - a trait with two empty-by-default methods
(`on_call_enter`/`on_call_exit`) that `Runtime::call` invokes around every
call, whatever tier ends up handling it. `sol run`'s only code path
instantiates `Engine<()>`, whose hook calls are the no-op `()` impl and
inline away entirely - a genuinely different compiled function from any
real hooks implementation, not a runtime-checked flag. This is the
mechanism behind both new commands:

- **`sol run --profile-time <out>`**: `profile.rs`'s `TimingHooks`
  times every call, reporting inclusive wall time (including callees) and
  call count per function. Honest limitation, confirmed empirically: it
  only sees calls routed through `Runtime::call` - once a function is
  promoted to native and recurses via Cranelift's own direct call
  instruction, those calls are invisible (`fib.sol` showed 221 calls to
  `fib` once promoted vs. the true ~7 million with promotion disabled).
  Avoiding that cost is exactly what keeps the *disabled* path free -
  instrumenting native call sites too would need either two compiled
  versions of every function or a runtime branch inside JIT'd code.
- **`sol debug <file.sol>`**: `debug.rs`'s `DebugHooks` is a REPL
  debugger - `break <fn>` / `continue` / `step` / `backtrace` / `quit` -
  pausing at function-call boundaries. Works uniformly across every tier
  (interpreted or promoted) since the hook wraps the call boundary itself,
  before tier dispatch. Scoped to call-boundary granularity, not
  per-source-line stepping (real line-level breakpoints need source lines
  threaded through the typed AST into bytecode, complicated by optimizer and escape-analysis
  optimizer passes reordering statements after type-checking - a
  substantially larger undertaking); `args`/return values print as raw
  `u64` bits, not type-formatted, since there's no live type info at a
  call boundary.

Verified via both a structural argument (different monomorphizations) and
an empirical one: `benchmarks/RESULTS.md`'s debugging section compares
uninstrumented binaries on four benchmark shapes, all within measurement
noise - see [the debugging feature document](features/debugging-and-profiling.md)
for the full writeup.

## Language overview

The normative reference is [`docs/spec/`](spec/README.md). This section is a
compact usage overview.

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

- **Function declarations**: `fn` is a shorter Sol-only alias for `function`,
  including `export fn`, `extern fn`, and `local fn`. In `.lua` source, `fn`
  remains an ordinary identifier for compatibility.
- **Types**: `i64`, `f64`, `bool`, `Array<T>`, named `struct`s, and typed
  top-level function values. Use `fn(Args) -> Return` (or the equivalent
  `fn(Args): Return`) in annotations. Top-level and stateless nested functions
  can be assigned and called through typed locals. A nested function may also
  capture `i64`, `f64`, `bool`, or `string` locals when all calls are direct;
  capture conversion passes the current value as an unboxed parameter and
  allocates no closure object, so an enclosing reassignment is observed by the
  next call. Escaping captured functions and assignments from inside a closure
  remain rejected until their two-word code/environment representation and
  precise GC layout are available.
- **Modules**: `import math.pipeline` resolves `math/pipeline.sol` relative to
  the importer, then falls back to `.lua`. `export function` and `export
  struct` define the typed interface. Qualified calls, annotations, and struct
  constructors are supported. The compiler loads a canonical dependency graph,
  rejects missing/private/indirect/cyclic imports, and runs typed top-level
  initializers once in dependency order before root `main`. A typed `.sol`
  import of a dynamic `.lua` interface remains an explicit future `any`
  boundary rather than silently sharing Sol's unboxed representation.
- **`struct Name { field: Type, ... }`** (top-level, alongside functions):
  `Name { field = expr, ... }` constructs one (fields in any order - they're
  reordered to declaration order internally), `value.field` reads and writes.
  Every field must be given in a literal - no partial initialization/defaults in
  the struct implementation. Struct-typed locals that never leave their function (never returned,
  passed to a call, stored into an array/another struct, or reassigned as a
  whole) are compiled with **zero heap allocation** at all - see "Early results"
  above - everything else is a small, `sol_alloc`-backed heap block,
  garbage-collected (see "Memory management" below).
- **`local x: T = expr`**: type annotation is optional - `local x = 10` infers
  `i64`, `local y = 20.0` infers `f64` (the inferred type is simply the
  initializer's own type).
- **Blocks**: `do ... end` and `{ ... }` both create a lexical scope for local
  declarations. Braces are a statement-level form, so `Name { field = expr }`
  remains a struct literal.

Run `scripts/test-lua55-suite.sh` to execute every top-level case from a local
`lua-5.5.1-tests` checkout through Sol. Its checked manifest reports pending
language features separately from declared host-required C API, CLI, native
module, filesystem, and locale cases; it retains per-case logs when
`SOL_LUA55_RESULTS_DIR` is set. The staged compatibility plan and reference
runner are in [the Lua compatibility feature document](features/lua-compatibility.md)
and `tests/lua55/README.md`.
- **Dynamic `.lua` execution** uses a separate `LuaValue` interpreter with byte
  strings, tables, closures/shared environments, varargs/multiple results,
  iterator triples, protected calls, same-block labels/gotos, table
  metatables, and focused base/string/table/math/utf8 libraries. Recursion,
  instruction/backedge, and heap-object allocation budgets are enforced. This
  does not box or add runtime branches to ordinary typed `.sol` code.
- **Every function must declare a return type.** Sol has no `void` functions. The
  entry point `sol run` looks for is always a niladic `main`, whose return
  value is printed - there's no `print`/I/O builtin in the language itself in
  typed core, so this is the only way a typed program produces an observable result.
- **Implicit `i64 -> f64` widening**: wherever an `i64` value appears where
  `f64` is expected (arithmetic with an `f64` operand, assignment to an
  `f64`-typed slot, an `f64` call argument/return), it's silently widened. Every
  other type mismatch is a hard compile error - in particular, never narrows
  `f64 -> i64`, and never coerces `Bool`/`Array`. `%` (modulo) is integer-only
  (Cranelift has no float-remainder instruction, and it's rare enough on floats
  not to be worth a runtime helper yet).
- **`and`/`or` short-circuit** in bytecode and native code. Sol also lowers
  compound `value is Type and value.field ...` conditions as nested branches,
  making the narrowed local available to the right operand and the body.
- **Arrays**: `new_array_i64(n)` / `new_array_f64(n)` are the only way to
  construct one (not user-callable-looking generics - monomorphic builtins,
  matching `faster_lua.md` §8's "monomorphization over generic dispatch"
  philosophy). `a[i]` reads/writes, `#a` is the length. **Bounds-checked**
  (out-of-range or negative indices trap the process - a real crash, not a
  catchable error, since there's no exception mechanism yet). Every array is a
  pointer to a small heap header (`runtime.rs`'s `ArrayHeader`),
  garbage-collected (see "Memory management" below); the original runtime's
  process-lifetime leaks no longer apply.
- **Integer arithmetic wraps** on overflow (`iadd`/`isub`/`imul` - matching
  typical systems-language behavior); **integer division/modulo by zero traps**
  the process (confirmed empirically: Cranelift's `sdiv`/`srem` already do this
  on this target, no extra check needed in `codegen.rs`).
- **Comments**: `-- like this`, to the end of the line (matches Lua).
- **`extern function name(params): T`** (`faster_lua.md` §25 FFI): declares a native
  symbol already loaded in the process (typically libc/libm), resolved via
  `dlsym` - no body, and `name` is also the C symbol name. Calling one
  compiles to a plain direct call, same as calling a sol function. Only
  `i64`/`f64`/`bool` cross the FFI boundary; no explicit per-library
  `ffi.load` - resolution is against the process's global symbol table.

## Architecture

```
source (.sol)
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
jit.rs (cranelift-jit's JITModule - compiles + finalizes)      aot.rs (cranelift-object's ObjectModule)
   │                                                               │
native machine code, called directly                     .o -> `cc` -> standalone executable
```

`run` and `build` share everything up to and including `types.rs`/`codegen.rs`
- only the last step (which `cranelift-module` backend, and whether
compilation is lazy/tiered vs. eager) differs. See "AOT compilation"
below.

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
  longer takes a caller-chosen one). sol keeps its own `LocalId -> Variable`
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
- `is_pic` must be the *opposite* between the two backends: the JIT path
  wants `false` (it maps its own memory, no PIE requirement), but
  `ObjectModule` (AOT, `aot.rs`) needs `true` - macOS's linker rejects text
  relocations in a PIE main executable otherwise (`Illegal text-relocations`
  at link time, not a runtime crash).

## Documentation map

The [feature index](features/README.md) links implementation rationale, status,
verification notes, and remaining work. The [language specification](spec/README.md)
defines observable behavior without tying it to a delivery sequence.

| Area | Implementation document |
| --- | --- |
| Compiler and correctness | [Compiler pipeline](features/compiler-pipeline.md), [safety](features/safety.md), [optimization](features/optimization.md) |
| Data and memory | [Records and escape analysis](features/records-and-escape-analysis.md), [memory management](features/memory-management.md) |
| Dynamic boundaries and execution | [Gradual typing](features/gradual-typing.md), [tiered execution](features/tiered-execution.md) |
| Native tooling | [Native toolchain](features/native-toolchain.md), [debugging and profiling](features/debugging-and-profiling.md) |
| Language/runtime expansion | [Feature delivery plan](features/delivery-plan.md), [Lua compatibility](features/lua-compatibility.md) |

## Trying it

```
cargo run --manifest-path crates/sol/Cargo.toml -- run crates/sol/tests/fixtures/extension_probe.sol
cargo test --manifest-path crates/sol/Cargo.toml
scripts/benchmark.sh --export-markdown benchmarks/RESULTS.md   # Sol is included wherever a matching .sol or legacy .sol exists

sol run --dump-ir/--dump-asm/--jit-log/--target-info <file.lua|file.sol>   # compiler introspection
cargo build --manifest-path crates/sol/Cargo.toml && ./crates/sol/target/debug/sol build <file.lua|file.sol> -o <out>   # AOT: needs the sibling libsol.a `cargo build` produces

sol run --profile-time report.txt <file.lua|file.sol>   # wall-clock profile
sol debug <file.lua|file.sol>                           # call-boundary REPL debugger (break/continue/step/backtrace/quit)
sol run --diagnostic-format json <file.lua|file.sol>    # editor-friendly structured diagnostics
```
