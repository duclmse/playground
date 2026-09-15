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
native-callable, and raised-error graphs. One adapter instance memoizes object
pointers, preserving repeated references, shared cells, and cycles. Legacy globals become
an explicit shared `_ENV` upvalue. Standard-library callables use portable
`(provider, function)` registry identifiers rather than Rust function pointers.
The adapter also maps the existing typed scalar ABI to canonical values without
allocating a wrapper.

The adapter is intentionally one-way and snapshot-based. It rejects legacy
native bridge callables that have no provider registration, stateful iterators,
userdata, and coroutine frames rather than inventing unsafe cross-collector
ownership. A migrated object graph must have one authoritative owner; code must
not mutate a legacy graph and its canonical snapshot as though they were the
same live object.

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
- old-to-young write barriers;
- stack maps that expose only declared frame slots;
- cyclic/repeated legacy-table import, production-created recursive closure
  import with its real library `_ENV`, standard-library callable identity, and
  typed scalar round trips.

These checks run in `scripts/test.sh` with warnings denied.

## Remaining U2 migration

U2 is complete only after production code uses canonical handles for dynamic
libraries and metatables, closures and native callables, userdata, errors,
coroutine frames, and the root `_ENV` table. The existing `Rc` trial-deletion
collector and typed arena must then cease owning production-visible objects.
The exit audit must demonstrate one identity and reachability domain under
forced collection across mixed dynamic/typed calls.

The first production-facing semantic steps are now in place: the legacy
bytecode runtime stores each global environment in a `LuaTable`, exposes `_G`,
resolves global names through a lexically rebound/captured `_ENV`, and uses the
canonical capability profile directly. These remove the old map-only global
behavior and duplicate authority model, but do not count as canonical object
ownership: tables and upvalue cells remain `Rc` objects until the next
migration slice.
