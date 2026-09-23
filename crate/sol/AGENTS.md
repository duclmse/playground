# crates/sol

Standalone Cargo crate (no workspace dependency on `crates/vm`/`crates/lua-vm`).
A Lua 5.5-compatible superset converging on one semantic runtime with optional
types and native tiers. The current implementation still has typed and dynamic
paths. Architecture overview: `docs/sol.md`; accepted future direction:
`docs/features/unified-sol-runtime-plan.md`; current normative behavior:
`docs/spec/`.

## What this crate is

- `sol` source files (`.sol`) are statically typed and compile through the
  typed pipeline (`typeck.rs` -> `codegen.rs`) straight to native code.
- `.lua` source files currently run through a separate Lua-compatibility path
  (`lua_runtime.rs`, dynamic typing via `dynamic.rs`) that must stay
  interoperable with real Lua semantics — see `crates/sol/tests/lua55.rs` and
  `crates/sol/tests/fixtures/lua55/`.
- Both share the same lexer/parser/AST front end (`lexer.rs`, `parser.rs`,
  `ast.rs`), gated by `parser::SourceMode` (`Sol` vs `Lua`), and both must
  handle **byte-oriented source** — do not require source or string-literal
  bytes to be valid UTF-8 (`lexer::lex_bytes` takes `&[u8]`, not `&str`).

## Layout

- `lib.rs` — front end: `compile`/`compile_bytes` (source -> type-checked,
  optimized `TProgram` + validated `main` return type).
- `../sol-core` — portable U2 canonical values, managed objects, precise roots,
  and tracing-GC contracts. The legacy runtime adapter is snapshot-only until
  production ownership migrates.
- `lexer.rs`, `parser.rs`, `ast.rs` — shared front end.
- `typeck.rs` — type checker (58K — the largest module; typed/dynamic boundary
  lives here).
- `optimize.rs`, `escape.rs`, `verify.rs` — IR-level passes between typeck and
  codegen.
- `codegen.rs` — Cranelift IR generation (72K, the largest file in the crate).
- `bytecode.rs`, `bccompile.rs`, `interp.rs` — bytecode tier (interpreter,
  compiled from typed IR) used before/alongside the JIT tier — see `tier.rs`
  for tiering logic and `docs/features/delivery-plan.md` for compiler
  foundations and their remaining work.
- `jit.rs` — Cranelift JIT backend; `aot.rs` + `runtime.rs` — ahead-of-time
  compilation to a standalone executable (`sol build`), with `#[no_mangle]`
  entry points `runtime.rs` exposes that AOT-linked objects call into (this is
  why `Cargo.toml` builds `crate-type = ["rlib", "staticlib"]`).
- `closures.rs`, `gc.rs`, `strings.rs`, `value.rs`, `types.rs`, `numeric.rs`,
  `aliases.rs` — runtime value representation and memory management shared
  across tiers.
- `dynamic.rs`, `lua_runtime/` — Lua-compatibility dynamic typing and runtime
  library; `lua_runtime/canonical.rs` is the transitional `sol-core` adapter.
- `lua_pattern.rs` — Lua's native pattern-matching engine (not regex; classes,
  sets, anchors, captures, `%b`/`%f`), used by `lua_runtime.rs`'s
  `string.find`/`match`/`gmatch`/`gsub`.
- `modules.rs` — multi-file `.sol` project compilation (`compile_project`,
  used by `sol build`/`sol run` for project-mode sources).
- `debug.rs` — `sol debug`'s call-boundary REPL debugger.
- `diagnostic.rs` — structured diagnostics (human text or
  `SOL_DIAGNOSTIC_JSON=1` machine-readable JSON); `main.rs` renders these.
- `profile.rs` — `--profile-out`/`--profile-in`/`--profile-time` support.
- `main.rs` — the `sol` CLI: `sol run`, `sol build -o <out>`, `sol debug`
  (all accept `.lua` or `.sol`; `.fl` is accepted as a legacy alias for
  `.sol` for backward compatibility).

## Commands

```sh
cargo test --manifest-path crates/sol/Cargo.toml   # unit + tests/programs.rs + tests/lua55.rs
scripts/test-lua55-manifest.sh                      # validates the Lua 5.5 corpus manifest
```

The full Lua 5.5 corpus/reference-oracle scripts
(`scripts/test-lua55-suite.sh`, `scripts/test-lua55-reference*.sh`) need the
pinned upstream Lua checkout/archive described in `tests/lua55/README.md`
(repo root) — don't substitute the system `lua` binary as the oracle. That
manifest is distinct from this crate's own `tests/fixtures/lua55/` (see its
own `README.md`): small, focused fixtures run directly by
`tests/lua55.rs` across bytecode/JIT/OSR/AOT tiers.

Run the CLI directly for manual checks:

```sh
cargo run --manifest-path crates/sol/Cargo.toml -- run path/to/file.sol
cargo run --manifest-path crates/sol/Cargo.toml -- run path/to/file.lua
```

## Change guidelines

- Keep typed and dynamic *representations* separate where needed for
  performance while converging on one object model, heap/GC, module graph, and
  semantic call ABI. `.lua` compatibility must not implicitly box or weaken
  proven typed hot paths.
- Preserve byte-oriented source handling; don't add a `&str`/UTF-8
  requirement to lexing or string-literal handling.
- Add focused regression coverage (`tests/programs.rs` for `.sol` behavior,
  `tests/lua55.rs` + the fixtures manifest for Lua-compatibility behavior)
  with any behavior change. For Lua-compatibility fixes, update the corpus
  manifest (`tests/lua55/manifest.toml`) and the matching checklist item in
  `docs/features/lua-compatibility.md` together.
- `codegen.rs` and `typeck.rs` are large; prefer surgical, narrowly-scoped
  edits and check `docs/features/` (implementation rationale) and
  `docs/spec/` (normative language behavior) for the invariant you're
  touching before changing tiering or type-checking behavior.
