# Canonical runtime foundation

Status: U2 in progress, started 2026-09-15.

This document describes the first executable foundation for the single Sol/Lua
runtime promised by the [unified runtime plan](unified-sol-runtime-plan.md). It
is an implementation-status document, not a claim that production execution
has already migrated.

## Implemented foundation

`crates/sol-core` is portable and has no native code-generator or ambient OS
dependency. It currently owns:

- a 16-byte explicit-tag `Value` with Lua scalar equality and truthiness;
- generation-checked `ObjectId` handles, so a reclaimed slot cannot make an
  old reference silently address a new object;
- byte strings, array/hash tables, closures, shared upvalue cells, threads,
  userdata, error values, and object headers in one managed heap;
- closure environments represented by a designated `_ENV` upvalue;
- explicit root registration and validated stack maps for interpreter and JIT
  frames;
- young/old generations, remembered old-to-young writes, major/minor collection
  entry points, weak keys/values, ephemeron fixed-point marking, finalizer
  queues, coroutine stacks, and traceback/cause tracing;
- explicit sandbox and native-host capability profiles, including independent
  package, filesystem, process, environment, clock, locale, stdin, stdout,
  native-module, and debug authority.

The production Lua runtime now consumes this exact `sol_core::Capabilities`
type. Its previous coarse `os` and `io` switches have been removed; native
operations check the narrow authority they actually use. `os.difftime` and
`os.date` with an explicit timestamp remain available without host authority,
while clock reads, environment reads, process exit, stdin, and stdout are
independently denied by the sandbox profile. `LuaCapabilities` remains only as
a naming-compatibility type alias, not a second model; embedders using its old
coarse fields must migrate to the granular fields.

The current collector deliberately prioritizes semantic correctness over
throughput. Minor collection uses the complete precise root graph while the
barrier and generation interfaces stabilize; later U7 performance work may
make nursery collection selective without changing object identity or rooting
contracts.

## Transitional adapter

`sol::lua_runtime::CanonicalAdapter` can import legacy nil, boolean, integer,
float, byte-string, table, Lua-closure, shared-upvalue, standard-library
native-callable, stateful iterator, userdata, coroutine, coroutine-wrapper, and
raised-error graphs, including registered native Sol bridges. One adapter
instance memoizes object pointers, preserving repeated references, shared cells,
and cycles. Legacy globals become an explicit shared `_ENV` upvalue.
Standard-library callables use portable `(provider, function)` registry
identifiers rather than Rust function pointers. The heap allocates a distinct
provider namespace for each imported native bridge and the adapter retains the
corresponding resolver entry outside the portable object graph. The adapter also
maps the existing typed scalar ABI to canonical values without allocating a
wrapper.

The adapter is intentionally one-way and snapshot-based. Executable native
pointers remain only in the external bridge registry and never enter a
canonical object. Coroutine snapshots flatten every live register, vararg,
captured cell, `_ENV`, and native continuation value into the canonical
thread's traced stack. This proves reachability but does not make a snapshot
resumable; executable frame shape, program counters, and continuations move in
U3. A migrated object graph must have one authoritative owner, so code must not
mutate a legacy graph and its canonical snapshot as though they were the same
live object.

## Verified invariants

Forced-collection tests cover:

- identity, slot reuse, and stale handles;
- string interning and Lua numeric table-key canonicalization;
- `_ENV` and captured-upvalue reachability;
- native-callable capture tracing and portable provider/function IDs;
- raised-error payload identity and reachability;
- weak values and ephemeron reachability;
- one-shot finalizer queueing and callback-window survival;
- suspended coroutine roots;
- suspended production-coroutine frame values, coroutine-wrapper cycles,
  iterator state, and userdata snapshot identity;
- heap-unique native-bridge provider registration and pointer-free callable
  objects;
- old-to-young write barriers;
- stack maps that expose only declared frame slots;
- cyclic/repeated legacy-table import, production-created recursive closure
  import with its real library `_ENV`, standard-library callable identity, and
  typed scalar round trips.

These checks run in `scripts/test.sh` with warnings denied.

## Remaining U2 migration

Userdata and installed native callables (`RegisteredNative`/`CFunction`) are
already canonical in production — there is no other representation left for
either. `package.loadlib`'s native-library handles (`c_api::NativeLibrary`)
are raw `dlopen` pointers with no `LuaValue`/GC identity at all, never owned by
either collector, so there is nothing to migrate there either; the checklist
wording naming these as outstanding work was stale.

Strings are now fully canonical in production: `LuaValue::String` and
`LuaKey::String` hold a `CanonicalString` handle into `sol_core::Heap` instead
of `Rc<Vec<u8>>`. Strings are a leaf value (no outgoing references), so this
slice landed independently of tables/closures/coroutines. It surfaced a real
Lua-compatibility invariant the existing test suite already encoded: real Lua
always interns short strings (`LUAI_MAXSHORTLEN`, 40 bytes) regardless of
origin, but never interns long strings except through the compiler's own
constant-pool deduplication, so a long runtime-computed string must not alias
an existing string of equal content. The runtime therefore exposes two
allocation paths — `Heap::alloc_string`/`CanonicalString::intern` (content
deduplicating, delegated to for every length) for compile-time bytecode
constants, fixed structural/native-library labels, global-variable-name
lookups, and the canonical empty string; and
`Heap::alloc_string_fresh`/`CanonicalString::fresh` (deduplicating only at or
under the 40-byte short-string cutoff, otherwise always a distinct object) for
everything else computed at run time — string-library results, concatenation,
`tostring`/error-message construction, and the C API's equivalents
(`lua_pushlstring`, numeric-to-string coercion in `lua_tolstring`, and
dynamically loaded chunk source/name).

U2 is complete only after production code also uses canonical handles for
tables (including metatables and the root `_ENV` table, which is just a
table), closures, and coroutine frames. Unlike strings, these three cannot
move storage in isolation: `sol_core::TableObject`/`UpvalueObject`/
`ThreadObject` can only hold `sol_core::Value`, which can reference *other
sol_core heap objects* but has no representation for a legacy `Rc<LuaClosure>`
or `Rc<LuaCoroutine>` — and closures capture `LuaValue`s that may be tables,
while coroutine frames (`regs`/`upvals`/`cells`) hold both, so the three
storage backends are mutually recursive.
The target design for this slice — a cheap `Copy` `ObjectId`-based value tier
for the hot path (distinct from the existing `Rc<CanonicalObjectRoot>`-rooted
`CanonicalTable`/`CanonicalString` handles, which stay reserved for long-lived
anchors), frame-walk-based rooting at defined GC safepoints, and the still-open
question of how a coroutine's frames become conditional GC roots without
leaking the coroutine-cycle case `lua_runtime::coroutine::LuaCoroutine` already
documents as a known gap — is written up in
[table-closure-coroutine-cutover.md](table-closure-coroutine-cutover.md).
`sol_core::TableObject::hash` moved from `HashMap` to an
order-preserving `IndexMap` (matching `LuaTable::hash`'s existing rationale) as
a prerequisite, since production tables cannot move onto it correctly
otherwise. The existing `Rc` trial-deletion collector and typed arena must then
cease owning production-visible objects. The exit audit must demonstrate one
identity and reachability domain under forced collection across mixed
dynamic/typed calls.

The first production-facing semantic steps are now in place: the legacy
bytecode runtime stores each global environment in a `LuaTable`, exposes `_G`,
resolves global names through a lexically rebound/captured `_ENV`, and uses the
canonical capability profile directly. These remove the old map-only global
behavior and duplicate authority model, but do not count as canonical object
ownership: tables and upvalue cells remain `Rc` objects until the next
migration slice.
