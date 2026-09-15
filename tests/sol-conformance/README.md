# Typed Sol capability regression suite

The directory name is retained for compatibility with existing scripts and
test paths. This suite is not Lua conformance evidence and must never be added
to the upstream compatibility pass count.

[manifest.toml](manifest.toml) maps every top-level file in the pinned Lua
5.5.1 test checkout (`lua-5.5.1-tests/*.lua`, see
[../lua55/README.md](../lua55/README.md)) to a hand-written typed `.sol`
counterpart under
[`crates/sol/tests/fixtures/sol-conformance/`](../../crates/sol/tests/fixtures/sol-conformance/),
using the same file stem (`attrib.lua` -> `attrib.sol`).

This is a different kind of suite from `tests/lua55/`. That corpus asks "does
Sol's `.lua`-compatibility mode run real upstream Lua source correctly?" This
suite instead asks: for each thing the upstream Lua test suite is checking,
**is there a typed-Sol reinterpretation of that intent, and if so, what does
it look like?** Typed `.sol` is a different, statically-typed language with
no coroutines, no C API, no metatables, no `io`/`os`, no pattern matching, and
no dynamic table/global model - so a large fraction of the upstream corpus
has no meaningful analogue at all. Each case is one of:

- **`status = "ported"`** (22 of 34) - the `.sol` file is a genuine
  reinterpretation of the upstream file's tested intent, expressed in Sol's
  actual typed feature set (see `docs/spec/`). For example, `bitwise.lua`'s
  cross-file `require`-based bit-operator tests become a direct exercise of
  Sol's own native `&`/`|`/`~`/`<<`/`>>` operators on `i64`.
- **`status = "not-applicable"`** (12 of 34) - the upstream file exercises a
  subsystem typed Sol does not have at all (the C API, a standalone
  executable's CLI, coroutines, `io`/`os`, a debug library, pattern matching,
  native modules, binary chunk dump/undump). Its `.sol` file is an
  intentional stub - a short comment explaining why, plus `return 0` - rather
  than a forced, meaningless port.

Each manifest entry's `expected` field is that fixture's exact `sol run`
stdout. **These values are self-authored from Sol's own typed semantics, not
copied from a Lua oracle** - unlike `tests/lua55/`, there is no meaningful
"does Sol's output match real Lua's output" question to ask here, since the
two programs aren't doing the same computation in the first place. What this
suite actually guards against is silent regressions in Sol's typed capabilities
across the specific corner cases each upstream file's intent maps to (integer
wraparound, generational-GC write barriers, `Map`/`Array` iteration order,
`any`/`is`/`as` narrowing, and so on).

## Running it

```sh
cargo test --manifest-path crates/sol/Cargo.toml --test sol_conformance
scripts/test-sol-conformance-suite.sh lua-5.5.1-tests
```

The Rust test runs every fixture and checks its manifest-recorded expected
stdout, plus a coverage cross-check that the manifest still has exactly one
case per upstream `.lua` file. The shell runner does the same thing plus a
`PORT`/`N/A`/`FAIL` breakdown, and works even without the upstream checkout
present (it then skips the 1:1 corpus-coverage check and only validates
manifest/fixture consistency).

See [docs/features/sol-conformance.md](../../docs/features/sol-conformance.md)
for the full status writeup, including the one JIT bug this suite surfaced
while it was being built.
