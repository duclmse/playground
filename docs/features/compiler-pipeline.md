# Typed compiler pipeline

> Status: implemented.

**Purpose**: prove the core architectural bet (typed values, SSA via Cranelift,
straight-to-native compilation) works at all, on the smallest useful language
slice. See [the language specification](../spec/README.md) for observable
behavior, `docs/sol.md` for the architecture overview, and
`benchmarks/RESULTS.md` for results.

- [x] Lexer (`lexer.rs`, `logos`) - keywords, identifiers, int/float literals,
      operators, `--` line comments.
- [x] Parser (`parser.rs`) - hand-written recursive-descent + Pratt expression
      parsing; functions, `local`, `if`/`elseif`/`else`, `while`, numeric `for`,
      `return`, array index/assign, `#` length.
- [x] Untyped AST (`ast.rs`).
- [x] Type checker (`typeck.rs`) - two-pass (signatures first, so
      recursion/forward references work), local type inference, implicit
      `i64 -> f64` widening, `i64`-only `%`, required function return types,
      block scoping.
- [x] Typed AST with resolved `LocalId`s (`types.rs`).
- [x] Codegen to Cranelift IR via `cranelift-frontend`'s `FunctionBuilder`
      (`codegen.rs`) - arithmetic, comparisons, non-short-circuiting `and`/`or`,
      `if`/`while`/numeric-`for` control flow, function calls, array load/store
      (bounds checking is covered by [Safety and correctness](safety.md)).
- [x] Array runtime support (`runtime.rs`) - `ArrayHeader`, leaked `Vec`-backed
      allocation; the current collector is documented in
      [Memory management](memory-management.md).
- [x] JIT execution (`jit.rs`, `cranelift-jit`) - declare/compile/finalize every
      function, call `main`, print its return value (`main.rs`).
- [x] Unit tests (`typeck.rs`'s `#[cfg(test)]`) + integration tests
      (`tests/programs.rs`) spawning the real binary.
- [x] Two `.sol` benchmarks (`benchmarks/fib.sol`, `table_array.sol`) wired into
      `scripts/benchmark.sh`.
- [x] `docs/sol.md` - architecture overview and Cranelift API notes.
