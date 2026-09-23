# Lua 5.5 fixtures

These are behavioral fixtures for the Lua 5.5 surface that `sol` intends
to adopt while retaining typed compilation. Each root `.lua` file is an oracle
for the official Lua 5.5 interpreter and ends in a success marker. Files under
`native/` are the currently implemented, compiler-run subset: each returns
`true` and is executed by `tests/lua55.rs` in tier-0 bytecode, promoted JIT,
OSR, and (for strings) AOT modes.

Run the reference corpus with Lua 5.5 installed:

```sh
for fixture in crates/sol/tests/fixtures/lua55/*.lua; do lua "$fixture"; done
```

The corpus is organized by manual area. `invalid_syntax.lua` asserts that
invalid chunks fail to compile. `native/` covers the features currently
implemented in Sol: lexical syntax and numeric forms, operators, control
flow, inferred and gradual scalar types, nil and truthiness, `<const>`,
multiple assignments/declarations, byte strings, and the supported string
functions.

The remaining root fixtures are intentional compatibility targets. They cover
tables, functions/closures, coroutines, metatables, patterns and formatting,
UTF-8, package loading, GC, I/O, OS, and debugging. They are not marked as
supported until they move to `native/` with cross-tier Sol tests.

`lua55_global_syntax.lua` is parser coverage for Lua 5.5 `global`
declarations. The dynamic runtime now executes its table-backed root and
lexically rebound `_ENV` paths; canonical-heap ownership and full upstream
environment/loading coverage remain migration work.

Run Sol's supported profile with `scripts/test-sol-conformance.sh`. It checks
the native fixtures and `dynamic_core.lua` with exact `true` output; the
manifest runner remains the authoritative report for unsupported/pending cases.
