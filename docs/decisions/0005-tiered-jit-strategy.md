# ADR 0005: tiered execution and LuaJIT-inspired optimization

- Status: accepted
- Date: 2026-09-15

## Context

The current typed Cranelift tier performs well on selected programs, while the
dynamic interpreter is far behind LuaJIT. Rewriting LuaJIT line for line would
not reuse Sol's type system or existing optimizer, and Rust alone does not make
a VM fast.

## Decision

Sol implements a Lua-compatible tiered VM in Rust, informed by measured LuaJIT
techniques rather than copied implementation structure:

1. a stackless portable bytecode interpreter is the semantic reference;
2. inline caches and bounded profiles record tags, shapes, versions, call
   targets, loops, allocation, and guard failures;
3. a fast baseline Cranelift tier removes dispatch overhead with runtime stubs;
4. an optimizing SSA tier combines static proof, annotations, and guarded
   observations, with OSR and full deoptimization snapshots;
5. AOT uses the same SSA/runtime ABI and keeps generic fallbacks where needed.

Method/function compilation comes first because it reuses the current backend.
Region or trace formation is added when profiles show function compilation
cannot meet the dynamic performance gate. A custom assembler/backend is
considered only after Cranelift is measured as the limiting factor.

The browser requires the same Tier-0 semantics compiled to WebAssembly, not a
native executable-memory JIT.

## Consequences

- Interpreter representation, calls, tables, strings, and GC are optimized
  before building native tiers on top of them.
- Every speculative native operation has guards, invalidation dependencies,
  and deoptimization state.
- Debugger, profiler, errors, coroutines, and GC remain correct across tiers.
- Typed annotations accelerate the same pipeline rather than choosing another
  compiler/runtime.
