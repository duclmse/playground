# Host/native boundary: decision record and capability matrix

Status: decided (ADR 0002, 2026-09-15) and substantively implemented. This
document is the capability matrix and implementation-status evidence that
`docs/features/milestones/u6-lua55-compatibility.md` §5 asks for. The
governing decision was already recorded before U6 began — see
`docs/decisions/0002-lua-compatibility-profile.md` — so this document does
not invent a new decision; it verifies the implementation against that
decision, corrects a stale "capability split remains partial" note in
`docs/features/lua-compatibility.md`, and names the precise, narrow gaps that
remain.

## The decision (ADR 0002, already accepted)

Lua compatibility is reported through five explicit profiles: core language,
portable standard library, native host/CLI, embedding/native modules, and a
capability-restricted sandbox. The native "fully compatible runtime" profile
is defined to include **source-compatible Lua 5.5 C API headers and
ABI-compatible linking**, so embedders and binary native modules can link
against Sol without source changes. `ltests` (PUC Lua's own internal C test
harness) is explicitly called out as test infrastructure, not public API:
"equivalent tests must cover the behavior they expose" rather than requiring
a literal port of `ltests.c`. `host-required` is explicitly a *temporary*
inventory status, never a permanent way to hide a native-profile gap.

## What is implemented

This is not a partial or aspirational profile — the embedding/native-module
half of ADR 0002 is built and passing today:

- **A real Lua 5.5 C API.** `crate/sol/src/lua_runtime/c_api.rs` (2000+
  lines) implements `lua_State`, `luaL_newstate`/`lua_close`,
  `luaL_openlibs`, the full stack-manipulation API (`lua_gettop`,
  `lua_settop`, `lua_rotate`, `lua_copy`, push/to conversions for every
  type), `lua_pushcclosure`/upvalues, userdata (`lua_newuserdatauv`,
  user values), the registry (`lua_rawsetp`/`lua_rawgetp`, `luaL_ref`/
  `luaL_unref`), metatables, `<close>` (`lua_toclose`), `lua_load`/
  `lua_dump`, `lua_pcallk`/`lua_error`, coroutines through the C API
  (`lua_newthread`, `lua_resume`, `lua_yieldk` with continuations,
  `lua_sethook`/`lua_gethook`), and warnings (`lua_setwarnf`/`lua_warning`).
  `crate/sol/include/{lua.h,lauxlib.h,lualib.h}` are the matching public C
  headers. The crate builds a real `cdylib`/`staticlib` (`libsol`,
  `Cargo.toml`'s `[lib] crate-type`), i.e. an actual linkable artifact, not
  just Rust-internal scaffolding.
- **Real native (dynamically loaded) module loading.** `c_api.rs` links
  `dlopen`/`dlsym`/`dlclose` directly (Unix; gated behind the `native_modules`
  capability field, `natives_load.rs:125,644`); `package.loadlib` and the
  `package.cpath`-driven C searcher (`search_native_loader`,
  `natives_load.rs:635-670`) resolve a `luaopen_<name>` symbol out of a real
  shared library and register it as a callable, exactly like real Lua.
- **End-to-end embedding fixture tests**, run as real compiled C programs
  against the real built library — not mocked: `crate/sol/tests/native/
  embedding_smoke.c` (an embedding host, exercising ~40 distinct API
  behaviors: C closures/upvalues, `pcall` error propagation with
  `luaL_error`, `luaL_Buffer`, userdata + uservalues, registry refs,
  raw table access, metatables, `__close`, a real coroutine driven through
  `lua_resume`/`lua_yieldk` with a continuation and a debug hook, and finally
  loading a genuine compiled native module through `require`) and
  `crate/sol/tests/native/sol_fixture.c` (the native module it loads,
  built as a real `.dylib`/`.so` and `dlopen`'d at runtime). `scripts/
  test-sol-c-api.sh` compiles and runs both on Darwin/Linux and was
  reconfirmed passing during this work (`Sol Lua 5.5 C API and native
  fixture build passed`).

This satisfies milestone §5 bullet 2 ("implement selected native services
behind explicit host traits") and bullet 4 ("add embedding/C-API/module
tests only for the approved profile") in full: the approved profile
*includes* a real C API and native module loader, and both already have
passing fixture-level tests.

## Capability matrix

| `Capabilities` field | Gates | `SANDBOX` | `NATIVE_CLI` | Corpus `requires` tag(s) |
|---|---|---|---|---|
| `package` | `require` resolving an explicitly registered in-memory or native module | deny | allow | — |
| `filesystem` | `io.open`/`io.output(path)`/`os.remove`/`os.tmpname`/file-handle natives | deny | allow | `filesystem`, `io` |
| `process` | `os.exit` | deny | allow | `os`, `standalone-cli` |
| `environment` | `os.getenv` | deny | allow | `os` |
| `clock` | `os.time`/`os.clock` | deny | allow | `os` |
| `locale` | `os.setlocale` (bookkeeping only; no real locale backend) | deny | allow | `locale` |
| `stdin` | `io.read` | deny | allow | `stdin`, `shell` |
| `stdout` | `io.write`/`file:write` | deny | allow | `io` |
| `native_modules` | `package.loadlib`/the C searcher `dlopen`-ing a real shared library (`c_api.rs`, `natives_load.rs`) | deny | allow | `native-modules` |
| `debug` | the `debug` library's introspection/hook natives | deny | allow | `dev-full`, `ltests` |
| *(embedding/C API)* | linking a host C program against `libsol` via `lua.h`/`lauxlib.h`/`lualib.h` | n/a (host-side, not gated per-script) | n/a | `c-api` |
| *(binary chunks)* | `load(_, _, "b")`/`string.dump` | always allowed (in-process envelope only; see below) | always allowed | `binary-chunks` |

`SANDBOX` denies a script's *own* access to these natives regardless of which
profile the embedding host itself was linked with — a `.lua` script running
inside a sandboxed `LuaRuntime` cannot reach the filesystem or load a native
module even though the C API used to embed it exists and is linked.

## Binary chunk load/dump: scope and format

`string.dump`/`load(_, _, "b")` are implemented (`natives_load.rs`), gated by
no capability (dumping/loading Sol's own compiled protos never touches the
filesystem, process, or any other externally-gated resource by itself), and
are format-versioned and fixture-tested:

- `lua55_binary_chunk_header()` (`natives_load.rs:15`) reproduces Lua 5.5.1's
  exact `ldump.c` header byte-for-byte (signature, version `0x55`, format
  `0`, the `LUAC_DATA` corruption-detection bytes, and the int/instruction/
  integer/number size-and-sample-value fields). A real Lua 5.5 host tool (or
  `string.unpack`) recognizes this prefix immediately.
- Immediately after that shared header, Sol's own envelope continues with a
  private `SolDmp\0\0` magic (`SOL_DUMP_MAGIC`, `natives_load.rs:32`) plus an
  opaque `u64` key resolved against a process-local `dumped_protos` registry,
  and two `u64` length fields. Sol has no portable cross-process bytecode
  serialization format (no register-file/opcode-table ABI fixed as stable for
  external consumption), so `string.dump` produces a same-process-only opaque
  handle rather than emitting bytecode it cannot actually execute standalone.
- `load_binary_chunk` (`natives_load.rs:447-532`) validates every layer of
  the envelope independently: `"truncated binary chunk"` for a short prefix
  or envelope, `"bad header in precompiled chunk"` for a mismatched Lua
  header *or* a mismatched Sol magic/unknown key (a foreign chunk — real Lua
  5.5 bytecode, a chunk from a different Sol process, or corrupted bytes —
  is rejected the same way real Lua rejects a chunk from an incompatible
  build), and `"bad binary chunk size"`/`"bad binary chunk source size"` for
  an internally inconsistent envelope.
- Three fixture tests in `crate/sol/tests/lua55_dynamic_runtime_string.rs`
  cover this directly: `dynamic_lua_runtime_string_dump_round_trips_through_load_in_binary_mode`
  (round-trip through `load(..., "b")`; rejecting `string.dump` on a native
  function), `dynamic_lua_runtime_dump_has_lua55_header_and_rejects_truncated_data`
  (header shape plus truncation), and the new
  `dynamic_lua_runtime_rejects_a_foreign_looking_binary_chunk` (a
  Lua-5.5-header-shaped but non-Sol payload — what a genuine `luac5.5` dump
  would look like to this loader — is rejected with `"bad header in
  precompiled chunk"` rather than misread). The real corpus exercises the
  same path live: `calls.lua` line 361 dumps and reloads a closure through
  this exact mechanism and passes against the pinned oracle.

True cross-process portability — loading bytecode a *real* external
`luac5.5` produced, or vice versa — is out of scope and unaffected by the C
API's existence (the C API doesn't change Sol's internal register/opcode
representation); `code.lua` (`requires = ["binary-chunks", "c-api"]`) stays
`host-required` for that reason.

## What is still `host-required`, and precisely why

Per ADR 0002, `host-required` must name a concrete, temporary gap — not stand
in for an undecided question. Ten corpus rows remain `host-required`
(`tests/lua55/manifest.toml`); the specific reason for each falls into one of
three concrete, already-scoped categories, none of which are "blocked on a
decision":

1. **`ltests`'s bespoke internal test DSL** (`api.lua`, `cstack.lua`'s final
   `if T then` block, `memerr.lua`). PUC Lua's `ltests.c` is not a public API
   surface; it's a 1400+-line custom `T.testC(...)` mini-interpreter and a
   set of raw internal-state probes (`T.stacklevel()`, allocator
   fault-injection counters) built against the reference implementation's
   specific internal data layout. ADR 0002 already anticipated this and
   requires "equivalent tests", not a literal port — `embedding_smoke.c`
   already *is* that equivalent coverage for the public API surface. What
   `ltests` additionally probes (raw C-stack depth accounting, allocator
   failure injection at a specific call count) is reference-implementation-
   internal instrumentation with no equivalent meaning against Sol's
   different stack/allocator representation, so no corpus row depending on
   the literal `T` global is expected to ever become a byte-for-byte pass;
   each such row's `pcall`/error-message-level assertions that don't touch
   `T` are exercised elsewhere in the corpus and in `tests/fixtures/lua55/`.
2. **Filesystem-path-based `require` search** (`attrib.lua`'s "default
   option" case, and `tracegc.lua`/`locals.lua`'s `require"tracegc"`, which —
   worth noting precisely — resolves to a plain *pure-Lua* sibling test file
   in the upstream suite, not a compiled native module at all; Sol's
   `require` deliberately only resolves modules explicitly registered via
   `LuaRuntime::add_module`, with no filesystem path search, per the existing
   decision already recorded in `docs/features/lua-compatibility.md`'s
   `load`/`require` checklist entries ("Native/filesystem loaders stay
   off... Path-based search... remain[s] open"). This is a real, addressable,
   already-identified follow-up (implement a `package.path`-driven filesystem
   searcher behind the existing `filesystem` capability) — not a new
   decision, and not gated on anything this document introduces.
3. **Standalone-executable process semantics** (`all.lua`, `main.lua`):
   argument parsing, REPL prompt behavior, and `readline` integration
   specific to the `lua5.5` reference executable. Sol ships its own CLI UX
   (`sol run`/`sol build`/`sol debug`) rather than a byte-for-byte `lua5.5`
   clone; this is a product decision already reflected in `main.rs`, not an
   embedding-boundary gap.

None of these is "blocked on a user decision" — each has either an existing,
documented project decision behind it, or (category 2) a concretely scoped,
buildable next step using machinery that already exists.

## Consequences for the milestone checklist

This document, together with ADR 0002 and the fixture tests it cites, is the
evidence `docs/features/milestones/u6-lua55-compatibility.md`'s host/native-
boundary section (§5) asks for. The capability-gating mechanism, the C API,
and native module loading were already implemented and tested before this
document was written; this document's job was to verify that against the
milestone's four bullets and correct the record where earlier project
documentation (this file's own first draft, and a stale line in
`docs/features/lua-compatibility.md`) understated what already exists.
