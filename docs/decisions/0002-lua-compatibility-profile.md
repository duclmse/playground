# ADR 0002: Lua 5.5.1 compatibility and embedding profile

- Status: accepted
- Date: 2026-09-15

## Context

“Lua-compatible” can refer only to source syntax, or additionally to libraries,
CLI behavior, binary chunks, native modules, and the C embedding interface.
Sandbox restrictions also need to remain distinguishable from missing
semantics.

## Decision

PUC Lua 5.5.1 is the initial semantic oracle. Checked-in metadata pins its
source/test revision. Compatibility is reported by explicit profiles:

1. core language;
2. portable standard library;
3. native host/CLI;
4. embedding/native modules;
5. capability-restricted browser/embedded sandbox.

The final native “fully compatible runtime” profile includes source-compatible
Lua 5.5 C API headers and ABI-compatible linking for supported platform/ABI
triples, so supported embedders and binary modules can link against Sol without
source changes. Lua's internal `ltests` hooks are test infrastructure rather
than public API, but equivalent tests must cover the behavior they expose.

Lua 5.5 binary chunks are version-specific and enter the native profile.
LuaJIT FFI, `jit.*`, LuaJIT bytecode, and Lua 5.1 compatibility extensions are
a separate optional profile and are not prerequisites for Lua 5.5 conformance.

Browser and default embedded builds use explicit deny-by-default capabilities.
Denied filesystem/process/native-loading operations do not make the core
language a different dialect.

## Consequences

- The project withholds “fully runtime-compatible” until the native and
  embedding profiles pass.
- `host-required` is a temporary inventory status, not a permanent way to hide
  native-profile gaps.
- Compatibility tests use PUC Lua; LuaJIT is the performance comparator.
- Version/platform-specific ABI work must be tested and documented separately.
