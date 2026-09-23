# crates/sol-core

Portable semantic-runtime foundation for the unified Sol/Lua engine. This crate
must remain usable by native and WebAssembly hosts: do not add Cranelift,
filesystem, process, locale, or other ambient host dependencies.

## Invariants

- Every managed object is addressed through a generation-checked `ObjectId`.
- Store managed references through heap mutation APIs so write barriers run.
- Interpreter/native frame roots are precise and described through `StackMap`;
  do not conservatively scan arbitrary machine words.
- `_ENV` is a closure upvalue, not a parallel global-name map.
- Weak tables, ephemerons, finalizers, and suspended thread stacks are part of
  reachability semantics, not optional cleanup passes.
- Do not create a live bidirectional bridge that lets another collector own or
  mutate the same production object graph.

## Commands

```sh
cargo test --manifest-path crates/sol-core/Cargo.toml
cargo clippy --manifest-path crates/sol-core/Cargo.toml --all-targets -- -D warnings
```
