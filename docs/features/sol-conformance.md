# Sol conformance suite

> Status: implemented. 22 of the 34 top-level `lua-5.5.1-tests` files have a
> genuine typed-Sol port; 12 are honest "not applicable" stubs. See
> [tests/sol-conformance/README.md](../../tests/sol-conformance/README.md)
> for how to run it.

**Purpose:** the [Lua compatibility corpus](lua-compatibility.md) answers
"does Sol's `.lua`-compatibility mode run real upstream Lua source
correctly?" This suite answers a different question: for each thing the
upstream Lua 5.5.1 test suite checks, is there a meaningful reinterpretation
of that same intent in Sol's *typed* `.sol` surface - a different, statically
typed language with no coroutines, no C API, no metatables, no `io`/`os`, no
pattern matching, and no dynamic table/global model?

## What exists

- [`tests/sol-conformance/manifest.toml`](../../tests/sol-conformance/manifest.toml) -
  one `[[case]]` per upstream `lua-5.5.1-tests/*.lua` file, mapping it to a
  `.sol` fixture and recording that fixture's exact expected `sol run`
  stdout.
- [`crates/sol/tests/fixtures/sol-conformance/`](../../crates/sol/tests/fixtures/sol-conformance/) -
  the 34 `.sol` files themselves, one per upstream file, same stem.
- [`crates/sol/tests/sol_conformance.rs`](../../crates/sol/tests/sol_conformance.rs) -
  runs every fixture through `cargo test` and checks its manifest-recorded
  stdout, plus a coverage cross-check against both the manifest's own
  `[[case]]` count and (when present) the real `lua-5.5.1-tests` checkout.
- [`scripts/test-sol-conformance-suite.sh`](../../scripts/test-sol-conformance-suite.sh) -
  the same check from the shell, with a `PORT`/`N/A`/`FAIL` breakdown.

Unlike the Lua corpus's reference-oracle scripts, **none of this is compared
against a real Lua 5.5.1 interpreter's output.** There is no meaningful
"matches Lua" question to ask about a typed reinterpretation of a dynamic
test file - `expected` values are self-authored from Sol's own typed
semantics. What the suite actually guards against is a silent regression in
Sol's typed semantics landing on exactly the corner cases each upstream
file's *intent* maps to.

## Ported vs. not applicable

22 files got a genuine port, each reusing a Sol typed feature that captures
the same intent as the upstream test:

| Upstream intent | Sol typed reinterpretation |
| --- | --- |
| `bitwise.lua`/`bwcoercion.lua` - bit ops and numeric coercion | Sol's native `&`\|`~`<<`>>` on `i64`; `i64`->`f64` implicit widening |
| `calls.lua`/`closure.lua` - deep/dynamic calls, closures | typed recursive calls; capture-by-value nested functions |
| `constructs.lua`/`goto.lua`/`locals.lua` - control flow, scoping | if/while/numeric-for/break; nested `do...end` scoping (no goto/labels/`<close>` in typed mode) |
| `errors.lua` - catchable errors | the valid, non-trapping side of the same boundaries `out_of_bounds.sol`/`division_by_zero.sol` already trap-test |
| `events.lua` - metatables/metamethods | function-value dispatch plus `any`/`is`/`as` narrowing (no operator overloading in typed mode) |
| `gc.lua`/`gengc.lua` - collector behavior | live-reference-survives-collection; a direct regression for the generational collector's promotion + write barrier (see [memory-management.md](memory-management.md)) |
| `math.lua` - math library | `extern function` bindings to real libm symbols (no `math.*` module in typed mode) |
| `nextvar.lua` - table/global traversal | `Map<i64, i64>` insert/lookup/`pairs` iteration (no globals or `next()` in typed mode) |
| `sort.lua` - table library sort | a hand-written insertion sort over `Array<i64>` (no sort intrinsic) |
| `strings.lua`/`utf8.lua` - string/utf8 libraries | `..`/`#`/`==`/`<` on `string`; a demonstration that `#` is a byte length, not a codepoint count |
| `vararg.lua` - varargs | an explicit `Array<i64>` argument in place of a packed vararg table (no variadic typed functions) |
| `attrib.lua`/`big.lua`/`heavy.lua`/`literals.lua`/`verybig.lua` | `<const>` locals; `Array`/struct stress; bounded heavy loops; `i64` wraparound at the boundary; large in-memory arrays |

12 files are intentional "not applicable" stubs (a short comment plus
`return 0`), because the subsystem they test does not exist for typed Sol at
all: `all.lua`/`api.lua`/`cstack.lua`/`memerr.lua` (C API), `code.lua`
(binary chunk dump/undump), `coroutine.lua` (dynamic-mode-only), `db.lua`
(debug library), `files.lua` (`io`/`os`), `main.lua` (the `lua` executable's
own CLI), `pm.lua` (pattern matching), `tpack.lua` (`string.pack`), and
`tracegc.lua` (native modules).

## A bug this suite found (fixed)

While porting `calls.lua`, an early draft called two mutually-recursive
top-level functions (`is_even`/`is_odd`) directly from `main` in the same
program. That reproducibly panicked the Cranelift JIT backend:

```
thread '<unnamed>' panicked at cranelift-jit-0.134.4/src/backend.rs:252:21:
can't resolve symbol is_odd
```

Root cause (`crates/sol/src/jit.rs`'s `promote`): `compute_inlinable`
(`codegen.rs`) marks a function inlinable unless it's *directly*
self-recursive (`is_directly_recursive` only detects a function calling
itself, not a mutual cycle). `promote`'s transitive dependency walk then
skipped compiling a real body for any callee in that `inlinable` set,
reasoning that inlined call sites never need one. But `inline_call`'s
`MAX_INLINE_DEPTH` safety net (codegen.rs) falls back to a genuine
out-of-line `call` once compile-time inline-unrolling of a recursive chain
gets 8 levels deep - which mutual recursion between two "not directly
recursive" functions hits just as easily as any other recursion. That
fallback call referenced a `FuncId` that was declared (in `Jit::new`) but
never `define_function`'d, since `promote` had skipped it - hence
`finalize_definitions` failing to resolve the symbol. This reproduced
regardless of whether `main` called one or both of the pair directly, as
long as the compile-time-unrolled chain reached the depth cap; the original
"calling only one name works" note above was an artifact of the specific
repro's recursion depth, not a distinct case.

Fixed by removing the `inlinable` short-circuit from `promote`'s dependency
walk: every real callee always gets a compiled body now, whether or not any
particular call site to it ends up inlined. `calls.sol` was restored to
genuine mutual recursion (`is_even`/`is_odd`, both called directly from
`main`), and `crates/sol/tests/fixtures/mutual_recursion.sol` (exercised via
`crates/sol/tests/programs.rs`) is a dedicated regression covering the
bytecode, native, OSR, and AOT tiers.

## Non-goals

- Byte-for-byte comparison of `.sol` output against a Lua 5.5.1 oracle - not
  meaningful, see above.
- Converting every upstream file 1:1 in the sense of *line-by-line*
  translation - several ports restructure the test entirely around a typed
  feature with a similar spirit (e.g. `events.lua`'s metamethod dispatch
  becomes function-value dispatch), since a literal line-by-line port often
  has no typed equivalent at all.
- Expanding typed Sol's feature set to make more of the 12 stubs portable -
  none of the missing subsystems (C API, coroutines, `io`/`os`, pattern
  matching, native modules) are being added as a result of this suite.
