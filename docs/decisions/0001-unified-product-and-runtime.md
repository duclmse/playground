# ADR 0001: one Sol product and semantic runtime

- Status: accepted
- Date: 2026-09-15

## Context

The repository grew a typed native compiler, a separate dynamic Lua runtime,
and a Piccolo-backed browser runtime. This produced useful implementations but
allowed compatibility and performance claims to apply to different engines.

## Decision

Sol is one Lua 5.5-compatible product. Untyped Lua, partially typed Sol, and
fully typed Sol share observable Lua semantics, object identity, heap/GC,
tables, strings, closures, coroutines, modules, libraries, errors, and a
semantic call ABI.

Typed and dynamic values may retain different internal representations.
Proven typed values stay unboxed in optimized frames; adapters box or check
values only at observable dynamic boundaries. “One runtime” does not mean “box
everything.”

`crates/sol` is the migration source for the canonical runtime. The current
Piccolo browser engine and legacy execution paths remain differential oracles
until native and WASM parity gates pass, then leave production use.

## Consequences

- File extension cannot permanently choose a different semantic runtime.
- Compatibility and performance are measured on the canonical engine.
- Web, CLI, LSP, and VS Code must share frontend/runtime truth.
- Runtime convergence must preserve unboxed typed hot paths.
- Documents describing the old split are historical/current-state records, not
  future architecture.
