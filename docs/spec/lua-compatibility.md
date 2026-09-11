# Lua compatibility specification

Sol's `.lua` mode targets Lua 5.5 semantics through a dedicated dynamic parser,
value representation, and interpreter. It is a compatibility subset, not an
alternate spelling of typed `.sol`. The detailed corpus inventory and open
engineering work are in [the Lua compatibility feature document](../features/lua-compatibility.md),
and test policy is in [`docs/conformance.md`](../conformance.md).

## Implemented surface

The current runtime implements a focused vertical slice including byte-oriented
source, Lua values, numeric and string operations, dynamic tables, closures and
shared captured environments, varargs and multiple results, iterator triples,
protected calls, same-block labels/gotos, table metatables, and selected
base/string/table/math/utf8 library functions. Recursion, instruction/backedge,
and heap-object allocation budgets are enforced.

The checked manifest at `tests/lua55/manifest.toml` is the authoritative case
inventory. Each upstream case is classified as `pass`, `adapted`,
`host-required`, `pending`, or `diverges`; an unclassified test is a harness
failure. `pending` behavior is not supported merely because its source parses.

## Mode-specific rules

- `fn` is an identifier, not a keyword.
- Lua 5.5 `global` declarations are recognized with their mode-specific scope
  and const rules.
- Values use Lua truthiness and dynamic dispatch, not typed Sol's `bool`-only
  condition rule.
- Tables have dynamic identity and are not implicitly converted to a typed Sol
  record, array, or map.
- Dynamic errors may be caught by supported protected-call operations.

## Capabilities and unsupported behavior

Host filesystem, process, OS, locale, debug, native-module, and C API access is
capability-gated. It must never become available merely because the compiler or
host process has that authority. The manifest records cases requiring these
facilities as `host-required` or documents an adapted semantic fixture.

Complete standard libraries, package loading, full goto scope validation,
precise dynamic GC/finalization, coroutines, typed/dynamic module bridging, and
dynamic native/JIT execution remain unsupported unless the manifest and
feature document explicitly mark their relevant slice implemented.

## Oracle

Compatibility claims must be compared against the pinned Lua 5.5.1 reference
profile described in `tests/lua55/README.md`. The system `lua` executable is not
a substitute. Normalization may remove platform paths, executable names, and
line-ending differences, but must not erase semantic output, error, or ordering
differences.
