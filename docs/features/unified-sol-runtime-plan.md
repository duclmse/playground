# Final-goal plan: one Lua-compatible Sol runtime, typed specialization, and integrated tooling

> Status: accepted convergence roadmap; U0 and U1 completed 2026-09-15; U3,
> U4, and U5 completed 2026-09-16; U2's production-object migration remains in progress. This document
> defines the intended end state and the order for future work. It does not change the current language
> contract by itself; `docs/spec/` and executable tests remain the description
> of released behavior until each milestone below lands.
>
> This roadmap supersedes the future architecture and performance direction in
> [the earlier Lua-superset execution plan](lua-superset-plan.md). Completed
> work recorded there remains valid baseline history.

## 1. Final goal

Sol will be a source-compatible superset of Lua 5.5 with optional, gradually
adopted types and Sol extensions. Untyped Lua, partially typed Sol, and fully
typed Sol will:

1. use the same Lua semantics, object model, heap, garbage collector, module
   graph, errors, coroutines, libraries, and callable ABI;
2. execute through a shared bytecode and optimization pipeline;
3. retain multiple internal representations so proven typed values stay unboxed
   and do not pay for dynamic dispatch;
4. use static inference, annotations, runtime profiles, guards, and
   deoptimization to progressively remove dynamic checks;
5. outperform the pinned LuaJIT baseline under the reproducible release gates in
   this document, while preserving compatibility;
6. power the native CLI, browser playground/debugger, LSP, and a first-party VS
   Code extension from the same frontend and semantic runtime.

The concise product promise is:

> Every supported Lua 5.5 program is a Sol program. Adding types improves
> diagnostics and makes performance more predictable; it does not move the
> program onto a different runtime or change its Lua behavior.

## 2. Terms and compatibility contract

### 2.1 “Same runtime”

“Same runtime” means one semantic implementation and one identity domain for
runtime objects. It does **not** mean that every value has the same physical
representation in every execution tier.

- Dynamic values use a compact tagged `Value` representation.
- Proven `i64`, `f64`, and boolean values may live unboxed in interpreter/JIT
  registers and native call frames.
- Typed records and arrays may use specialized layouts when escape and alias
  analysis prove that doing so is unobservable.
- A value crossing to dynamic Lua is boxed through a checked adapter that
  preserves object identity.
- A dynamic value crossing into typed code is checked once at the boundary; the
  typed body then relies on the checked contract.
- Tables, strings, closures, userdata, threads, errors, and all values visible
  through Lua APIs belong to the same managed heap regardless of the source file
  that created them.

There must not be a “typed GC” and “Lua GC” that can disagree about reachability
or identity, nor separate module instances for typed and untyped callers.

### 2.2 Language profiles

The compatibility claim is split into explicit profiles so sandbox restrictions
are not confused with semantic incompatibility.

| Profile                   | Required final behavior                                                                                                                                                                        |
| ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Core language             | Lua 5.5 lexical, parsing, numeric, control-flow, function, closure, vararg, multi-return, table, metatable, error, coroutine, and `_ENV` semantics                                             |
| Portable standard library | Base, coroutine, package, string, utf8, table, math, and capability-independent debug behavior                                                                                                 |
| Native host               | Lua-compatible CLI behavior, filesystem-backed loading, `io`, `os`, locales, binary chunks, native modules, and declared process capabilities                                                  |
| Embedding                 | Version-targeted Lua 5.5 C API/ABI or an explicitly documented source-compatible embedding boundary; Sol must not say “fully runtime-compatible” until this decision is implemented and tested |
| Browser sandbox           | The same language/runtime semantics with filesystem, process, native loading, and unrestricted debug access denied through explicit capabilities                                               |
| LuaJIT extensions         | FFI, `jit.*`, Lua 5.1 compatibility quirks, and LuaJIT-specific bytecode are a separate optional profile, not implied by Lua 5.5 compatibility                                                 |

The reference language revision, standard-library behavior, upstream corpus
revision, LuaJIT version, and benchmark builds must be pinned in checked-in
metadata. Changing a pin is a reviewed compatibility change.

### 2.3 Source forms

- `.lua` accepts standard Lua 5.5 and defaults to compatibility diagnostics.
- `.sol` accepts every valid `.lua` program plus optional Sol syntax.
- Sol-only words such as `fn`, `struct`, and `type` must be contextual rather
  than unconditional keywords wherever reserving them would reject valid Lua.
- Renaming a valid file from `.lua` to `.sol` must not change its behavior.
- The extension may enable additional syntax, editor defaults, or type-checking
  policy, but may not select a different runtime or different Lua semantics.
- Source and Lua strings remain byte-oriented; compatibility cannot depend on
  valid UTF-8.

### 2.4 Type-checking modes

Type policy is independent from execution semantics:

| Mode     | Behavior                                                                                                                                      |
| -------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `off`    | Parse and execute Lua; collect optimization profiles but publish no optional type diagnostics                                                 |
| `infer`  | Infer types and publish non-blocking diagnostics; uncertainty remains dynamic                                                                 |
| `strict` | Enforce explicit contracts and requested strict regions; unannotated Lua constructs remain valid unless the program opts into a stronger rule |

An annotation is a checked contract, not an unchecked optimization hint. A
contract violation originating in dynamic Lua becomes a normal catchable Lua
error. A failed speculative optimization guard deoptimizes and is not a source
error. Native traps are reserved for compiler bugs and typed operations whose
specified contract is not catchable.

### 2.5 LuaJIT relationship and scope boundaries

Sol will not translate LuaJIT's implementation line by line. It will build a
Lua-compatible tiered VM in Rust and adopt measured techniques such as compact
tagged values, fast tables, inline caches, type feedback, trace/region
specialization, side exits, and low-overhead machine-code generation. Rust is an
implementation and safety choice, not a performance feature by itself.

The performance comparator and the compatibility oracle have different jobs: PUC
Lua 5.5 defines the targeted language behavior, while a pinned LuaJIT build
defines the performance baseline. LuaJIT-specific FFI, `jit.*`, bytecode, and
Lua 5.1 compatibility behavior enter scope only through the separate profile in
section 2.2. Sol must not copy a LuaJIT shortcut that changes the selected Lua
5.5 semantics merely to improve a benchmark.

Other boundaries for this roadmap:

- one semantic runtime does not require one physical representation;
- a native JIT is not required inside the browser sandbox;
- the final benchmark gate does not allow compatibility waivers;
- capability-denied host operations are supported sandbox behavior, not silent
  omissions from the native compatibility profile;
- existing Piccolo and legacy Sol runtimes remain test oracles during migration,
  not permanent production alternatives hidden behind file extensions.

## 3. Target architecture

```mermaid
flowchart TB
  LUA[.lua source] --> FE[Byte-oriented superset frontend]
  SOL[.sol source plus optional types] --> FE
  FE --> AST[Shared AST and Lua semantic analysis]
  AST --> INF[Flow-sensitive type and effect inference]
  INF --> BC[Unified bytecode with dynamic and specialized opcodes]
  BC --> I[Portable interpreter]
  BC --> B[Baseline native JIT]
  BC --> O[Optimizing SSA JIT / AOT]
  PROF[Runtime type, shape, and call profiles] --> O
  ANN[Checked annotations] --> INF
  I --> RT[Shared Value / heap / GC / tables / closures / threads]
  B --> RT
  O --> RT
  RT --> WEB[WASM runtime and browser debugger]
  AST --> LSP[LSP analysis]
  LSP --> VSC[VS Code client]
```

### 3.1 Repository boundaries

The intended code ownership is:

- `crates/sol-core`: byte-oriented frontend, AST, semantic analysis, type
  inference, unified bytecode, portable interpreter, runtime object model,
  libraries, GC interfaces, debugger events, and capability model. This crate
  must build without Cranelift or native OS access.
- `crates/sol`: native CLI, Cranelift baseline/optimizing tiers, AOT, native
  capability providers, profiler, and debugger commands. During migration,
  existing `crates/sol` modules move behind `sol-core` APIs incrementally; do
  not begin with a big-bang directory move.
- `crates/sol-wasm`: `wasm-bindgen` wrapper around `sol-core`'s interpreter,
  virtual modules, budgets, debugger session, profiler events, and serialized
  inspection values.
- `crates/sol-lsp`: project analysis built exclusively on `sol-core`; it must
  not reimplement token, scope, or type logic.
- `packages/sol-runtime`: generated/browser package replacing
  `@lua-playground/runtime` after parity.
- `apps/web`: runtime-independent Monaco/debugger UI consuming
  `packages/sol-runtime`.
- `editors/vscode-sol`: first-party VS Code extension that launches or locates
  `sol-lsp` and registers `.lua` and `.sol` documents.

The vendored `crates/vm` fork has been retired. `crates/lua-vm` and its DAP
consumer remain outside the canonical Cargo workspace as migration history
until the `sol-core` WASM adapter replaces their browser-facing interfaces.

### 3.2 Runtime value and heap model

The shared runtime must provide:

- a measured compact `Value` representation; NaN boxing is a candidate, not a
  foregone conclusion, and must be compared against an explicit tag/payload
  layout on all supported architectures;
- canonical integer/float behavior, including overflow, conversion, equality,
  hashing, NaN, and negative-zero rules matching the target Lua revision;
- interned or otherwise allocation-efficient byte strings with content equality
  and stable GC ownership;
- one table implementation with array/hash parts, identity, weak modes,
  metatables, versioning, iteration invalidation rules, and shape metadata;
- closures with shared mutable upvalue cells and `_ENV` represented as a real
  upvalue rather than a parallel global map;
- heap-resident resumable frames for coroutines, tail calls, yields, protected
  calls, debugger suspension, and deoptimization;
- userdata and native closures with capability-aware host handles;
- errors and stack traces represented as runtime values and propagated without
  Rust panics crossing interpreter/JIT/FFI boundaries;
- a tracing collector with precise interpreter roots and generated native stack
  maps, generational barriers, weak tables, ephemeron processing, finalization,
  and safe coroutine scanning.

Rust safety is a boundary, not a claim that the JIT and GC contain no `unsafe`.
Unsafe code must be isolated behind reviewed value, heap, executable-memory, and
stack-map modules with invariant tests and Miri/sanitizer jobs where those tools
apply.

### 3.3 Calls and mixed execution

All functions have a common semantic calling convention supporting arbitrary
argument counts, varargs, multiple results, tail calls, errors, and yields.
Execution tiers may use faster internal ABIs when a call target and signature
are proven, but must generate adapters to the semantic ABI.

Required call combinations are:

| Caller              | Callee              | Required behavior                                                                  |
| ------------------- | ------------------- | ---------------------------------------------------------------------------------- |
| dynamic interpreted | dynamic interpreted | Full Lua calls, varargs, multi-return, tail calls, yields                          |
| dynamic interpreted | typed native        | Boundary guards once, catchable type error, then unboxed body                      |
| typed native        | dynamic interpreted | Box arguments, preserve identity, accept dynamic result or check a declared result |
| typed native        | typed native        | Direct specialized ABI when proven; semantic adapter when callable dynamically     |
| JIT                 | native/FFI          | Capability and GC-safe transition with stack maps and error translation            |
| coroutine           | any tier            | Resume through a trampoline or deopt to a resumable interpreter frame              |

Compiled frames require metadata sufficient to reconstruct bytecode state at
every guard, allocation safepoint, call, yield, and debugger-visible source
location.

### 3.4 Unified bytecode and SSA

The bytecode must represent both generic Lua operations and proven
specializations. For example, generic `ADD` retains coercion/metamethod
semantics, while `ADD_I64` is legal only with proof or a dominating guard.
Specialized bytecode is an optimization of the same function, not a different
language.

The optimizing SSA IR needs first-class operations for:

- tagged dynamic arithmetic, comparisons, truthiness, concatenation, indexing,
  field access, calls, and iteration;
- unboxed scalar and specialized aggregate operations;
- type, shape, metatable-version, global-version, and call-target guards;
- allocations, write barriers, safepoints, and precise roots;
- effect summaries for calls, metatables, globals, aliases, coroutines, debug
  hooks, and FFI;
- deoptimization snapshots mapping SSA values back to bytecode registers and
  inlined frames;
- OSR entry and side-exit blocks;
- source locations retained through optimization.

Cranelift remains the initial native backend. A custom assembler or trace
backend is considered only after profiles show Cranelift compile latency or
generated code is the limiting factor; it is not a prerequisite for the unified
architecture.

## 4. Type inference and specialization

### 4.1 Keep proof separate from observation

Every inferred fact carries one of these strengths:

1. **Proven:** derived from syntax, constants, dominance, checked annotations,
   escape/effect analysis, or a completed boundary check. No runtime guard is
   needed while its dependencies remain valid.
2. **Guarded:** derived from a versioned assumption such as a stable global,
   table shape, metatable, or call target. Generated code contains a guard and
   deoptimization snapshot.
3. **Observed:** runtime profile data used to choose a specialization. It is
   never consumed as proof until a generated guard validates it.
4. **Unknown:** execute the generic Lua operation.

This distinction must be visible in optimization diagnostics so a developer can
see why an operation stayed dynamic or why a guard exists.

### 4.2 Static analysis sequence

Run these analyses on a control-flow graph before native compilation:

1. lexical binding and upvalue resolution, including real `_ENV` handling;
2. SSA construction and definite-assignment facts;
3. literal and assignment propagation;
4. flow-sensitive union and nilability narrowing;
5. numeric-kind/range inference where it preserves Lua overflow semantics;
6. return and local call-signature inference;
7. closure escape, alias, mutation, and effect analysis;
8. table-literal shape and field-type inference;
9. loop induction and invariant analysis;
10. allocation escape/scalar-replacement eligibility;
11. guard planning and invalidation dependencies;
12. representation selection and lowering.

The initial type lattice should include `never`, `nil`, booleans and optional
literal facts, integer, float, number, byte string, function signatures, table
shapes, thread, userdata, small unions, and unknown/dynamic. Widen large or
unstable unions predictably instead of allowing compile-time explosion.

### 4.3 Flow-sensitive examples

Literal-derived locals require no tag checks while they remain unaliased:

```lua
local x = 10
local y = 20
return x + y
```

An explicit Lua check is also an optimization guard:

```lua
if math.type(x) == "integer" then
    return x + 1
end
```

Table shapes may be proven only while mutation and escape analysis make the
assumption unobservable:

```lua
local point = { x = 10.0, y = 20.0 }
return point.x + point.y
```

If `point` escapes, gains a metatable, or is reachable through an unknown alias,
the compiler must retain generic operations or install shape/metatable guards. A
typed/sealed record may opt into a stronger contract, but exposing it as an
ordinary Lua table requires a boxing/view strategy that preserves the specified
identity and mutation behavior.

### 4.4 Runtime profiling

Profiles are collected per bytecode site and must be bounded in memory. Track:

- operand and result tags;
- table shapes and metatable versions;
- global binding versions;
- call targets and argument/result signatures;
- branch direction and loop trip counts;
- allocation types, survival, and escape behavior;
- guard failures, deoptimizations, and polymorphism degree.

Use monomorphic specialization first, a small bounded polymorphic cache second,
and a stable megamorphic generic path after the site exceeds its shape/target
budget. Do not repeatedly compile an unbounded number of variants.

### 4.5 Annotation rules

- Missing annotations mean dynamic Lua, not a static error.
- Local annotations constrain assignments after a checked initialization.
- Parameter and return annotations are enforced at dynamic entry/exit.
- Inferred types may be more precise internally but are not public contracts.
- Explicit `any` denotes a dynamic `Value` and does not allocate a new box on
  every assignment when the value is already dynamic.
- `is`, `as`, and standard Lua tests feed the same narrowing engine.
- Metatable and debug APIs may invalidate guarded facts but may not invalidate
  proven facts without crossing a contract boundary.
- An optimizer must not use an annotation until type checking has established
  its soundness for all reachable entries.

## 5. Execution tiers

### Tier 0: portable interpreter

The interpreter is the semantic reference used by native and WASM builds. It
must be stackless/resumable, allocation-efficient, debugger-visible, and able to
execute specialized opcodes while falling back to generic Lua behavior.

### Tier 1: baseline native JIT

Compile hot functions quickly with minimal optimization. Inline monomorphic
caches and lower generic operations to small runtime stubs. This tier exists to
remove dispatch overhead and provide a fast bridge while Tier 2 compiles.

### Tier 2: optimizing JIT

Lift hot functions or regions to SSA, consume proven and observed types, inline
stable callees, scalar-replace allocations, specialize table access, hoist
guards, vectorize safe loops, and emit complete deoptimization metadata.

Start with method/function compilation because it reuses the current Cranelift
pipeline. Add region or trace formation for hot loops only when measurements
show function compilation cannot match LuaJIT on important dynamic workloads.

### AOT

AOT uses the same optimizing SSA and runtime ABI. Fully proven code can omit
guards; mixed/dynamic code retains runtime stubs and deoptimization or generic
fallback entries. AOT may use profile-guided specialization, but the executable
must remain correct when the deployment profile differs from the training run.

### Browser

The browser runs Tier 0 compiled to WebAssembly. Native executable-memory JIT is
not a browser requirement. A later WebAssembly code-generation tier may be
added, but browser semantic parity, debugging, and responsiveness take priority
over matching native LuaJIT throughput.

## 6. Compatibility and correctness strategy

### 6.1 Test layers

Every semantic change must have the narrowest applicable combination of:

- parser/AST unit tests in both `.lua` and `.sol` forms;
- type-inference tests that assert proven, guarded, observed, or unknown facts;
- bytecode snapshots or structural assertions;
- interpreter behavior tests;
- baseline-JIT and optimizing-JIT tier-agreement tests;
- OSR and deoptimization tests that deliberately fail guards after side effects;
- GC stress tests at every allocation/safepoint and mixed-tier boundary;
- differential tests against pinned Lua 5.5;
- differential fuzzing of parser, bytecode, runtime values, tables, functions,
  errors, and coroutine schedules;
- native/WASM parity tests for the portable capability profile;
- LSP fixtures using the same source and semantic facts.

Tests must distinguish “the process did not crash” from matching output, errors,
side effects, finalizer behavior, and exit status.

### 6.2 Compatibility gates

The current 34-file upstream manifest is a baseline inventory, not a pass
metric. Milestones promote unchanged upstream files only after comparison with
the pinned reference build.

Final native compatibility requires:

- zero unexplained `pending` or `diverges` entries in the targeted language and
  portable-library corpus;
- all capability-independent upstream cases passing unchanged;
- capability-dependent cases passing under the matching native profile;
- bytecode, debug, CLI, C API/embedding, locale, filesystem, and native-module
  cases either passing the declared full-runtime profile or explicitly keeping
  the public claim narrower than “fully compatible runtime”;
- no allowlist entry without an owner, reason, target milestone, and regression
  test;
- random differential suites completing with no untriaged mismatch.

The hand-written typed Sol conformance fixtures remain useful feature tests but
must be named and reported as typed capability regressions, never as evidence
that unmodified Lua is compatible.

### 6.3 Security and capabilities

Runtime providers expose filesystem, process, environment, time, locale, native
loading, stdin/stdout, and debug authority explicitly. The browser and library
default is deny; the native `sol` CLI can opt into a documented Lua-like host
profile. A capability failure must be deterministic and testable and must not
cause a semantic fork elsewhere in the runtime.

## 7. Performance strategy and release gates

### 7.1 Benchmark suites

Maintain three separately reported suites:

1. **Untyped compatibility:** unchanged Lua programs exercising numeric loops,
   calls, recursion, closures/upvalues, varargs, tables, polymorphic fields,
   metatables, strings, patterns, modules, errors, coroutines, and GC.
2. **Gradually typed:** the same programs with increasing annotation coverage,
   measuring each step rather than substituting unrelated typed rewrites.
3. **Typed/data-oriented:** arrays, records, numeric kernels, allocation,
   callbacks, modules, and real applications that can exploit unboxed layouts.

Include microbenchmarks for diagnosis and application-sized workloads for the
claim. A benchmark with no LuaJIT-equivalent behavior may inform Sol tuning but
cannot support the comparative headline.

### 7.2 Measurement protocol

- Pin source, inputs, Lua/LuaJIT commits or releases, build flags, CPU
  architecture, OS, power mode, and compiler toolchain.
- Report cold whole-process latency, warm steady-state throughput, compilation
  time, p50/p95/p99 latency, peak RSS, allocated bytes, GC time/pauses, code
  size, guard failures, and deoptimization counts.
- Use both x86-64 and AArch64 release machines.
- Preserve raw samples and machine-readable summaries.
- Compare identical source and inputs for untyped claims.
- Run correctness before timing and reject samples from semantically divergent
  executions.
- Require an A/B benchmark and profiler evidence before merging a representation
  or JIT optimization.

### 7.3 Progressive gates

| Gate                    | Required result                                                                                                                                                                                                                          |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Interpreter readiness   | Faster than the pinned reference Lua geometric mean on the untyped suite and no longer slower than the Piccolo-based sibling VM on any core category                                                                                     |
| Baseline-JIT readiness  | At least 0.75x LuaJIT throughput geometric mean on hot untyped workloads, with bounded compile latency and no correctness waiver                                                                                                         |
| Dynamic parity          | At least LuaJIT geometric-mean throughput, with no individual application workload more than 20% slower and no material memory/pause regression hidden from the report                                                                   |
| Final performance claim | At least 1.10x LuaJIT geometric-mean throughput on the published untyped application suite and no category geometric mean below parity; typed/annotated results are reported separately and must show monotonic or explained improvement |
| Typed advantage         | Fully typed/data-oriented suite exceeds LuaJIT by a separately published target while preserving mixed-call and compatibility behavior                                                                                                   |

These numbers are release criteria, not permission to overfit. If the final gate
is not met, the release says which gate it reached and does not claim to be
faster than LuaJIT. Startup-heavy and throughput-heavy claims remain separate.

## 8. Milestone sequence

Each milestone is independently reviewable. Its implementation, tests,
benchmarks, documentation, and migration notes land together. A later milestone
may prototype early, but may not declare an earlier exit gate complete.

### U0 — Charter, baselines, and decision records — **complete**

**Purpose:** stop architectural drift before more implementation work.

Deliverables:

- [x] adopt this document as the authoritative future roadmap;
- [x] update the product brief, architecture, specification introduction,
  `AGENTS.md`, and CLI documentation to the unified goal;
- [x] record decisions for target Lua revision, C API scope, contextual extension
  syntax, common call ABI, runtime crate boundary, JIT strategy, and benchmark
  gates;
- [x] rename/reclassify typed “conformance” reporting as typed capability tests;
- [x] capture current compatibility, interpreter, JIT, web, and LSP baselines in
  machine-readable reports;
- [x] add a cross-component CI summary that cannot present classified/pending tests
  as passing compatibility.

Exit gate: all top-level documents describe one product and link to the same
compatibility/performance definitions; baseline commands reproduce on a clean
checkout.

Delivered in U0:

- unified the product brief, architecture overview, repository guides, spec
  introduction, CLI overview, and historical-roadmap pointers;
- accepted ADRs 0001-0006 under `docs/decisions/`;
- captured `baselines/u0-2026-09-15.json`, explicitly distinguishing the last
  complete dynamic benchmark suite from later targeted remeasurements;
- added `scripts/project-status.sh` with text/JSON output, classification
  validation, and a strict full-compatibility gate;
- added `scripts/test-project-status.sh` to prevent typed capability ports from
  being counted as oracle-backed Lua compatibility;
- reclassified the legacy `sol-conformance` suite as typed capability
  regressions in documentation, test names, and script output;
- wired LSP checks and the unified status gate into `scripts/test.sh`.

Verification at completion: `scripts/test.sh` passed the native Sol suite,
browser runtime/conformance suite, strict Sol/LSP Clippy, manifest/status
checks, focused Lua fixtures, typed benchmark smoke, and web production build.
The honest baseline remains 0/34 oracle-backed upstream passes, 26 pending, and
8 host-required; U0 records that gap rather than changing runtime behavior.

### U1 — Superset frontend and semantic AST — **complete**

**Purpose:** make all Lua source valid input to the Sol frontend without yet
changing runtime implementation.

Deliverables:

- [x] replace extension-driven parser forks with a common Lua grammar plus
  contextual Sol extensions;
- [x] separate `Dialect`/extension flags, type-check policy, host capabilities, and
  execution tier instead of overloading `SourceMode`;
- [x] preserve byte spans and original spelling through AST and diagnostics;
- [x] implement a real lexical binder for locals, upvalues, labels, `_ENV`, and
  nested scopes;
- [x] make `.lua` and annotation-free `.sol` produce semantically equivalent ASTs;
- [x] expose parser/binder APIs to `sol-lsp`.

Exit gate: every parser fixture and unchanged upstream source accepted in `.lua`
is also accepted when treated as `.sol`; AST differential tests show no semantic
mode fork; invalid Sol annotations produce located diagnostics.

Completed 2026-09-15. `LanguageConfig` now separates the Lua 5.5 dialect, Sol
extension recognition, and type policy; compatibility `SourceMode` adapters no
longer select parser productions. The lossless parse result retains token
lexemes and byte spans, the shared binder models lexical scopes, upvalues,
labels, and `_ENV`, and `sol-lsp` consumes both APIs. The frontend differential
gate covers every parseable checked-in Lua fixture, benchmark, sibling-VM
script, and pinned Lua 5.5 upstream source in both profiles, as well as
contextual identifiers and located annotation errors.

The former Sol-only `{ ... }` statement block was removed because it is
syntactically indistinguishable from Lua's newline-insensitive `callee { ... }`
call sugar. Scoped statement blocks use Lua's equivalent `do ... end` form.

### U2 — Canonical runtime object model and GC foundation — **in progress**

**Purpose:** establish one identity/reachability domain before merging execution
tiers.

Deliverables:

- [x] introduce the canonical runtime `Value`, object headers/handles, strings,
  tables, closures, upvalues, threads, userdata, errors, and capabilities;
- [x] replace dedicated globals with `_ENV` table/upvalue semantics;
- [x] define root registration and stack-map interfaces before optimizing layouts;
- [ ] migrate dynamic libraries and metatables onto the canonical objects;
- [x] add precise tracing, barriers, weak/ephemeron rules, finalizer queues, and
  coroutine roots;
- [x] provide temporary adapters for existing `LuaValue` and typed heap objects so
  migration can proceed without a flag day.

Exit gate: identity, weak reference, finalization, coroutine, and mixed-adapter
stress tests pass under forced collection; no production object is owned by two
independent collectors.

Started 2026-09-15. The portable `sol-core` crate now defines the canonical
tagged value, generation-checked object handles, all planned managed object
kinds, explicit host capabilities, `_ENV` closure upvalues, precise registered
roots/stack maps, generational metadata and barriers, weak tables, ephemerons,
finalizer queues, and coroutine tracing. Focused forced-collection tests cover
these invariants. A transitional adapter preserves legacy scalar, string,
table, closure, shared-upvalue, `_ENV`, standard-library native-callable,
registered native-bridge, stateful-iterator, userdata, coroutine,
coroutine-wrapper, raised-error, cycle, and repeated-reference identity when
importing old `LuaValue` graphs, and preserves unboxed typed scalars at the
typed boundary. Coroutine imports flatten all live frame values into traced
thread roots but intentionally remain non-resumable snapshots until U3 defines
the unified executable frame ABI. Canonical native callables carry portable
provider/function registry IDs and traced captures, never raw host pointers;
heap-issued provider namespaces prevent collisions across adapters.

The production Lua runtime now uses `sol_core::Capabilities` directly instead
of maintaining a second coarse `os`/`io` authority type. Clock, environment,
process, stdin, and stdout effects are independently gated, the native CLI opts
into the declared `NATIVE_CLI` profile, and library embedding remains
sandboxed by default.

U2 remains in progress because the production Lua interpreter and typed runtime
still own objects in their existing `Rc` and arena collectors. The adapter is a
migration seam, not a second production owner: imported canonical graphs are
snapshots and must not be mutated concurrently with legacy graphs. Dynamic
libraries, native callables, closures, userdata, coroutine frames, and the
production global environment still need to move onto canonical handles before
the exit gate can be claimed. See
[canonical-runtime-foundation.md](canonical-runtime-foundation.md).

### U3 — Unified bytecode, frames, and semantic call ABI — **completed 2026-09-16**

**Purpose:** execute typed and untyped functions in one resumable engine.

Deliverables:

- [x] converge `lua_bytecode` and typed bytecode into one function/prototype model
  with generic and specialized opcodes;
- [x] standardize varargs, multiple results, tail calls, errors, protected calls,
  yield/resume, and source maps;
- [x] use heap-resident/trampolined frames that can suspend for coroutine, debugger,
  or deoptimization;
- [x] implement all dynamic↔typed call combinations and identity-preserving
  boxing/unboxing adapters;
- [x] route `sol run` through this engine for both extensions;
- [x] retain an old/new differential switch until the unified path is stable.

Exit gate: annotation-free `.lua` and `.sol` run through the same runtime path;
mixed modules call, error, tail-call, yield, and collect correctly in every
direction; the legacy partition/fallback path is no longer the default.

Completed 2026-09-16. `sol-core` now defines the tier-independent function IDs,
prototype metadata, arity, value-count, call-site, call-kind, frame-state, and
call-outcome contracts, including tail redispatch, returned/yielded/raised
transitions, portable native-callable IDs, a shared function registry, checked
boundary values, and instruction source maps. Generic Lua and specialized Sol
bytecode both implement the same executable-prototype interface and project
their call instructions onto the same semantic ABI.
The generic compiler no longer stores Lua's open argument/result convention as
an untyped `-1` sentinel, and its resumable frames now carry stable function
IDs and canonical ready/running/suspended/returned state. The typed interpreter
publishes the same frame-state transitions to its existing zero-cost hooks.
Both compilers emit explicit semantic tail calls: generic Lua closure frames are
replaced in the trampoline, and specialized bytecode redispatches in a loop, so
deep proper tail recursion does not consume native or heap frame depth. U3
also routes annotation-free `.lua` and `.sol` through the same generic
runtime based on AST type surface rather than filename; an explicit annotation
selects specialization without changing the semantic object runtime. This exposed and fixed dynamic
integer-loop overflow, negative-divisor floor arithmetic, and minimum-integer
shift discrepancies.

The specialized dispatcher now hosts generic Lua functions as semantic slots,
so typed bytecode can call dynamic code and propagate normal results, catchable
errors, proper tail calls, and coroutine yield/resume outcomes without a second
calling convention. The dynamic dispatcher performs the reverse call through
registered pointer-free callable IDs; raw machine pointers remain only in its
host-side provider registry and never enter values, tables, frames, or canonical
snapshots. Scalar guards use the common boundary adapter, while canonical
handles cross unchanged to preserve identity. The CLI selects this mixed path
for an eligible typed caller of a dynamic function and retains
`SOL_RUNTIME_PATH=legacy-partition` only as the differential switch. Unsupported
non-scalar specialization and reentrant call graphs safely remain generic; U5
extends their optimized layouts rather than defining another runtime.

The exit gate is covered by annotation-free `.lua`/`.sol` routing tests,
dynamic-to-typed call and protected-error tests, typed-to-dynamic source and
error tests, deep cross-tier tail recursion, a real dynamic coroutine
yield/resume through a specialized slot, canonical callable collection tests,
and the legacy/unified differential test. The old raw-pointer value bridge is
no longer the installed production representation.

### U4 — Gradual type system and sound static inference — **completed 2026-09-16**

**Purpose:** remove checks from ordinary Lua using proof, without rejecting
dynamic programs.

Deliverables:

- [x] implement the type lattice, small unions, widening rules, flow narrowing, nil
  elimination, return inference, and local call signatures;
- [x] add CFG/SSA-based escape, alias, mutation, and effect analysis;
- [x] infer local table shapes and nonescaping closure signatures;
- [x] make annotations optional contracts and add `off`/`infer`/`strict` policies;
- [x] emit optimization explanations for proven and remaining dynamic operations;
- [ ] replace heap-allocating `any` transitions with the canonical tagged value
  wherever possible.

Exit gate: inference fixtures show that literals, dominated type tests, loop
indices, local functions, and nonescaping table literals remove redundant
checks; all annotation-free compatibility fixtures still execute unchanged.

Completed 2026-09-16. `typeck::inference` supplies a bounded union lattice,
truthy/type-test narrowing, nil elimination, loop widening, fixed-point return
and nonescaping-local-call inference, table shapes, and structured CFG join/phi
summaries. Escape, alias, mutation, and effect facts are conservative: an
unknown call invalidates a table shape instead of making program validity depend
on an optimization. The AST retains whether each parameter annotation was
written, so an omitted Lua parameter may specialize while explicit `any`
remains a dynamic contract. The CLI exposes `--type-policy off|infer|strict`
and `--explain-types`; policies never change Lua semantics.

The U3 semantic boundary already represents dynamic values with
`sol_core::Value` and passes proven scalars through `BoundaryValue::Unboxed`, so
mixed-tier scalar transitions allocate no `any` box. The older typed-only IR
continues to use its two-word `any` allocation internally; replacing that
isolated representation requires U5's typed-layout/identity work and is not on
the annotation-free unified path. Executable fixtures in
`crates/sol/tests/fixtures/inference` cover every exit-gate proof, compare
`off`/`infer` results, and the complete compatibility suite remains the
semantic regression gate.

### U5 — Typed layouts and mixed-module specialization — **completed 2026-09-16**

**Purpose:** retain the current typed compiler’s strongest performance features
inside the unified runtime.

Deliverables:

- [x] lower proven scalars, arrays, records, maps, and closure environments to
  specialized layouts;
- [x] preserve identity when a specialized object becomes dynamically visible;
- [x] scalar-replace nonescaping table/record literals where Lua observation cannot
  detect the allocation;
- [x] generate semantic ABI adapters and direct typed ABIs;
- [x] make imports/`require` share one module cache and support typed contracts over
  dynamic exports;
- [x] generate precise GC layouts and barriers for specialized objects.

Exit gate: existing typed benchmark IR retains unboxed hot paths; mixed-module
fixtures pass without duplicate modules or collectors; adding unused dynamic
support causes no generated-code change to a fully proven function.

Implemented scope: proven scalars remain unboxed; arrays and scalar maps keep
their specialized buffers; typed records carry pointer-slot masks and are
scalar-replaced when nonescaping; nonescaping closure captures are
lambda-lifted as hidden typed parameters. Reference payloads placed in `any`
retain their original pointer identity. Native and typed-bytecode allocation
now emit the same precise layouts, while legacy callers deliberately use the
conservative sentinel. Dynamic `.lua` modules expose a typed import contract
only when every parameter and result is explicitly annotated; their bodies
remain generic and the boundary checks scalar arguments/results. Canonical
paths deduplicate transitive imports, and the loaded namespace is published
through the same `package.loaded` entry used by `require`.

The exit gate is executable: mixed fixtures require the unified dispatcher and
compare repeated `require` identity, a diamond dependency contains one module
body, contract violations fail at the adapter, and an unused dynamic function
leaves a proven `main`'s bytecode and constants byte-for-byte unchanged. The
typed IR regression script continues to reject boxing/dynamic calls in hot
benchmark functions. This does not complete U2: generic Lua tables/closures
still have transitional `Rc` ownership even though mixed calls share the U3
semantic ABI and module identity.

### U6 — Lua 5.5 compatibility completion

**Purpose:** complete semantics before making final performance claims.

Deliverables:

- [ ] close remaining grammar, coercion, `_ENV`, goto/scope, `<close>`, metamethod,
  iteration, coroutine, error, and GC-observable gaps;
- [ ] complete portable standard libraries and debug behavior;
- [ ] implement native filesystem/package/`io`/`os`/locale profiles;
- [ ] implement Lua 5.5 binary chunk load/dump compatibility where required;
- [ ] execute the embedding/C API decision from U0, including native module tests;
- [ ] promote manifest rows only through unchanged reference comparisons.

Exit gate: the compatibility gates in section 6.2 pass for the declared full
runtime profile. If the embedding profile remains incomplete, public wording
must remain source-compatible rather than fully runtime-compatible.

#### U6 exit ledger (2026-09-21)

The manifest is the release ledger, not a substitute for this gate. It
currently reports 9/34 unchanged upstream cases as oracle-backed passes, with
11 implementation-pending rows, 10 rows that require the declared native host
profile, and 4 documented divergences. U6 is complete only when all of the
following are delivered and the corresponding unchanged rows compare cleanly:

- portable runtime: arbitrary/debug-visible `_ENV` upvalues, full debug
  metadata and hooks, exact diagnostic/chunk-name formatting, remaining
  grammar and `string.pack` coverage, and default-budget behavior for bounded
  corpus programs;
- GC/runtime identity: tracing-style observable collection and finalization
  semantics rather than the transitional `Rc`/cycle-collector approximation;
- native profile: filesystem/locale/stdio behavior, Lua binary chunks, the
  embedding/C API decision, and native module loading/tests;
- intentional divergences: replace invocation/model representations with the
  declared native behavior, or remove the divergence by matching the upstream
  invocation path.

No manifest classification or public compatibility claim may be widened ahead
of an unchanged pinned-oracle comparison.

Current U6 progress: the numeric coercion path now shares source-literal
decimal/hex/hex-float parsing without applying that coercion to comparisons;
mixed integer/float ordering is exact at the i64 boundary and treats NaN as an
unordered numeric value. The portable math surface includes Lua 5.5's
xoshiro256** generator with reference seed vectors, and the UTF-8 library now
matches the unchanged upstream case in a pinned differential run. Named
varargs bind a live, mutable auto-packed table whose `n` controls subsequent
`...` expansion; loaded Lua chunks are variadic and share the default global
environment; shared string metatables make the unchanged `bwcoercion.lua`
case match; and `gsub` observes empty-match progression, table `__index`, and
source identity reuse. Runtime errors record bytecode source lines, and the
frame header's program counter now stays in step with the instruction being
executed (not just the last explicit suspension point), so an error that
propagates without crossing a call boundary attributes the right source line
instead of a stale one. Bitwise/shift "no integer representation" errors
annotate their offending operand's field name when it was just loaded by a
table-field access (e.g. `math.huge << 1` reports "field 'huge'"), matching
real Lua's `getobjname`-derived wording via a narrow backward bytecode scan
rather than general debug-name tracking. `tonumber` on a decimal string
exactly at the most-negative-integer boundary (`"-9223372036854775808"`) now
converts to that exact integer instead of an imprecise float, mirroring real
Lua's unsigned-accumulate-then-negate string-to-integer conversion. The
unchanged `bwcoercion.lua`, `utf8.lua`, `pm.lua`, and `vararg.lua` cases all
match the pinned Lua 5.5.1 oracle, giving four promoted rows; `math.lua` is
close but still blocked on hex-float parsing for very long numerals; the
remaining U6 rows stay pending at their next observed blocker.

The unchanged `coroutine.lua` case now also matches the pinned Lua 5.5.1
oracle. Weak-value tables no longer retain a suspended `coroutine.wrap`
result merely through its internal continuation; `table.unpack` rejects a
one-million-result range before allocating a result list; and the portable
debug library implements `debug.setupvalue` for Lua closures. This promotes
the row without relaxing its source or oracle comparison.

`package.path`, `package.cpath`, `package.preload`, `package.config`, and a
real `package.searchpath` now exist (honest Sol-specific defaults, since
there is no installed share/lib prefix: `package.path =
"./?.lua;./?/init.lua"`, `package.cpath = ""`), and `require`'s
module-not-found error matches real Lua 5.5.1 byte-for-byte, including the
`'package.path' must be a string`-style error when `package.path`/`cpath`
holds a non-string. This was `attrib.lua`'s real blocker (the manifest's
prior note was stale). Chasing it further into the file surfaced and fixed a
second, unrelated bug: `Stmt::MultiAssign` used to resolve each target's own
table/key sub-expression one at a time, interleaved with that target's own
store, so an earlier target reassigning a variable could corrupt a later
target's addressing in the same statement; every target's addressing is now
resolved before any of the statement's stores run. `attrib.lua` remains
`pending`: verified against the pinned oracle with `_port` predefined `true`
(mirroring `all.lua`'s own sandboxing convention, legitimate here because Sol
has no dynamic C-module loader by design) through the file's "test conflicts
in multiple assignment" section, it next hits a distinct, pre-existing,
general parser bug - `parse_postfix` applies call/index/field suffixes to any
primary expression instead of restricting them to real Lua's `prefixexp`
grammar, so an immediately-invoked function expression on its own line gets
absorbed as a call on the previous statement's trailing table constructor -
left for follow-up.

`big.lua`'s stress case drove four more fixes. Global reads/writes compiled
against a custom `_ENV` table (via `load`'s fourth argument) now route
through the same `__index`/`__newindex` metamethod resolution ordinary table
indexing uses whenever `_ENV` carries a metatable, instead of reading/writing
the table directly; a minimal `debug.traceback` native was added, backed by
new runtime bookkeeping (`pending_frame_label`/`entry_label` on frames pushed
to run an `__index`/`__newindex` body, and `pending_error_stack` stashed
before an `xpcall` handler runs, since the erroring Lua frames are already
unwound by then) so a metamethod body that errors gets an "in metamethod
'...'" annotation on its traceback, matching `ldebug.c`'s naming for
table-access-triggered calls; a table constructor with more elements than fit
in its register-count-sized counter now frees field registers back to the
table's own register as each field is stored, instead of exhausting the
`u16` register space; and `#t` is now a real Lua-style O(1)-common-case
border search instead of an O(n) scan, fixing an O(n^2) blowup in any
`t[#t + 1] = v` append loop. With those fixed, standalone `big.lua` now
reaches its own top-level `coroutine.yield` and fails there exactly as the
pinned real Lua 5.5.1 binary does when invoked the same way — `all.lua`
itself only ever runs this file wrapped in `coroutine.wrap`, so the manifest
records this row as `diverges` (an intentional invocation-model gap) rather
than `pending`.

Lua 5.4+ `<close>`/to-be-closed-variable support is now implemented end to
end: parser support for the `<close>` local attribute (typed `.sol` still
rejects it, falling back to the dynamic runtime, since it is a dynamic-only
feature); a `MarkClose`/`CloseSlots` bytecode pair the compiler emits at
every scope-exit path (normal fallthrough, `break`, `return`) that closes
locals in LIFO declaration order; `__close` metamethod dispatch that passes
through the in-flight error on an error-driven unwind, consistently across
`pcall`/`xpcall`, `coroutine.resume`, and the blocking native-call bridge;
and a generic `for`'s implicit closing of a 4th iterator-list value scoped to
the whole loop. (A `goto` jumping out of a `<close>` variable's scope is a
deliberate, documented, out-of-scope limitation.) This resolved
`nextvar.lua`'s `assert(closed)` blocker and, in turn, reached its previously
unreached "testing ipairs with metamethods" section, which surfaced one more
real bug now also fixed: `ipairs`'s iterator read table slots with raw
access instead of respecting `__index` (Lua's `ipairs` has used ordinary,
metamethod-respecting indexing since 5.3). With both fixed, `nextvar.lua` now
runs to completion under elevated instruction/call-depth/allocation budgets;
its manifest row stays `pending` only because `sol run`'s default
embedder-sized instruction budget is exhausted mid-file in an unrelated
hash-collision stress section, a pre-existing budget-sizing gap rather than
a compatibility bug.

The CLI also had a general output-loss bug affecting every erroring corpus
case with prior output, not just a display nuance: `print`/`io.write` only
ever appended to an in-memory buffer that was flushed to real stdout on the
success path (`write_lua_run`), so any script that printed output before an
uncaught error lost that output entirely instead of matching real Lua's
write-immediately semantics. `LuaError` now carries whatever output had been
buffered at the point it was raised, and the CLI flushes it before reporting
the error. This was found while isolating `nextvar.lua`'s remaining blocker
(prior output was needed to tell where execution had reached) and is
independent of the CLI's one intentional divergence (auto-printing the
top-level chunk's return value). With it fixed, `nextvar.lua` advanced past
its former instruction-budget exhaustion to fail
`checkerror("bad argument", pairs)`: Sol's native argument-count validation
(the shared `required` closure in `lua_runtime/natives.rs` and repeated
`LuaError::new("missing argument")` sites in `lua_runtime/dispatch.rs`) used a
generic "missing argument" message instead of real Lua's
`"bad argument #N to 'name' (...)"` wording, so `checkerror`'s substring match
failed. That wording gap is now fixed across `call_native`'s argument checks
(a systemic fix, not specific to `pairs`/`ipairs`), which unblocked several
other pending rows' `checkerror`-style assertions.

`nextvar.lua` then failed its "testing next x GC of deleted keys" section:
`next(t, k)` on a table where `k` had just been set to nil mid-traversal
(real Lua explicitly permits this) raised "invalid key to 'next'" instead of
resuming, because `LuaTable::set` removed a hash-part key outright on
`t[k] = nil` instead of tombstoning it. Fixed by tombstoning (keeping the key
with a `Nil` value so `next` can still locate its position to resume from,
while `entries()`/iteration/length continue to skip tombstones as absent).
That surfaced a second, harder-to-reproduce bug behind the same section,
nondeterministic across process runs: `LuaTable::hash`'s `std::HashMap` can
silently reorder its whole iteration order on a same-key overwrite — exactly
what the tombstone write does — because `HashMap::insert`'s internal
capacity-growth check runs before it knows whether the key already exists, so
a table that happens to be at its growth threshold rehashes (and reorders)
even though no key was added or removed. Since `next` resumes by re-locating
the last-returned key in a freshly fetched snapshot and continuing right
after it, such a reorder could strand not-yet-visited entries before that
position and silently truncate the traversal. `LuaTable::hash` is now an
`indexmap::IndexMap`, which never repositions an existing key on overwrite,
making the resume-by-position algorithm robust regardless of internal
rehashing.

With both fixed, `nextvar.lua` advances past the entire "testing next x GC of
deleted keys" section and now fails at line 581, inside a `table.insert`/
`table.remove` boundary-condition helper exercised against tables built with
negative/zero integer keys mixed with the `#` length operator — not yet
root-caused.

That line-581 failure was `table.insert`/`table.remove` splicing
`LuaTable`'s internal array-part `Vec` directly instead of going through
generic `t[i]` get/set the way real Lua's `lua_geti`/`lua_seti`-based
implementation does, so a live integer key held in the table's hash part —
`0`, a negative number, or anything past the array part's contiguous prefix —
was invisible to both natives. Fixed by rewriting both in terms of the
table's generic `get`/`set`/`len()`, matching real Lua's exact bounds-check
semantics, including the subtle rule that `table.remove`'s default position
(`#list`) is used unchecked even at size `0` (letting `table.remove(a)`
observe `a[0]` on a table whose only entry is `a[0]`).

With that fixed, `nextvar.lua` advanced to line 613, in the "testing table
library with metamethods" section: `table.insert`/`concat`/`unpack`/`remove`/
`sort` must all respect a table argument's `__index`/`__newindex`/`__len`
metamethods — i.e. work correctly against a "proxy" table whose actual
storage lives behind those metamethods on a *different* table — instead of
only ever touching the proxy's own (possibly empty) raw storage directly.
`table.sort` was the hardest case: its coroutine-yield-safe stepped
implementation (`SortState`/`LuaRuntime::sort_step`, needed so a comparator
can itself `yield` across a coroutine boundary) read the table's raw
`array` field directly to gather values and wrote back into it directly on
completion, both bypassing metamethods entirely — so `table.sort(proxy)`
silently did nothing whenever the proxy's own array was empty. Fixed by
adding blocking, metamethod-aware `LuaRuntime::index_get`/`index_set`/
`length_of` helpers (thin wrappers around the existing non-blocking
`index_resolve`/`set_index_resolve`/`len_resolve` primitives already used by
the bytecode dispatch loop, invoking the resolved metamethod call
synchronously via the existing `self.call` recursion) and rewiring
`table.insert`/`remove`/`concat`/`unpack`, plus `SortState`'s table field
(now the original `LuaValue` argument rather than a raw
`Rc<RefCell<LuaTable>>`) and both ends of `sort_step`, to go through them.

Fixing `table.insert`'s bounds arithmetic for the metamethod case also
surfaced the corpus's very next block, "testing overflow in table.insert
(must wrap-around)": when a `__len` metamethod reports `math.maxinteger`,
`table.insert(t, v)`'s implicit end position (`size + 1`) must wrap around to
`math.mininteger`, matching real Lua's C integer arithmetic, rather than
panicking on Rust's default debug-build overflow check. Fixed by computing
that position (and its companion bounds check) with explicit
`wrapping_add`/unsigned-wraparound comparison instead of a plain `+`, and by
only running `table.insert`'s element-shifting loop for the explicit-position
three-argument call (matching real Lua's `tinsert`, which has no shifting
loop at all in the two-argument case) so the wrapped end position can never
itself drive a bogus shift.

With all three fixed, `nextvar.lua` advances past the entire "testing table
library with metamethods" and "testing overflow in table.insert" sections and
now fails at line 742, in the "testing floats in numeric for" section (mixed
integer/float control-value semantics for the numeric `for` loop) — not yet
root-caused.

That line-742 failure turned out to be two separate bugs in the same region.
First, `load "for v, k in pairs{} do v = 10 end"` was expected to fail to
compile ("assign to const variable 'v'") but silently succeeded: a numeric
`for`'s control variable is registered as an implicitly const local, but the
bytecode compiler's `GenericFor` case registered its loop variables
(`v`/`k`) as ordinary, non-const locals. Fixed by registering them as const,
matching the numeric-for case exactly.

Second (surfaced once the first fix let the corpus reach its next line),
`for i = 1, 10.9 do checkint(i) end` was expected to run as a ten-iteration,
all-integer loop (`math.type(i) == "integer"` throughout) but instead ran the
whole loop in floats: `ForPrep` only took its integer-loop path when *every*
control value (start/stop/step) was already an integer, so a float limit
alongside an integer start/step forced the whole loop, and its control
variable, to floats. Real Lua's `forlimit` instead "fixes" a float limit into
an integer one whenever start/step are integers — rounding it toward the loop
(floor when ascending, ceil when descending) and clamping to
`i64::MAX`/`i64::MIN` when the float is out of `i64` range (needed for cases
like `for i = m, m - 10, -1 do` where `m = math.maxinteger`) — and only then
falls back to an all-float loop if start or step themselves aren't integers.
Fixed by adding a `float_for_limit` helper implementing that exact rounding/
clamping (including the "no integer can possibly satisfy the loop" case,
e.g. a NaN limit, which skips the loop entirely) and using it in `ForPrep`
whenever start/step are integers but the limit isn't.

With both fixed, `nextvar.lua` advances past the entire "testing floats in
numeric for" section and now fails at line 919, on `assert(closed)` inside a
block that gives a table a `__pairs` metamethod returning a fourth,
to-be-closed value from its iterator triple. This needs `<close>`/to-be-
closed-variable support — including a generic `for`'s implicit closing of
that fourth value — which is a substantial, already-tracked, not-yet-
implemented feature (`docs/features/lua-compatibility.md`'s Phase 4;
`crates/sol/src/parser.rs`'s `parse_attribute` explicitly rejects `<close>`
with "requires resource finalization, which is not implemented yet"), not a
small bug fix like the ones above.

`goto.lua`'s label/goto validation is now implemented in the dynamic bytecode
compiler: `FuncState` tracks a live active-local count per scope (mirroring
real Lua's `fs->nactvar`), clamps a bubbled-out pending goto's count to its
closing scope's starting count (matching `movegotosout`), and
`check_goto_scope` compares goto/label counts to reject both a duplicate label
in the same or a nested open scope and a `goto` that jumps into a local's
scope, using the same `"<goto NAME> at line L> jumps into the scope of 'V'"`
wording real Lua uses. `global *`/`global none` declarations (Lua 5.5's
block-scoped global-declaration statement) also register as goto-scope
pseudo-locals — occupying a slot in the active-local count without allocating
a register or resolving as a local on later bareword use — so a goto skipping
one is caught the same way, matching `goto.lua`'s
`errmsg([[ goto l2; global *; ::l1:: ::l2:: print(3) ]], "scope of '*'")`
case. This is a deliberately narrow slice of Lua 5.5's `global` declaration
feature (goto-scope participation only, not the full
declare-before-use/`<const>`/`_ENV`/redefinition semantics), scoped to unblock
this concrete corpus failure rather than building the whole feature
speculatively.

With that fixed, `goto.lua` advanced from line 30 to line 166, where its only
`debug.*` dependency, `debug.upvalueid(closure, index)`, was unimplemented.
Added a `LuaValue::LightUserdata(usize)` variant carrying a closure upvalue
cell's `Rc::as_ptr` identity, a `NativeFunction::DebugUpvalueid` dispatch that
reads it out of `LuaClosure.upvals`, and a `debug` library table installed
like `os`/`io`/`coroutine` (always present as a global/preload entry, with
`upvalueid` itself gated on the existing, previously-unused
`capabilities.debug` flag). This satisfies `goto.lua`'s
upvalue-sharing-topology assertions (lines 168-225, the only `debug.*` calls
the file makes).

`goto.lua`'s fuller `global`-declaration strict-checking assertions (lines
296-474: `global none` rejecting an undeclared bareword target, globals
rejecting `<close>`, declare-before-use, `_ENV`/redefinition checks) are now
implemented too, on top of the goto-scope pseudo-local tracking above.
`global function NAME(...) end` used to never shadow anything — it compiled
identically to a plain top-level `function NAME` (ordinary assignment sugar)
because both share the `Stmt::GlobalFunction` AST variant. Real Lua's
`globalfunc` declares (and activates) `NAME` as a global *before* compiling
its body, so a new `Function.is_global_decl` field (set only by the parser's
genuine `global function` production, never by the other two origins of the
same AST variant) now drives that same declare-before-body-compile ordering,
so the body's own recursive self-reference and any later use of the name
resolve as the declared global rather than an outer local of the same name.
Separately, `global NAME = value` and `global function NAME` now run the
runtime `"global '%s' already defined"` guard real Lua's
`checkglobal`/`OP_ERRNNIL` requires whenever the target's current value is
non-nil at the moment of declaration — a new `Instr::ErrorIfGlobalDefined`,
checked via the same `_ENV`-aware read `emit_environment_get` already used
for ordinary global access, so it also fires correctly through a rebound
local `_ENV` table (`goto.lua` lines 463-474). See
`crates/sol/tests/lua55_dynamic_runtime.rs`'s
`dynamic_lua_runtime_named_global_declaration_is_block_scoped_and_shadows_an_outer_local`/
`dynamic_lua_runtime_global_function_declaration_shadows_an_outer_local_of_the_same_name`/
`dynamic_lua_runtime_global_declaration_with_an_initializer_errors_if_already_defined`/
`dynamic_lua_runtime_global_already_defined_check_also_applies_to_a_rebound_env_table`.

With those fixed, running the unmodified upstream file now reaches line 329
before stopping on a pre-existing, intentionally out-of-scope divergence:
Sol always reserves `global` as a hard keyword, while real Lua's
non-instrumented build (without the `T`/ltests flag) treats it as a
contextual/soft keyword usable as an ordinary identifier, so
`load("global = 1; return global")` fails to compile under Sol instead of
succeeding. Working around only that one line (in
`crates/sol/scratch/goto_bisect.lua`, never the real corpus file) reaches a
second, independent, likewise out-of-scope divergence at line 361: the file
expects real Lua's `chunkname:N:` error-message-prefix convention, but Sol's
diagnostics use a `line N:` prefix instead (the error content itself is
correct). With both worked around, `goto.lua` runs cleanly end-to-end and
prints `OK`; `tests/lua55/manifest.toml`'s entry stays `pending` because the
unmodified upstream file does not yet exit 0 under `sol run`.

`constructs.lua`'s manifest note claiming a line-6 `require "debug"` blocker
was stale (the same `debug` preload work above already covers it). Chasing
this file further past that point turned up two genuine, now-fixed bugs: a
table read of a nil or otherwise unhashable key (`t[nil]`, `t[0/0]`) wrongly
raised `"table index is nil"`/`"table index is NaN"` - real Lua only raises
that for a *write* (`luaH_newkey`), never a read, so `LuaTable::get` in
`lua_runtime/value.rs` no longer reuses the write path's key error; and a
parenthesized multi-value expression (`(f())`) failed to truncate to exactly
one value the way real Lua requires, instead still expanding all of `f`'s
results - fixed with a new `ast::ExprKind::Paren` node that the parser wraps
around a parenthesized `Call`/`CallExpr`/`MethodCall`/`Vararg`, which the
dynamic bytecode compiler (`lua_bytecode/compile_expr.rs`) compiles as a
single-value expression. See
`dynamic_lua_runtime_table_read_of_a_nil_or_unhashable_key_returns_nil_but_a_write_still_errors`/
`dynamic_lua_runtime_parenthesized_call_truncates_to_one_value` in
`crates/sol/tests/lua55_dynamic_runtime.rs`. With both fixed, the unmodified
upstream file now reaches line 244's `checkload` assertion before stopping on
the same pre-existing, out-of-scope `line N:` vs. `chunkname:N:` diagnostic-
prefix divergence already noted above for `goto.lua`; `constructs.lua` stays
`pending` for the same reason.

`errors.lua`'s manifest note claiming a line-6 `require "debug"` blocker was
likewise stale. Chasing this file further turned up two more genuine,
now-fixed bugs: `error()`'s message argument is optional in real Lua
(`luaB_error`'s `lua_settop(L, 1)` defaults it to `nil`), but Sol's
`NativeFunction::Error` required it; and real Lua's `luaG_errormsg` converts a
thrown `nil` error object to the literal string `"<no error object>"` at the
moment it is raised, so `pcall`/`xpcall` never observe a raw `nil` from
`error()`/`error(nil)` - both fixed in `lua_runtime/natives.rs`. Separately,
Lua's `retstat` grammar only allows `return` (optionally followed by one `;`)
as a block's *last* statement; Sol's parser silently accepted trailing tokens
or a second `;` after `return` instead of rejecting them - fixed in
`parser.rs`'s `parse_block` and the top-level chunk loop in `parse_program`,
which now consume one optional `;` after a `return`/`MultiReturn` statement
and then require a block terminator (or end of input at top level). See
`dynamic_lua_runtime_error_with_no_message_or_a_nil_message_becomes_no_error_object`/
`dynamic_lua_runtime_return_must_be_the_last_statement_in_a_block` in
`crates/sol/tests/lua55_dynamic_runtime.rs`. With both fixed, the unmodified
upstream file now reaches line 65's `checksyntax` call before stopping on the
same pre-existing, out-of-scope `line N:` vs. `chunkname:N:` diagnostic-prefix
divergence noted above; `errors.lua` stays `pending` for the same reason.

`literals.lua`'s manifest note claiming a line-8 `require "debug"` blocker was
likewise stale, but this file also exercises `require"debug".getinfo`/
`debug.getinfo` inline (lines 38/249), which Sol had never implemented at
all - added a minimal `debug.getinfo(level)` that only supports the numeric
stack-level form and only returns a `currentline` field (the one thing this
file's `lexstring` helper reads), reading the level-th `Frame::Lua` from the
top of `LuaRuntime::frames` and mapping its `header.pc` through
`proto.source_map.location` the same way error tracebacks already do.
Chasing this file also turned up two genuine, now-fixed byte-oriented lexer
bugs sharing one root cause: Rust's `u8::is_ascii_whitespace` deliberately
excludes vertical tab (0x0B), but Lua's own lexer treats it as a space
character alongside `' '`, `'\t'`, and `'\f'` (llex.c's `case ' ': case '\f':
case '\t': case '\v':`) - both between ordinary tokens and while a `\z`
string escape is skipping whitespace, so `crates/sol/src/lexer.rs`'s main
scan loop and its `\z`-escape handler each needed an explicit `|| b == 0x0b`
alongside `is_ascii_whitespace()`. See
`dynamic_lua_runtime_vertical_tab_and_form_feed_count_as_whitespace_including_inside_a_z_escape`/
`dynamic_lua_runtime_debug_getinfo_reports_the_calling_frames_current_line` in
`crates/sol/tests/lua55_dynamic_runtime.rs`. With all three fixed, the
unmodified upstream file now reaches line 85's `lexerror` helper, which
expects a `near '<token>'` phrase in lex/parse error messages that Sol's
diagnostics don't produce at all - the same out-of-scope diagnostic-format
divergence noted above for `goto.lua`/`constructs.lua`/`errors.lua`, just its
"near" half rather than its chunkname-prefix half; `literals.lua` stays
`pending` for the same reason.

`sort.lua`'s previous blocker (`table.create`'s hash-size hint not honored)
is now fixed: `table.create(sizeseq, sizerest)` preallocates both the array
part (`sizeseq` nils) and the hash part (`IndexMap::with_capacity(sizerest)`,
which `collectgarbage("count")`'s existing live-heap accounting already
prices in automatically once the hash part actually has that capacity, with
no separate manual bookkeeping needed), with the same out-of-range/table-
overflow argument validation as `ltable.c`'s `luaH_resize`. Chasing the rest
of the file surfaced and fixed several more real gaps: `table.insert` didn't
reject a wrong argument count; `debug.getmetatable`/`debug.setmetatable` for
numbers didn't exist at all (added a `number_metatable` runtime slot, shared
across `Integer`/`Float` like real Lua's single `LUA_TNUMBER` metatable slot,
settable only through `debug.setmetatable` since ordinary `setmetatable`
rejects non-table values); `index_resolve` had a real, previously-hidden bug
where any non-table/non-string value skipped the metamethod lookup entirely
and raised "attempt to index" immediately, silently defeating scalar
metatables regardless of what installed them; `table.unpack` rejected any
non-table argument even though real Lua's `lua_geti`/`luaL_len`-based
implementation happily unpacks a proxied scalar; and `length_of` (the
library-facing "read an actual integer length" helper used by
`table.insert`/`unpack`/etc., as distinct from the raw `#` operator) raised a
generic coercion error instead of real Lua's specific "object length is not
an integer" wording for a non-integer `__len` result. `table.move` did not
exist at all - implemented `table.move(a1, f, e, t [, a2])` end to end,
matching `ltablib.c`'s `tmove`/`checktab`: a plain table or a value with the
needed `__index`/`__newindex` metamethod is accepted as source/destination;
the forward-vs-backward copy direction is chosen exactly as real Lua does
(`t > e || t <= f || different tables` copies forward, otherwise backward) so
that both the final result *and* the exact sequence of reads/writes a side-
effecting metamethod can observe match real Lua even when an error aborts
the move partway through; and the "too many elements to move"/"destination
wrap around" argument checks use `i128`-widened arithmetic mirroring real
Lua's `LUA_MAXINTEGER`-relative `luaL_argcheck`s, since the corpus exercises
`math.maxinteger`/`mininteger` boundary ranges directly. This, in turn,
surfaced two more real bugs in `table.sort`, in both its blocking
(`call_native`) and coroutine-yield-safe stepped implementations alike: a
table whose `__len` reports an enormous size (e.g. `math.maxinteger`) made
the native attempt a huge `Vec::with_capacity` and abort the process instead
of raising real Lua's "array too big" (`ltablib.c`: `luaL_argcheck(n <
INT_MAX, ...)`, checked before any allocation); and an invalid (non-strict-
weak-order) comparator was never detected at all, where real Lua's quicksort
raises "invalid order function for sorting" via its own partition's boundary
checks - insertion sort has no equivalent structural signal, so this adds an
explicit post-hoc pass confirming every adjacent pair is actually in order
before writing the result back, reusing the same comparator/`__lt` calls
(including through the stepped sort's coroutine-yield-safe state machine, by
adding a `validating` phase to `SortState` after the main sort completes).
With all of that fixed, `sort.lua` now runs to its "testing sort" section's
`perm{...}` calls, which rely on a bare global `unpack` - not a Sol
compatibility gap: real Lua 5.5 never registers a global `unpack` either
(only `table.unpack`/`string.unpack`, confirmed against `ltablib.c`/
`lstrlib.c`), and this corpus file only has one because the shared
`all.lua` harness binds a local `unpack = table.unpack` alias before
`dofile`-ing each individual test module. Run standalone (as this manifest
does, and as genuine Lua 5.5 would be run the same way), `sort.lua` hits
this identically, so it stays `pending` on that harness-only dependency
rather than a fixable compatibility bug.

`math.lua` now passes end to end (oracle-backed against the pinned Lua 5.5.1
reference), after fixing a chain of differential bugs found while chasing it
one failure at a time: hex-float mantissa overflow for very long numerals
(`tonumber('0xe03' .. string.rep('0', 1000) .. 'p-4000')`) - naively
accumulating every hex digit into an `f64` overflows to infinity long before a
numeral's `p`-exponent is ever applied, so the lexer now reimplements real
Lua's `lua_strx2number` algorithm: cap mantissa accumulation at 30 significant
hex digits and fold any excess (plus every fractional digit, regardless of the
cap) into a corrective integer exponent instead; catastrophic-cancellation
precision loss in the float `%` operator for large magnitudes (`2.0^54 % 3`
gave `0.0` instead of `1.0`) - `a - floor(a / b) * b` loses all precision once
`a` is large enough that `a / b` and its floor are themselves already-rounded
floats, so it now computes the hardware `fmod(a, b)` plus real Lua's
`luai_nummod` sign correction instead, which stays exact at any magnitude;
coercion-failure error messages read `"expected number"`/`"expected string"` -
the words transposed relative to real Lua's own `"number expected"`/`"string
expected"` wording that `checkerror`'s pattern-matching depends on;
`math.tointeger` only accepted a live number value, never a numeral string
(`minint .. ""`, `"34.0"`), even though real Lua's `math_toint` (via
`lua_tointegerx`) applies the same string-to-number coercion arithmetic does;
`i64::MAX as f64` rounds up to `2^63` (`i64::MAX` itself isn't exactly
representable as an `f64`), so an inclusive upper bound of `<= i64::MAX as
f64` wrongly accepted `2^63` itself - one past the largest representable
integer - in both `math.tointeger` and the table-key float-to-integer
normalization that backs indexing a table with a huge float key, both now
fixed to use the same strict bound real Lua's `luaV_flttointeger` uses;
`math.max`/`math.min` compared arguments by converting both to `f64` first,
which loses precision for large integers (`minint` and `minint + 1` both
round to the same float), so `math.max(minint, minint + 1)` wrongly returned
`minint` - fixed to compare via the same exact int/float comparison the
`<`/`>` operators already used; and float-to-string conversion
(`LuaValue::display_bytes`, backing `tostring`/`print`/concatenation/
`string.format`'s `%s`) was simply Rust's `f64::to_string`, which never uses
scientific notation (so huge or tiny magnitudes produced enormous plain-
decimal digit runs) and never appends the `.0` real Lua uses to keep a whole-
number float visually distinct from an integer - replaced with a
`format_lua_float` helper that reuses Rust's `{:e}` formatter (already a
shortest-round-trip digit generator) as a digit source, then places the
decimal point using real Lua's plain-vs-scientific `%g`-style rule
(scientific when the decimal exponent is `< -4` or `>= max(digit count, 15)`,
verified experimentally against the pinned `lua5.5` reference binary since
this isn't documented behavior). That reference binary's own shortest-digit
generator occasionally emits one digit more than strictly necessary in hard
cases (e.g. `1/3` prints with 17 digits though 16 already round-trip, a known
characteristic of Grisu-family generators without a slow-path fallback); only
the round-trip and distinctness properties are load-bearing for Lua
compatibility (the corpus checks `tonumber(tostring(x)) == x`, never exact
digit strings), so this divergence is harmless and left as is rather than
chasing an exact reimplementation of that quirk. Finally, `math.random`'s
statistical distribution checks are legitimately instruction-heavy (bounded,
not runaway, but a few million VM instructions), which needed the `sol` CLI's
default Lua instruction budget raised from 1,000,000 to 20,000,000 - still
within "trusted local script" territory per that default's own documented
rationale (`crates/sol/src/main.rs`'s `lua_dynamic_budgets`), and distinct
from `heavy.lua`'s still-`pending` genuinely-unbounded stress loop.

`require` now genuinely loads real `.lua` files off the host filesystem,
gated on `Capabilities::filesystem` like every other host-filesystem read in
this runtime: real Lua 5.5's `findloader` search order (`package.preload`,
then `package.path`, then `package.cpath`) is implemented end to end,
including dotted sub-module names translating to nested paths via
`package.searchpath`'s `sep`/`rep` substitution, a matched `path` file being
compiled and cached exactly like `load`/`dofile`, and `package.searchers`
being type-checked (but not yet actually dispatched through) so a
non-table replacement fails the same way real Lua's does. A `package.cpath`
match is a distinct, permanent gap - Sol has no dynamic C-module (native
`.so`) loader on any platform, matching `AGENTS.md`'s "keep native loading
capabilities explicit" guardrail - and raises a clear, distinct error rather
than silently succeeding or misreporting a real match as absent.
`io.output`/`io.write`/`io.close` (gated on `filesystem`/`stdout`
respectively) and `os.remove` (gated on `filesystem`) round out real
file-handle writes and deletion; `package.loadlib` always answers `nil,
<message>, "absent"`, matching what real Lua itself reports when built
without dynamic-load support - the one honest, platform-independent answer,
since Sol never has that support. Two further interpreter bugs surfaced and
are now fixed while chasing this work through `attrib.lua`: `require`'s
already-loaded check compared `package.loaded[name]` against `Nil` instead of
using Lua truthiness, so a module whose loader legitimately caches `false`
was wrongly treated as permanently loaded instead of reloaded on the next
`require`, unlike real Lua's `ll_require` (`lua_toboolean` in `loadlib.c`);
and a bare (non-`local`) `_ENV = {}` reassignment inside a `load`-compiled
chunk used to compile as an ordinary `SetGlobal("_ENV", ...)`, which only
stashed a spurious `_ENV` key into the *shared* globals table without
changing what subsequent plain names in that chunk resolved against, so
later global writes leaked into the caller's real globals instead of the
chunk's own replacement table - real Lua treats `_ENV` as an always-present
implicit per-chunk upvalue, and a bare reassignment replaces only that
chunk's own upvalue cell. Fixed with a new `Instr::SetEnvironment` bytecode
op that replaces the current frame's `Globals` in place, emitted by
`emit_environment_set` specifically for a bare, non-declaring `_ENV`
assignment with no existing lexical local/upvalue in scope; `local _ENV =
...` and Sol's own `global _ENV = ...` declaration keyword are unaffected.
With all of the above, `attrib.lua` now runs correctly through every
`package`/`require`/`io`/`os` section and the file's own multiple-assignment
tests, verified against the pinned `lua5.5.1` binary run from inside
`lua-5.5.1-tests/` with the corpus's prebuilt `libs/*.so` test modules
present; it stops precisely at line 337's unconditional `require"lib2-v2"` (a
genuine native-module load, run regardless of whether an earlier
`package.loadlib` probe succeeded), the permanent native-module boundary
described above - the manifest now records this row as `host-required`
(`native-modules`) rather than `pending`, since the remaining gap is a
principled capability exclusion, not a missing language/runtime feature.
This same `require` work, as an unplanned side effect, also resolved
`bitwise.lua`'s real blocker (`require "bwcoercion"`, already a passing
row) with no changes to `bitwise.lua` itself; both `bitwise.lua`'s
manifest row and `docs/features/lua-superset-plan.md`'s stale "a stated
non-goal" wording are updated to match.

`calls.lua`'s "C-stack overflow while handling C-stack overflow" section
(the dynamic call-depth budget being exhausted, real Lua's stand-in for a
genuine C-stack overflow) previously escaped every enclosing `pcall`/`xpcall`
unconditionally: four separate call-depth-exhaustion checks in
`lua_runtime/dispatch.rs`'s trampoline (`drive_result`'s `PushClosure` arm,
`resolve_call`'s `PushClosure` arm, and `push_native_call`) returned a raw
`Err` straight out of the driving loop instead of going through
`unwind_error_to_marker`, the same marker search every other runtime error
already used to find an enclosing `pcall`/`xpcall` frame. All four now route
through it, so a call-depth-exhaustion error is caught like any other Lua
runtime error. This surfaced a second, correctly-modeled gap: real Lua's
`xpcall` message handler is not retried if invoking it *itself* overflows -
that "double fault" reports the fixed message `"error in error handling"`
rather than escaping uncaught or looping. `unwind_error_to_marker`'s marker
search now also matches its own `Xpcall(XCallStage::Handler)` marker (a case
its search previously excluded, with an `unreachable!()` guard on the
now-reachable path) and returns that synthetic message directly, discarding
the handler's own second failure the same way real Lua's `nCcalls`
double-overflow detection does, verified byte-for-byte against the pinned
`lua5.5.1` binary. With this fix, `calls.lua` now runs correctly through this
entire section; it next stops at line 178's "tail calls x chain of
`__call`", a different, unrelated gap - calling through a chain of `__call`
metamethods in tail position is not currently preserved as a real tail call
(`TailClosure`), so it charges ordinary call-depth budget the way a
non-tail call does, where real Lua keeps this O(1) C-stack usage via its own
tail-call/`__call` interaction. The manifest row stays `pending` with this
updated, precise blocker.

`calls.lua`'s "tail calls x chain of `__call`" section is now fixed at its
root cause in `lua_runtime/dispatch.rs`'s `step_result_for_call`, the single
resolver shared by every `Instr::Call`/`Instr::TailCall`/metamethod-triggered
call site. It previously only fast-pathed a `__call` chain exactly one hop
deep (the immediate `__call` metamethod value had to already be a `Closure`);
anything deeper fell through to `StepResult::CallLeaf`, which dispatches
through the blocking, recursive `LuaRuntime::call` bridge - correct in value
but charging ordinary `call_depth` per hop and, critically, never becoming a
`StepResult::TailClosure`, so a tail call resolved through such a chain could
never actually be a tail call. `step_result_for_call` now resolves the whole
`__call` chain iteratively in a loop (no native-stack recursion, no
`call_depth` charge per hop), exactly mirroring real Lua's `luaD_precall`
"retry" loop in `ldo.c`: each hop that isn't itself callable prepends its own
(still-unresolved) value onto the front of the pending argument list and
moves on to *its* `__call` metamethod, until landing on a real closure/native
function or a value with no `__call` at all. Because this loop is the one
shared resolver, the existing `Instr::TailCall` dispatch (which already
turned a resolved `PushClosure` into `TailClosure`) now does so correctly
regardless of `__call` chain depth, with no other change needed - a tail call
through an arbitrarily deep, repeatedly-invoked `__call` chain is O(1)
call-depth, matching real Lua. The loop is bounded to a new `MAX_CALL_CHAIN =
15`, verified empirically against the pinned `lua5.5` oracle (a 15-hop chain
resolves normally down to an ordinary "attempt to call a ... value", while a
16-hop chain raises exactly `"'__call' chain too long"`) and matching real
Lua 5.5's dedicated `MAX_CCMT` bit-packed counter on `CallInfo` in
`tryfuncTM` - a materially smaller, distinct bound from the existing
`MAX_METATABLE_CHAIN = 2000` used for `__index`/`__newindex` fallback chains.
With this fix, `calls.lua` now runs correctly through both the tail-call and
the value/argument-ordering halves of "testing chains of `__call`" (line
~195, a 15-deep chain ending in `table.pack`, verified byte-for-byte against
the oracle including argument order and the `Res.n`/`Res[i]` table-identity
checks); it next stops at line 212's `debug.getinfo(1, 't').extraargs`
assertion - `debug.getinfo`'s returned table is still intentionally minimal
(`currentline` only, per its own doc comment in `natives.rs`) and has no
`extraargs` field (the Lua 5.5 extension reporting how many varargs a call
supplied beyond a function's named parameters), a separate, unrelated
feature gap. The manifest row stays `pending` with this updated, precise
blocker.

That `extraargs` gap is now closed, but its real semantics turned out to be
different from what its name (and the prior note here) suggested. Reading the
actual Lua 5.5.1 reference source (`ldebug.c`'s `auxgetinfo`, `ltm.c`'s
`tryfuncTM`, `ldo.c`'s `luaD_precall`) and confirming empirically against the
pinned `lua5.5.1` oracle showed that `lua_Debug.extraargs` (populated by
`debug.getinfo`'s `"t"` option) is *not* a count of a vararg function's
received arguments beyond its named parameters - that internal quantity
(`ci->u.l.nextraargs`) exists only for `OP_VARARG`'s own use and is never
exposed through the debug API. `extraargs` is instead `(ci->callstatus &
MAX_CCMT) >> CIST_CCMT`: the number of `__call` metamethod hops
`luaD_precall`'s retry loop walked to reach the currently-inspected closure -
literally the same counter the U6 `__call`-chain fix above already tracks and
bounds as `MAX_CALL_CHAIN` (not a coincidence: Lua's `MAX_CCMT` is a 4-bit
field, capping at exactly 15, matching `MAX_CALL_CHAIN`'s own value). A direct
call - no `__call` involved - reports 0 regardless of how many actual varargs
the function received; a call resolved through *n* `__call` hops reports
exactly *n*. Sol now tracks this as `LuaFrame::call_chain_hops`, set from
`step_result_for_call`'s existing chain-resolution loop's `hops` count and
threaded through `StepResult::PushClosure`/`TailClosure` and `new_lua_frame`
into every pushed frame (the blocking, non-trampolined `LuaRuntime::call`
bridge's own inline `__call` resolution - used for metamethod calls issued
directly from native Rust code, not the ordinary `Instr::Call`/`Instr::TailCall`
path - does not track hops and always reports 0; a documented, narrow gap
distinct from the trampolined path this feature was verified against).
`debug.getinfo(level, ...)`'s returned table now always includes `extraargs`
alongside `currentline`, sourced from the resolved frame the same way
`currentline` already is. This makes the entirety of `calls.lua`'s "testing
chains of `__call`" section (line ~195, including the line-212 assertion)
pass. The corpus file now progresses further and stops at a new, unrelated
blocker: line 342's `load(read1(x), "modname", "t", _G)` passes a *function*
reader as the chunk source (real Lua's generic `lua_load` protocol, calling
the function repeatedly to accumulate source until it signals end-of-input),
which Sol's `NativeFunction::Load` arm does not support - it unconditionally
requires argument #1 to already be a `LuaValue::String`, so it raises the
generic "string expected" error instead of invoking the reader. The manifest
row stays `pending` with this new, precise blocker.

`load`'s generic reader support followed: `NativeFunction::Load` now accepts
a function (or a number, auto-coerced to its string form, matching real
Lua's `lua_tolstring` on argument #1) in addition to a plain string. When
argument #1 is a function, it is called repeatedly with no arguments - via
`self.call`, the same blocking bridge `table.sort`'s comparator arm already
uses from inside a native, since natives in this trampoline are permitted to
call back into Lua synchronously that way - and each call's first return
value is taken as the next source piece, exactly matching real Lua's
`lbaselib.c` `generic_reader`/`lzio.c` `luaZ_fill`: a nil or empty-string
return ends the stream (both treated identically as end-of-input), any other
non-string/non-number return is a caught error ("reader function must
return a string"), and all pieces read before the stream ends are
concatenated into one source buffer that then runs through the exact same
`compile_chunk` path the string form already used. An error raised by the
reader itself is likewise caught here rather than propagated, matching real
Lua's `lua_load` running the whole reader loop under a protected parser -
only a first argument that is neither string/number/function fails an
immediate, uncaught argument-type check, matching `luaL_checktype`'s
placement *before* that protected region. This makes `calls.lua`'s line-342
`load(read1(x), "modname", "t", _G)` and line-343 assertion pass, along with
the file's later `load(function () return nil end)`/`load(function () return
true end)` cases (lines 349-352) and the "small bug" case (lines 356-358)
where a reader's first returned piece happens to be nil. See
`dynamic_lua_runtime_load_accepts_a_reader_function_that_returns_pieces`/
`dynamic_lua_runtime_load_reader_returning_nil_immediately_yields_an_empty_chunk`
in `crates/sol/tests/lua55_dynamic_runtime.rs`. The corpus file now
progresses further and stops at a new blocker: line 344's
`debug.getinfo(a).source` (where `a` is the closure `load` just produced)
passes a *function value*, not a stack level, as `debug.getinfo`'s first
argument - Sol's `NativeFunction::DebugGetinfo` arm only implements the
numeric stack-level form, and even that form's returned table never
populates a `source` field (only `currentline`/`extraargs`). The manifest
row stays `pending` with this new, precise blocker.

`closure.lua` is now promoted to `pass`. Its previous blocker note (weak-table
GC exhaustion) turned out to be stale from an earlier investigation; the real
remaining blocker was a register-aliasing bug at line ~140: a loop-body local
later captured as an upvalue by a nested closure could be assigned the same
register number as an earlier-compiled, textually-preceding scratch temp
within the same loop body (e.g. a `while` guard condition's boolean result).
Ordinary stack-discipline register recycling (`reset_to`/`pop_scope`/
`end_statement`) only protected *already-captured* registers going forward
(`retired_floor`), not registers a still-open loop had used earlier in its own
body - and because a loop's body is compiled once but executed repeatedly, the
guard condition's scratch-temp code re-runs on every iteration, including ones
that skip the captured local's own re-initialization via an early
`return`/`break`, silently overwriting the live closure's captured cell with
an unrelated boolean/temporary value. Fixed by adding `LoopCtx.reg_floor` in
`crates/sol/src/lua_bytecode/func_state.rs`: the highest register `alloc_reg`
has handed out anywhere in the current loop's body so far, which now also
floors every recycling site's `next_reg` for as long as that loop is being
compiled (propagated to an enclosing loop, if any, when a nested loop is
popped). This is the same "costs a few extra registers, never correctness"
tradeoff `retired_floor` already uses, generalized from "never reuse a
captured register in the future" to "never reuse a register this loop has
ever used, for the rest of this loop's compilation."

Fixing this also required making top-level `function NAME(...) end` chunk
hoisting (`parser.rs`'s `parse_program`) precise rather than all-or-nothing:
a plain top-level function is hoisted into the independently-compiled
`functions` list (which has no enclosing scope and can never capture a
chunk-scope local as an upvalue) only when its own name does not rebind a
preceding chunk-local and a conservative free-variable scan of its body
(`function_references_any_name`, walking every `Stmt`/`ExprKind` variant, not
shadowing-aware by design - a false positive only costs hoist eligibility,
never correctness) finds no reference to a chunk-local declared earlier in
the same chunk; otherwise it compiles as an ordinary in-order
`Stmt::GlobalFunction` chunk statement, matching real Lua's assignment-sugar
semantics for `function NAME(...) end`. This keeps `sol build`'s AOT/typed
pipeline eligible for self-contained top-level functions that happen to
follow an unrelated chunk-local (e.g. `native/strings.lua`'s `concat`/
`compare`), which a coarser "any chunk-local exists" condition had
incorrectly disqualified. The decision stays dialect-uniform (no
`sol_extensions` gate), preserving `tests/frontend_conformance.rs`'s
invariant that identical `.lua` source parses to an identical AST under both
`LanguageConfig::LUA` and `LanguageConfig::SOL`.

Separately, `debug.upvalueid`/`debug.upvaluejoin` compatibility gaps
surfaced while working through `closure.lua`'s upvalue-identity assertions
were also closed: `debug.upvalueid(f, n)` now returns `nil` for an
out-of-range upvalue index instead of erroring (matching the real oracle),
and also accepts a `string.gmatch` iterator value (`LuaValue::GMatchIterator`,
not a `Closure`) by giving it exactly one opaque identity (its own `Rc`
pointer) at index 1 and `nil` elsewhere. `debug.upvaluejoin(f1, n1, f2, n2)`,
previously entirely unimplemented, now makes `f1`'s upvalue `n1` share
storage with `f2`'s upvalue `n2` by replacing `f1`'s upvalue cell with a
clone of `f2`'s `Rc<RefCell<LuaValue>>`.

`calls.lua`'s "test for generic load" section made substantial further
progress (still `pending`, now blocked on a real `_ENV`-upvalue
architecture - see `tests/lua55/manifest.toml`'s entry for the remaining
gap). `debug.getinfo` now accepts a function value (not just a numeric
stack level) as its first argument and reports a `source` field for any
`Proto` compiled through `load`, taken from the chunkname `load`'s caller
supplied: `LuaRuntime` gained a `chunk_sources: HashMap<usize, Rc<Vec<u8>>>`
side table keyed by `Proto` pointer identity (the same pattern as the
existing `prototype_ids` table), populated by a new
`compile_chunk_named`/`register_chunk_source` pair that recurses into every
nested `Proto` a chunk compiles to, matching real Lua's per-chunk (not
per-function) `source`. `load`'s mode argument (`"b"`/`"t"`/default `"bt"`)
is now enforced (`lauxlib.c`'s `checkmode`): a chunk is treated as binary
only if it starts with the `0x1B` signature byte real Lua's binary chunks
also start with, and a mode mismatch returns `nil` plus the exact
`"attempt to load a text/binary chunk (mode is '...')"` message the corpus
checks for via `string.find`. `string.dump` is now implemented, though not
as real bytecode serialization - Sol has no `Proto` (de)serializer - but as
an opaque same-process handle (a `0x1B` byte followed by a key) into a new
`dumped_protos: HashMap<usize, Rc<Proto>>` registry that keeps the dumped
`Proto` alive; `load(handle, ..., "b")` decodes the handle and rebuilds an
equivalent closure directly, without going through the lexer/parser at all.
This is sufficient for same-process round-tripping (the only case the Lua
5.5 corpus exercises) but not for a chunk written to a file and loaded by a
different process. Finally, `parser.rs`'s two generic "this token can't
start a statement/expression" fallback error messages now include the
literal phrase "unexpected symbol", matching real Lua's `lparser.c` wording
- the corpus matches parse-error messages by substring via `string.find`,
so that wording is part of the compatibility contract, not cosmetic.

### U7 — Interpreter performance foundation

**Purpose:** make the semantic engine efficient before adding native tiers.

Deliverables, each benchmarked independently:

- [ ] packed/tagged `Value` representation prototype and measured selection;
- [ ] dense frame/register layout with no routine `Rc` clone or heap allocation per
  register operation/call;
- [ ] allocation-free common call, return, vararg, and iterator paths;
- [ ] optimized table array/hash layout, string interning/hashing, and shape IDs;
- [ ] direct-threaded/computed dispatch only if a maintainable Rust implementation
  measures better than dense `match` dispatch;
- [ ] generational allocation fast paths and measured barriers;
- [ ] fast metamethod-negative paths and tail-call frame reuse.

Exit gate: interpreter-readiness performance gate passes with compatibility, GC
stress, debugger, and WASM tests enabled.

### U8 — Inline caches and bounded profiling

**Purpose:** collect and exploit dynamic facts without native-code dependency.

Deliverables:

- [ ] table field/index, global, arithmetic/metamethod, iterator, and call-target
  inline caches;
- [ ] shape, metatable, global, and module version counters with invalidation;
- [ ] bounded mono/poly/megamorphic transitions;
- [ ] type/shape/call/allocation profiles serialized for benchmark inspection and
  optional profile-guided runs;
- [ ] cache correctness tests that mutate aliases, metatables, `_ENV`, modules, and
  debug-visible state.

Exit gate: cache-heavy benchmarks improve without semantic mismatch; forced
invalidation and megamorphic workloads remain bounded and correct.

### U9 — Baseline dynamic JIT

**Purpose:** remove dispatch overhead quickly for hot untyped functions.

Deliverables:

- [ ] fast bytecode-to-Cranelift lowering for generic and cached operations;
- [ ] semantic/runtime stubs for slow paths;
- [ ] safepoints, stack maps, exception/error transitions, and coroutine fallback;
- [ ] hot-function counters and background or bounded compilation policy;
- [ ] direct entry adapters for stable call targets;
- [ ] code cache lifecycle, invalidation, and executable-memory safety.

Exit gate: baseline-JIT readiness gate passes; compile latency and code memory
stay within published budgets; interpreter/JIT differential and GC tests pass.

### U10 — Optimizing SSA JIT, OSR, and deoptimization

**Purpose:** reach LuaJIT-class performance on untyped Lua using inference and
profiles.

Deliverables:

- [ ] lift bytecode to shared SSA with proof provenance;
- [ ] insert/hoist/fuse guards and record full deoptimization snapshots;
- [ ] specialize arithmetic, tables, calls, loops, allocation, and iteration;
- [ ] inline across stable dynamic and typed calls;
- [ ] enter optimized loops through OSR and leave through precise side exits;
- [ ] reconstruct inlined frames for errors, coroutines, profiler, and debugger;
- [ ] prevent recompilation storms with failure counters and widening.

Exit gate: dynamic parity gate passes first; final-performance work continues
until the final performance claim gate passes or the release explicitly states
that it has not yet achieved the goal.

### U11 — AOT and annotation-driven peak performance

**Purpose:** make optional types a predictable accelerator on the same engine.

Deliverables:

- [ ] feed checked annotations and static inference directly into shared SSA;
- [ ] remove guards and generic runtime calls proven unnecessary;
- [ ] specialize generics, records, arrays, maps, and callbacks across modules;
- [ ] retain dynamic adapters for exported/reflective entry points;
- [ ] support profile-guided AOT with safe fallback when profiles change;
- [ ] compare each gradually typed program against its unchanged Lua version.

Exit gate: typed advantage gate passes, annotation coverage yields monotonic or
explained results, and mixed dynamic behavior remains compatible.

### U12 — Canonical WASM playground and debugger

**Purpose:** make the web product a Sol playground rather than a separate VM.

Deliverables:

- [ ] create `sol-wasm` and `packages/sol-runtime` from the Tier-0 runtime;
- [ ] reproduce execution budgets, virtual modules, output capture, breakpoints,
  stepping, stack frames, locals/upvalues, evaluation, profiling, and timeline;
- [ ] accept `.lua` and `.sol` projects and use the shared parser/type diagnostics;
- [ ] keep execution in a worker and capabilities default-deny;
- [ ] switch the web adapter behind a feature flag, run old/new browser
  differentials, then remove the Piccolo production dependency;
- [ ] address bundle size, initialization latency, and long-running responsiveness.

Exit gate: existing web end-to-end debugger scenarios pass on the canonical
runtime; native/WASM portable-profile fixtures agree; the production worker no
longer imports `@lua-playground/runtime`.

### U13 — Semantic LSP and first-party VS Code client

**Purpose:** ship supported editor tooling, not only a generic-server binary.

Deliverables:

- [ ] commit and test `sol-lsp` as part of the supported repository;
- [ ] replace textual indexing with the U1 binder and U4 type facts;
- [ ] implement incremental documents, multi-file module graphs, complete diagnostic
  lists, semantic rename/references, completion, signature help, hover, symbols,
  semantic tokens, formatting policy, and cancellation;
- [ ] share the same analysis with Monaco directly or through a worker-safe LSP
  transport;
- [ ] create `editors/vscode-sol` with language configuration, syntax/semantic
  highlighting, server launch/download/path configuration, logs, restart,
  workspace discovery, and `.lua`/`.sol` activation;
- [ ] add protocol, golden fixture, headless VS Code integration, packaging, and
  clean-install tests.

Exit gate: a packaged extension installed into a clean VS Code profile starts
the bundled or configured server and passes diagnostics/navigation/completion/
rename tests across mixed `.lua`/`.sol` modules; LSP behavior agrees with CLI
analysis.

### U14 — Integrated release qualification

**Purpose:** prove the final product claim end to end.

Deliverables:

- [ ] run compatibility, fuzz, tier differential, GC stress, sanitizer, native,
  WASM, LSP, VS Code, and web E2E matrices from a clean checkout;
- [ ] publish raw benchmark data and summaries for both architectures;
- [ ] audit capabilities, native loading, executable memory, FFI, and browser
  isolation;
- [ ] remove obsolete fallback/partition paths and production Piccolo dependencies
  only after rollback tags and differential evidence exist;
- [ ] align all feature/spec/product documents and publish known limitations;
- [ ] package CLI/runtime libraries, web assets, LSP, and VS Code extension from one
  versioned release process.

Exit gate: every item in the definition of done below is satisfied. Otherwise
the release is labeled according to the highest completed gate rather than using
the final claim.

## 9. Migration strategy from the current repository

Use a strangler migration rather than rewriting all working code at once:

1. Freeze current typed, dynamic, and Piccolo behavior as differential tests.
2. Extract frontend/runtime interfaces while implementations remain in place.
3. Introduce canonical objects and adapters next to `LuaValue` and typed heap
   objects.
4. Move one object family at a time: strings, tables, closures/upvalues,
   threads, userdata, then typed aggregates.
5. Move calls to the semantic ABI before merging bytecode instruction sets.
6. Compile selected functions through the unified engine behind a diagnostic
   flag and compare results with both legacy paths.
7. Switch native `sol run`, then WASM/web, then delete unreachable legacy code.

During migration:

- no adapter may copy a table or closure in a way that changes identity;
- no object may be considered collectible by one heap while referenced by
  another;
- no benchmark win can waive a compatibility failure;
- no source-mode branch may be added without documenting whether it controls
  syntax, diagnostics, capabilities, or execution;
- old and new performance numbers must name the exact runtime path;
- each deletion requires repository-wide `rg` evidence and full relevant tests,
  not merely a successful build.

## 10. Dependency graph and critical path

```mermaid
flowchart LR
  U0 --> U1 --> U2 --> U3 --> U4 --> U5
  U3 --> U6
  U6 --> U7 --> U8 --> U9 --> U10 --> U11 --> U14
  U5 --> U10
  U3 --> U12 --> U14
  U4 --> U13 --> U14
```

Compatibility and runtime identity (`U1`–`U6`) are the critical correctness
path. Interpreter/JIT work must use that path rather than optimizing a runtime
scheduled for removal. Web work begins once the portable runtime and semantic
ABI are stable. LSP work can proceed after the binder/type APIs stabilize and
does not need to wait for JIT work.

## 11. Definition of done

The final goal is complete only when all of the following are true.

### Language and runtime

- [ ] Valid target-version Lua executes unchanged as Sol.
- [ ] `.lua` and annotation-free `.sol` have identical observable semantics.
- [ ] Types are optional, sound contracts and do not create a second runtime.
- [ ] Typed/untyped modules, calls, errors, coroutines, tables, and GC objects
  interoperate with shared identity.
- [ ] The declared native compatibility profile, including the decided embedding
  boundary, passes its upstream and differential suites.

### Performance

- [ ] The final performance claim gate in section 7.3 passes on published,
  reproducible untyped application workloads.
- [ ] Typed/annotated workloads retain a separately measured advantage.
- [ ] Startup, compilation, memory, and GC behavior are published and not hidden by
  throughput-only reporting.
- [ ] No compatibility mode or fallback runtime is excluded from the headline.

### Products

- [ ] Native CLI, AOT/JIT, and embedding interfaces use the canonical runtime.
- [ ] The web playground runs the canonical interpreter in WebAssembly and supports
  `.lua` and `.sol` editing/debugging.
- [ ] `sol-lsp` provides semantic multi-file analysis from the shared frontend.
- [ ] A packaged first-party VS Code extension launches and supports `sol-lsp`.
- [ ] Monaco, VS Code, and the CLI agree on parsing, binding, and diagnostics.

### Engineering quality

- [ ] Compatibility, fuzz, tier differential, GC stress, sanitizer, native/WASM, web
  E2E, LSP, and extension tests run in CI at appropriate frequencies.
- [ ] Benchmarks retain raw results and environment metadata.
- [ ] Unsafe runtime/JIT boundaries have explicit invariants and targeted tests.
- Product, feature, specification, and status documents agree with executable
  behavior.

Until every category is complete, progress should be reported by the completed
compatibility and performance gates, not by the final “fully compatible and
faster than LuaJIT” wording.
