## Verdict

Yes-the project has materially drifted from your stated purpose.

The individual components are progressing, but they currently form three loosely
connected projects:

1. A statically typed Lua-like language optimized for native execution.
2. A separate Lua 5.5 compatibility interpreter.
3. A browser Lua debugger powered by another VM.

The repository does not yet deliver one coherent “Sol is a Lua superset”
product.

| Intended outcome             | Current state                                                           | Assessment        |
| ---------------------------- | ----------------------------------------------------------------------- | ----------------- |
| Sol is a Lua superset        | .sol and .lua select different languages and runtimes                   | Material drift    |
| Fully Lua-compatible runtime | 26/34 upstream cases pending, 8 host-required, 0 passing complete cases | Far from complete |
| Faster than LuaJIT           | Typed programs sometimes win; dynamic Lua is dramatically slower        | Goal was narrowed |
| Sol web playground           | Functional Lua playground using the Piccolo-based runtime               | Wrong runtime     |
| LSP with VS Code client      | Early LSP exists; no real Sol VS Code extension                         | Partial prototype |

## Where the drift happened

The repository now defines Sol as a typed, Lua-compatible language designed to
beat LuaJIT only on typed workloads-not as a complete Lua superset that beats
LuaJIT. That narrower mission is explicit in docs/sol.md:3.

More importantly, source extensions select separate languages:

- .sol selects typed Sol.
- .lua selects Lua compatibility mode.

This is specified in docs/spec/source-and-lexical-grammar.md:3. Typed Sol is not
a syntactic or semantic superset of Lua: it lacks Lua features such as dynamic
globals, metatables, variadic typed functions, coroutines, standard I/O, and
other runtime behavior. The typed conformance document itself calls .sol “a
different, statically typed language” in docs/features/sol-conformance.md:8.

That means the current design is closer to:
```
Sol distribution
 ├── typed Sol language
 └── separate Lua compatibility mode
```
rather than:

Sol language = all Lua syntax and semantics + optional Sol extensions

## Runtime compatibility status

The dynamic runtime has substantial functionality: byte-oriented parsing,
tables, closures and upvalues, varargs, multiple returns, metatables,
coroutines, pattern operations, libraries, budgets, and cycle collection. This
is real progress.

But the compatibility gate remains at:

- 34 upstream Lua 5.5.1 top-level cases classified
- 26 pending
- 8 host-required
- 0 complete upstream cases promoted to pass

That status is documented directly in docs/features/lua-compatibility.md:18.

Major remaining areas include \_ENV, complete closing/finalization semantics,
weak-key and GC fidelity, library coverage, module loading, debug facilities,
and the typed/dynamic call boundary.

The typed “Sol conformance” suite does not improve that compatibility score. It
contains 22 hand-written typed reinterpretations and 12 not-applicable stubs,
and is explicitly not compared with Lua output:
docs/features/sol-conformance.md:31.

## Performance status

The typed native compiler is the strongest part of the project. It includes
bytecode, Cranelift JIT, OSR, AOT, specialization, GC, and optimization work. On
selected benchmarks it beats LuaJIT, although not universally; for example,
LuaJIT still wins the typed object-allocation benchmark.

The Lua-compatible runtime is nowhere near the stated performance goal. Its
measured baseline was approximately:

- fib: 133× slower than LuaJIT
- function calls: 230× slower
- objects: 334× slower
- coroutine resume: 28× slower

See the full table in benchmarks/RESULTS.md:523.

Recent frame pooling and field-key optimizations have improved it meaningfully,
but it remains much slower even than the repository’s other unoptimized Lua VM.
Most importantly, the project explicitly decided to narrow the performance claim
to typed .sol rather than pursue dynamic LuaJIT performance:
benchmarks/RESULTS.md:560.

That decision directly conflicts with your intended purpose.

The separate LuaValue layer is not itself the problem. Full Lua semantics
require boxed dynamic values, identity, tables, metatables, closures, threads,
and userdata. The drift is treating this runtime as a permanently second-class
compatibility mode exempt from the performance goal. Internally separate typed
and dynamic representations are sensible; externally they need one language,
module system, runtime contract, and optimization strategy.

## Web playground status

The web application builds successfully and is already a capable Lua
IDE/debugger. However, it is not a Sol playground.

It imports @lua-playground/runtime in apps/web/src/lua-worker.ts:2, which comes
from the separate Piccolo-based crates/lua-vm. The UI is .lua-only:

- Monaco is fixed to Lua in apps/web/src/App.tsx:559.
- Project filenames must end in .lua in apps/web/src/project.ts:111.

The original web product brief even lists Lua 5.5 and LuaJIT support as
non-goals: docs/product- brief.md:35. That is a different product mission from
the one you just specified.

Because the browser cannot execute the current native Cranelift output directly,
Sol needs either:

- its canonical bytecode interpreter compiled to WebAssembly, or
- a compiler backend that emits browser WebAssembly.

The existing debugger UI can then remain while its VM adapter changes to Sol.

## LSP and VS Code status

An early crates/sol-lsp implementation exists and builds cleanly. It supports
diagnostics, hover, completion, definitions, references, symbols, rename, and
signature help.

However:

- The crate and its documentation are currently untracked, so they are not part
  of repository history.
- It has zero automated tests.
- References and rename are textual rather than semantically scoped.
- It is single-file only.
- Diagnostics return only the first error.
- There is no actual Sol VS Code extension.

These limitations are recorded in docs/sol-lsp.md:57. The documented
installation method is merely to point a generic LSP client at the binary:
docs/sol-lsp.md:85.

The untracked LuaHelper directory is a reference project, not a VS Code client
wired to sol-lsp.

## Recommended correction

The next milestone should be a convergence milestone, not another isolated
feature milestone:

1. Define Sol as “Lua 5.5 accepted unchanged, plus optional types and
   extensions.” File extension may control defaults or diagnostics, but not
   create two incompatible languages.
2. Make crates/sol the canonical runtime for the CLI, web playground, LSP,
   debugger, and benchmarks. Retain crates/lua-vm only as a comparison/oracle
   during migration.
3. Establish an exact compatibility contract: source semantics, standard
   library, package behavior, CLI, debug API, C API, binary chunks, and host
   capabilities. If C API compatibility is excluded, describe Sol as
   source-compatible rather than fully runtime-compatible.
4. Gate compatibility on unchanged upstream Lua tests. Typed ports should remain
   useful regression tests but should not be called Lua conformance.
5. Put dynamic Lua performance back inside the main performance objective. Start
   with interpreter efficiency and inline caches, then add baseline/native
   specialization or JIT support.
6. Port the canonical Sol runtime or compiler to WebAssembly and switch the web
   playground away from Piccolo.
7. Commit and harden sol-lsp, then add an editors/vscode-sol extension that
   launches it, registers .sol and .lua, manages the server binary, and includes
   integration tests.

Build health is good: the Sol test suite passed all 199 tests, Sol and LSP
passed strict Clippy, and the web production build completed. The issue is
therefore not project quality-it is that the current acceptance criteria and
integration architecture no longer match the product you want.
