# Lua Conformance Test Plan

Piccolo is a from-scratch, pure-Rust Lua-*like* VM, not a build of the
official C reference implementation (see
[architecture.md](./architecture.md#runtime-choice-rust--piccolo)). That
means this project no longer inherits language conformance for free — it
has to validate it, on an ongoing basis, itself. This document is that plan.

It's referenced from [risks.md §3](./risks.md#3-lua-semantic-conformance-new-direct-consequence-of-the-piccolo-decision)
(the risk this mitigates), [roadmap.md](./roadmap.md#phase-0--conformance-harness)
(when it gets built), and [product-brief.md](./product-brief.md) (the goal
it's now qualifying: "Lua semantics" is a validated claim, not an assumed
one).

## Why this is a document, not a checkbox

Conformance isn't a milestone you hit once — piccolo is an actively
evolving project, this codebase will extend/patch it (per
[risks.md §1](./risks.md#1-piccolo-debug-introspection-surface-the-central-risk)),
and every language-feature bug reported by a user is either a debugger bug
or a VM semantics bug until proven otherwise. The corpus and the
known-deviations ledger need to stay live for the project's lifetime, not
just exist at launch.

## Scope

**In scope for v1 conformance validation:**

- Core language semantics: control flow (`if`/`while`/`for`/`repeat`,
  `goto`/labels, `break`), operators and their precedence, multiple
  assignment/return, varargs.
- Closures and upvalue capture (including the classic "loop variable
  capture" cases that catch interpreters off guard).
- Tables: array part vs hash part behavior, `#t` length semantics,
  `pairs`/`ipairs`/`next` iteration order guarantees (or lack thereof).
- Metatables/metamethods actually needed for the inspector and for typical
  user scripts: `__index`, `__newindex`, `__call`, `__tostring`, the
  arithmetic/comparison metamethods.
  **Update (Phase 0 finding):** piccolo 0.3.3's VM does not implement the
  arithmetic/comparison metamethods at all (confirmed by source inspection —
  see the known-deviations ledger below); `__index`/`__newindex`/`__call`/
  `__tostring` do work and are conformance-tested. Arithmetic/comparison
  metamethod support is now tracked as VM-fork work, not a conformance gap
  to close by testing harder.
- Error handling: `error`, `pcall`, `xpcall`, error object types (string vs
  table errors).
  **Update (Phase 0 finding):** `pcall`/`error`/`assert` ship with piccolo;
  `xpcall` did not and has been added as a host-registered stdlib extension
  (see the ledger). `error()`'s position-prefixing does not match real Lua
  and is a documented, accepted deviation for this MVP.
- Coroutines: `create`/`resume`/`yield`/`status`, since Phase 8's coroutine
  debugging design depends on this matching Lua's model.
- A practical subset of the standard library: `string` (including pattern
  matching — this is a known area where non-reference implementations
  diverge), `table`, `math`, `os.time`/`os.clock` (the sandbox-safe subset,
  see [architecture.md](./architecture.md#sandbox)).
  **Update (Phase 0 finding):** piccolo 0.3.3 ships no Lua pattern-matching
  engine (`find`/`match`/`gmatch`/`gsub`) and no `string.format`, and no
  string metatable (so `s:method()` colon-call syntax doesn't work on
  strings) — all confirmed by source inspection, all documented in the
  ledger, all deferred out of this MVP. `table.insert`/`concat`/`sort` were
  missing from piccolo too, but were small enough to add as host stdlib
  extensions rather than defer.

**Out of scope / explicitly not chased for v1:**

- Full standard-library parity (`io`, most of `os`, `package`/`require`
  internals beyond what the virtual FS needs) — the sandbox disables most
  of this anyway.
- Byte-exact string semantics for binary data (see the string-representation
  note in the interpreter-design discussion — this project accepts JS/Rust
  string types rather than true Lua byte-strings).
- Integer/float subtype edge cases beyond what's needed for correct
  arithmetic display in the inspector, unless a specific bug surfaces.
- `utf8` library, `debug` library (not exposed to user scripts at all — see
  the sandbox section), weak tables, `__gc` metamethod timing.

Anything hit by a real user bug report that falls in "out of scope" gets
triaged into the ledger below, not silently ignored.

## Fixture sourcing

Two complementary corpora, not one:

1. **Adapted subset of the official Lua test suite** (`lua-5.4-tests`,
   distributed by lua.org alongside the reference manual). The full suite
   assumes a complete `lua.c` host — file I/O, `os.exit`, full `io`/`os`,
   `utf8`, etc. — which this sandboxed playground doesn't and shouldn't
   provide. Hand-pick the tests that exercise pure language semantics and
   the in-scope stdlib subset above (e.g. `calls.lua`, `closure.lua`,
   `nextvar.lua`, relevant parts of `strings.lua`, `math.lua`), skip or stub
   the rest, and track which files are partially vs fully adopted in the
   ledger.
2. **A small hand-written corpus specific to this project**, covering
   exactly the scenarios the debugger's own docs use as examples —
   recursion (for stepping), closures over loop variables (for the
   inspector's reference-identity handling), coroutines (for the thread
   model), self-referential tables (`t.self = t`, for the `ObjectRegistry`).
   These double as fixtures for
   [risks.md §5](./risks.md#5-testing-strategy-for-stepping-logic)'s
   golden-file stepping tests — one corpus, two consumers, so they don't
   drift apart.

## Harness design

- Runs at the Rust level against `crates/lua-vm` directly (fast, no browser
  needed) for the bulk of the corpus.
- A smaller smoke subset also runs through the actual `wasm-bindgen`
  boundary (i.e. from TS, the way the real app calls it) to catch
  marshaling bugs that a pure-Rust test wouldn't see.
- Output format: each fixture declares expected stdout and/or an expected
  return/error value; the harness diffs actual vs expected and reports
  pass/fail per fixture, not just a suite-level pass/fail — a single
  regression should point at one `.lua` file, not require bisecting.

## Known-deviations ledger

A running, explicit list of "supported / partially supported / unsupported"
for anything a user could plausibly hit, e.g.:

| Feature | Status | Notes |
|---|---|---|
| Arithmetic/comparison metamethods (`__add`, `__sub`, `__mul`, `__div`, `__mod`, `__pow`, `__eq`, `__lt`, `__le`, ...) | **Unsupported** | piccolo 0.3.3's VM binary-op opcodes (`crates/thread/vm.rs`) raise a `BinaryOperatorError` directly and never consult `meta_ops` for these operators — confirmed by reading `meta_ops.rs`, which only implements `index`/`new_index`/`call`/`tostring` dispatch, nothing arithmetic. Fixing this needs VM changes, not host code — tracked alongside the [risks.md §1](./risks.md#1-piccolo-debug-introspection-surface-the-central-risk) fork decision. `metatables.lua` only exercises `__index`/`__newindex`/`__call`/`__tostring`, which do work. |
| `string` pattern matching (`find`, `match`, `gmatch`, `gsub`) and `string.format` | **Unsupported** | piccolo 0.3.3's `stdlib/string.rs` only implements `len`, `lower`, `reverse`, `sub`, `upper` — no pattern engine at all. Would require hand-writing a Lua pattern-matching engine; deferred out of this MVP, tracked as future host-stdlib work. `strings.lua` only exercises the supported subset. |
| `s:method()` colon-call syntax on string values | **Unsupported** | piccolo has no string metatable/`__index` fallback for `Value::String` (`meta_ops::index` has no `Value::String` arm) — `("hello"):upper()` raises a type error. Use `string.upper("hello")` form instead. |
| `table.insert` / `table.concat` / `table.sort` | **Supported (host extension)** | piccolo 0.3.3's `stdlib/table.rs` only ships `pack`/`unpack`. Added as host-registered callbacks in `crates/lua-vm/src/lib.rs` (`extend_table_library`), consistent with architecture.md's "host opts stdlib pieces in explicitly" design. `table.sort` with a custom comparator is not supported (would need a `Sequence`-driven callback into Lua for each comparison) and errors explicitly rather than silently ignoring the comparator. |
| `xpcall` | **Supported (host extension)** | piccolo 0.3.3's `stdlib/base.rs` only ships `pcall`. Added as a host-registered global in `crates/lua-vm/src/lib.rs` (`install_xpcall`), mirroring piccolo's own `pcall` `Sequence` implementation. |
| `error(msg)` position-prefixing (`"chunk:line: " .. msg`) | **Unsupported** | piccolo's `error` callback (`stdlib/base.rs`) passes the message value through unchanged; real Lua prepends the call-site position for string messages at the default level (1). No current-line introspection is available from a host callback without the frame/position API that [risks.md §1](./risks.md#1-piccolo-debug-introspection-surface-the-central-risk) found missing. `errors.lua`'s expected output reflects piccolo's actual (unprefixed) behavior. |
| `type()` of a VM-internal runtime error (e.g. `1 + nil`) caught by `pcall`/`xpcall` | **Deviates: `"userdata"` not `"string"`** | Confirmed in piccolo's `error.rs`: `Error::Runtime(...)` (anything raised by the VM itself rather than a user `error()` call) converts to a Lua value as a `UserData` wrapping the Rust error (with a `__tostring` metamethod for display), never a plain string. Only errors explicitly raised via `error("...")` are real Lua strings. `errors.lua`'s expected output reflects this. |
| Integral-float display (`2^10` → `"1024.0"` not `"1024"`) | **Fixed (host workaround)** | piccolo's own `Value::Display` (`value.rs`) does `write!(w, "{}", f)`, which drops the trailing `.0`; `meta_ops::tostring` bakes this in internally before a caller ever sees it (it eagerly stringifies non-string/non-metatable values). Worked around in `crates/lua-vm/src/lib.rs`'s `print` by special-casing `Value::Number` *before* calling `meta_ops::tostring` (numbers can never carry a metatable in piccolo, so this is safe) and formatting via a small `format_lua_number` helper. Only fixes numbers that flow through this project's `print`; any future host code calling piccolo's own `tostring`/`Display` directly on a float will still see the truncated form. |
| `table`/`function`/`thread`/`userdata` raw `tostring` format | **Deviates: `<table 0x...>` not `table: 0x...`** | piccolo's `Value::display` (`value.rs`) uses `<table {:p}>` instead of Lua's `table: 0x...`. Not currently exercised by any fixture (nothing in the corpus prints a raw table/function without `__tostring`); noted here so it doesn't surprise a future fixture. |

Rules for the ledger:

- A deviation found by a failing conformance test gets an entry before the
  test is skipped/xfailed — never silently marked pending without a row
  here.
- A deviation found in production (user-reported) gets a fixture added to
  the corpus *and* a ledger entry, so it can't regress silently again.
- This ledger is what backs the product-brief's conformance claim — if it's
  out of date, that claim is unsupported.

## CI gating

- The full Rust-level corpus runs on every change to `crates/lua-vm`.
- The smoke subset (through `wasm-bindgen`) runs on every change touching
  the `@lua-playground/runtime` boundary.
- A regression in either is a blocking failure, not a warning — this is the
  project's only substitute for "trust the reference implementation."
