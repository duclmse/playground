# Repository guide

This repository contains two related but independent Lua efforts:

- `crates/lua-vm` is the browser runtime, built on the vendored `crates/vm`
  (a patched Piccolo fork) and compiled to WebAssembly for `apps/web`.
- `crates/sol` is a separate typed, native-oriented Sol compiler/JIT. Its
  compatibility and delivery plan is in `docs/sol-roadmap.md`.

Do not assume a change to one runtime applies to the other. Keep their
semantics, tests, and documentation separate unless a task explicitly spans
both.

## Layout

- `apps/web` — Vite + React + TypeScript playground UI.
- `packages/lua-runtime` — wasm-bindgen package consumed by the web app;
  generated `pkg/` contents come from `scripts/build-wasm.sh`.
- `crates/lua-vm` — WASM-facing runtime, debugger session, conformance tests.
- `crates/vm` — vendored Piccolo fork. Preserve fork-marker comments and read
  `crates/vm/README.md` before changing its debugger-facing internals.
- `crates/dap-server` — native Debug Adapter Protocol server using `lua-vm`.
- `crates/sol` — standalone Cargo crate with compiler, interpreter, JIT, AOT,
  and Lua-compatibility fixtures.
- `docs` — architecture, protocol, conformance, and roadmap documents.
- `tests/lua55` — Lua 5.5 corpus manifest and reference-oracle metadata.

## Commands

There is no root Cargo workspace. Run Rust commands against the relevant
manifest:

```sh
cargo test --manifest-path crates/lua-vm/Cargo.toml
cargo test --manifest-path crates/sol/Cargo.toml
cargo test --manifest-path crates/dap-server/Cargo.toml
```

Use the project wrappers for cross-component checks:

```sh
scripts/test.sh                 # lua-vm tests, corpus-manifest check, web build if deps exist
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
  for the browser/debugger, `docs/sol.md` and `docs/sol-roadmap.md` for Sol.
- Keep the browser runtime sandboxed: host filesystem, OS, and native loading
  capabilities must stay explicit.
- Keep Sol's typed and dynamic representations separate. `.lua` compatibility
  features must not implicitly box or weaken typed `.sol` hot paths.
- Add focused regression coverage with behavior changes. For Lua compatibility,
  update the corpus manifest/fixture and status documentation together.
- Preserve byte-oriented Lua source handling in Sol; do not require source or
  string-literal bytes to be valid UTF-8.
- Avoid modifying generated output, lockfiles, vendored `crates/vm`, or broad
  documentation rewrites unless the task calls for them.
- The worktree may contain user changes. Inspect `git status` first and do not
  revert, reformat, or overwrite unrelated edits.

## Completion

Run the narrowest relevant test first, then the appropriate crate/project
check when practical. In the handoff, state what changed, which commands
passed, and any required external prerequisite that prevented a broader check.
