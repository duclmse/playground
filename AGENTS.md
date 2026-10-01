# Repository guide

This repository is converging on one Sol product: a Lua 5.5-compatible
superset with optional types, a shared semantic runtime, native tiers, a web
playground, and editor tooling. The authoritative future roadmap is
`docs/features/unified-sol-runtime-plan.md`.

The current implementation still contains two runtime efforts during that
migration:

- `crate/lua-vm` was the prior browser runtime, built on the vendored Piccolo
  fork `crates/vm`. That fork has been retired (deleted, not just excluded)
  and `crate/lua-vm`'s own source was never updated to match, so it and its
  DAP consumer (`crate/dap-server`) no longer compile; both stay outside the
  root Cargo workspace as migration history until the `sol-core` WASM adapter
  replaces their browser-facing interfaces (see
  `docs/features/unified-sol-runtime-plan.md` §3.1). Do not port them back
  onto a resurrected `vm` without checking that plan first.
- `crates/sol` contains the native compiler/JIT and the in-progress Lua 5.5
  runtime that will become canonical. Its implementation status is in
  `docs/features/` and its current language contract is in `docs/spec/`.
- `crates/sol-core` is the portable, host-independent foundation for the
  canonical value/object model, precise roots, and tracing collector.
  Production tables/closures/coroutines finished migrating onto it (U2,
  completed 2026-09-26; see `docs/features/unified-sol-runtime-plan.md` and
  `docs/features/table-closure-coroutine-cutover.md`).
- `crates/decompiler` is an independent recovery-oriented decompiler for
  raw Sol instruction words and version-matched Lua binary chunks.

Do not assume a change to one runtime already applies to the other. During the
migration, keep differential tests and current-status documentation explicit,
but do not introduce new permanent product semantics that depend on choosing a
runtime by file extension.

## Layout

- `apps/web` — Vite + React + TypeScript playground UI.
- `packages/lua-runtime` — wasm-bindgen package consumed by the web app;
  generated `pkg/` contents come from `scripts/build-wasm.sh`.
- `crate/lua-vm` — former WASM-facing runtime/debugger session; retired
  along with `crates/vm` (see the migration note above) and excluded from the
  root Cargo workspace. Does not currently compile.
- `crate/dap-server` — native Debug Adapter Protocol server that depended on
  `lua-vm`; excluded from the root Cargo workspace for the same reason.
- `crates/sol` — standalone Cargo crate with compiler, interpreter, JIT, AOT,
  and Lua-compatibility fixtures.
- `crates/sol-core` — portable canonical runtime value/heap/GC foundation; no
  native code generator or ambient host-OS dependency.
- `crates/decompiler` — standalone library/CLI that emits annotated
  Sol-style register code from Sol bytecode or `luac` listings/chunks.
- `crates/sol-lsp` — standalone LSP server for both `.sol` and `.lua`, built
  on `crates/sol`. See `docs/sol-lsp.md`.
- `docs` — architecture, protocol, conformance, feature, and specification documents.
- `tests/lua55` — Lua 5.5 corpus manifest and reference-oracle metadata.

## Commands

The root Cargo workspace covers first-party crates and excludes `crate/lua-vm`
and `crate/dap-server` (retired migration history, see above). Use it for
cross-crate validation, or run commands against a specific manifest for
focused work:

```sh
cargo test --workspace
cargo test --manifest-path crates/sol-core/Cargo.toml
cargo test --manifest-path crates/sol/Cargo.toml
cargo test --manifest-path crates/decompiler/Cargo.toml
cargo build --manifest-path crates/sol-lsp/Cargo.toml
```

Use the project wrappers for cross-component checks:

```sh
scripts/test.sh                 # lua-vm tests, corpus-manifest check, web build if deps exist
scripts/project-status.sh --check # honest compatibility/capability/architecture summary
npm run build --workspace=apps/web
scripts/build-wasm.sh           # regenerates packages/lua-runtime/pkg after lua-vm changes
```

`scripts/build-wasm.sh` requires the `wasm32-unknown-unknown` target and the
`wasm-bindgen` CLI. Do not hand-edit generated files in
`packages/lua-runtime/pkg`; regenerate them only when the task requires it.

For Sol's Lua 5.5 work, run the focused crate tests and manifest validation:

```sh
cargo test --manifest-path crates/sol/Cargo.toml
scripts/test-lua55-manifest.sh
```

The full corpus/reference scripts need the pinned upstream Lua checkout or
archive described in `tests/lua55/README.md`; do not silently substitute the
system Lua executable as its oracle.

## Change guidelines

- Read the applicable document before changing a subsystem: `docs/README.md`
  for the browser/debugger; `docs/sol.md`, `docs/features/`, and `docs/spec/`
  for Sol.
- Keep the browser runtime sandboxed: host filesystem, OS, and native loading
  capabilities must stay explicit.
- Keep Sol's typed and dynamic *representations* distinct where performance
  requires it, while converging them on one object model, heap, GC, module
  graph, and call ABI. `.lua` compatibility must not implicitly box proven
  typed hot paths.
- Add focused regression coverage with behavior changes. For Lua compatibility,
  update the corpus manifest/fixture and status documentation together.
- Put one-off diagnostic or exploratory test files (e.g. a bytecode dump used
  to root-cause a bug) in that crate's `scratch/` directory (e.g.
  `crates/sol/scratch/`) instead of `tests/`. `scratch/` is gitignored, so
  files there are never committed and are not auto-discovered by `cargo test`;
  copy a file into `tests/` temporarily if you need it to actually run.
- Preserve byte-oriented Lua source handling in Sol; do not require source or
  string-literal bytes to be valid UTF-8.
- Avoid modifying generated output, lockfiles, or broad documentation
  rewrites unless the task calls for them.
- The worktree may contain user changes. Inspect `git status` first and do not
  revert, reformat, or overwrite unrelated edits.

## Completion

Run the narrowest relevant test first, then the appropriate crate/project
check when practical. In the handoff, state what changed, which commands
passed, and any required external prerequisite that prevented a broader check.
