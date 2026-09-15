# Product brief

## Vision

Sol is a Lua 5.5-compatible language and runtime implemented in Rust. Every
supported Lua program remains valid; developers can add optional types and Sol
extensions to improve diagnostics, tooling, and predictable native performance
without moving onto a different runtime.

The same frontend and semantic runtime power:

- a native CLI with interpreter, JIT, OSR, and AOT tiers;
- an entirely client-side web playground with a real debugger and object
  inspector;
- an LSP shared by Monaco and a first-party VS Code extension;
- a version-targeted embedding API and explicit native/browser capability
  profiles.

The final performance objective is to outperform a pinned LuaJIT baseline on a
published suite of unchanged, untyped Lua applications, while typed programs
retain a separately measured advantage. Until that gate passes, performance is
reported by achieved tier rather than claimed broadly.

## Product principles

1. **Compatibility before shortcuts.** PUC Lua 5.5 defines target behavior.
   Optimizations may guard and deoptimize but may not silently change Lua
   semantics.
2. **One runtime, multiple representations.** All code shares object identity,
   tables, strings, closures, coroutines, errors, libraries, modules, and GC.
   Proven values may stay unboxed in optimized frames.
3. **Types are optional contracts.** Untyped Lua remains valid. Static
   inference removes proven checks; profiles remove hot checks through guards;
   annotations make contracts explicit.
4. **Claims are measured.** Upstream classification is not a compatibility
   pass, and a typed rewrite is not evidence about unchanged Lua performance.
5. **Tooling shares language truth.** CLI, LSP, VS Code, and Monaco reuse the
   same parser, binder, and type facts.
6. **Capabilities are explicit.** Native execution can expose a Lua-like host
   profile; browser and embedded use default-deny capability providers without
   creating a language fork.

## Target users

- Lua developers who want drop-in execution with an incremental path to types
  and native specialization.
- Developers embedding Lua/Sol in games, services, tools, and configuration
  systems.
- Performance-sensitive teams that need profiling, predictable optimized hot
  paths, and AOT deployment.
- Learners and educators who benefit from a browser debugger that exposes
  locals, upvalues, tables, call stacks, coroutines, and execution flow.
- Editor users who expect semantic navigation, completion, diagnostics, and
  refactoring in VS Code and Monaco.

## Required outcomes

### Language and runtime

- `.lua` accepts the pinned Lua 5.5 language unchanged.
- `.sol` accepts the same language plus contextual type/extension syntax.
- Renaming annotation-free `.lua` to `.sol` does not change behavior.
- Typed and untyped functions/modules interoperate through one semantic ABI.
- Native compatibility, browser sandbox, and embedding profiles state their
  host capabilities precisely.
- “Fully runtime-compatible” is withheld until the declared standard-library,
  CLI, binary-chunk, native-module, and C API/ABI profile passes its gates.

### Performance

- The portable interpreter first beats reference Lua and the repository's
  current Piccolo baseline without compatibility regressions.
- Baseline and optimizing native tiers use inline caches, inference, runtime
  profiles, guards, OSR, and deoptimization for untyped Lua.
- The final LuaJIT comparison uses identical Lua source and inputs, includes
  real applications, and reports startup, throughput, memory, and GC behavior.
- Typed/annotated programs are measured as a progressive optimization of the
  same program, in addition to specialized data-oriented workloads.

### Products

- The web playground supports `.lua` and `.sol`, runs the canonical portable
  runtime in WebAssembly, and retains its worker isolation and debugger UI.
- `sol-lsp` provides semantic, multi-file analysis from the shared frontend.
- A packaged `vscode-sol` extension starts or locates `sol-lsp` and supports a
  clean install.

## Current stage

The repository has completed convergence milestone U0 and is ready for U1. Its
strongest pieces already exist, but remain split:

- `crates/sol` has a mature typed Cranelift pipeline and a substantial but
  incomplete Lua 5.5 interpreter.
- `crates/lua-vm`/`crates/vm` currently power the browser through Piccolo and
  remain a migration oracle until the canonical WASM runtime reaches parity.
- `crates/sol-lsp` is an early single-file LSP prototype; a first-party VS Code
  client does not yet exist.
- The upstream Lua 5.5 manifest is fully classified but currently has zero
  complete oracle-backed file passes; typed capability ports are separate
  regression evidence.
- Typed Sol beats LuaJIT on selected workloads, while the untyped dynamic path
  remains far behind it. The final performance goal therefore remains open.

Current implementation behavior is specified in [spec/](spec/README.md). The
accepted architecture, milestones, performance gates, and definition of done
are in the [unified runtime plan](features/unified-sol-runtime-plan.md).

## Non-goals

- A line-for-line port of LuaJIT. Sol uses a Rust implementation and adopts
  measured JIT techniques while PUC Lua remains the semantic oracle.
- Forcing every typed value into the dynamic tagged representation.
- Running a native executable-memory JIT inside the browser; the portable
  interpreter compiled to WebAssembly is the required browser tier.
- Multi-user collaboration, accounts, or a hosted execution backend as part of
  runtime convergence.
- Treating LuaJIT-specific FFI, `jit.*`, or bytecode behavior as implicit Lua
  5.5 compatibility. Those require a separately declared profile.

## Definition of done

The product is done only when the compatibility, final LuaJIT performance,
native/WASM, web debugger, semantic LSP, VS Code packaging, security, and
cross-tier correctness gates in
[the unified runtime plan](features/unified-sol-runtime-plan.md#11-definition-of-done)
all pass. Partial releases state the highest completed gate and their known
limitations instead of using the final claim.
