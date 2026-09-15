# ADR 0004: portable runtime boundary and semantic call ABI

- Status: accepted
- Date: 2026-09-15

## Context

Native Cranelift dependencies and OS capabilities should not be required by the
browser or LSP. At the same time, separate runtime object/call models prevent
mixed typed/dynamic execution and make GC ownership unsafe.

## Decision

The target crate boundary is:

- `sol-core`: byte frontend, binder, types/inference, unified bytecode,
  portable interpreter, runtime objects, libraries, capability interfaces,
  debugger events, and GC contracts;
- `sol`: native CLI, Cranelift tiers, AOT, native host providers, profiler, and
  embedding library;
- `sol-wasm`: `wasm-bindgen` adapter around `sol-core`;
- `sol-lsp`: analysis client of `sol-core` only.

Extraction is incremental; interfaces are introduced within the current crate
before moving modules.

All callables implement a semantic ABI supporting arbitrary arguments,
varargs, multiple results, tail calls, catchable errors, yields, and debugger/
deoptimization suspension. Proven typed calls may use a direct fast ABI, but
must provide adapters when dynamically visible. Dynamic-to-typed checks occur
at entry, and typed-to-dynamic calls box through the canonical `Value` without
copying identity-bearing objects.

All production runtime objects belong to one tracing heap. Native frames expose
precise roots through stack maps; interpreter/coroutine frames expose explicit
roots. No adapter may make two collectors independently own one object.

## Consequences

- The current typed and Lua bytecodes converge on one function/frame model.
- JIT metadata must reconstruct semantic frames at guards, calls, yields, and
  debugger locations.
- `sol-core` cannot depend on Cranelift or ambient filesystem/process access.
- Piccolo remains outside the canonical identity domain and is used only across
  a differential process/API boundary during migration.
