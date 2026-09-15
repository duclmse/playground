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
- explicit sandbox and native-host capability profiles.

The current collector deliberately prioritizes semantic correctness over
throughput. Minor collection uses the complete precise root graph while the
barrier and generation interfaces stabilize; later U7 performance work may
make nursery collection selective without changing object identity or rooting
contracts.

## Transitional adapter

`sol::lua_runtime::CanonicalAdapter` can import legacy nil, boolean, integer,
float, byte-string, and table graphs. One adapter instance memoizes every
legacy table pointer, preserving repeated references and cycles. It also maps
the existing typed scalar ABI to canonical values without allocating a wrapper.

The adapter is intentionally one-way and snapshot-based. It rejects legacy
closures, native callables, iterators, userdata, and coroutine frames rather
than inventing unsafe cross-collector ownership. A migrated object graph must
have one authoritative owner; code must not mutate a legacy graph and its
canonical snapshot as though they were the same live object.

## Verified invariants

Forced-collection tests cover:

- identity, slot reuse, and stale handles;
- string interning and Lua numeric table-key canonicalization;
- `_ENV` and captured-upvalue reachability;
- weak values and ephemeron reachability;
- one-shot finalizer queueing and callback-window survival;
- suspended coroutine roots;
- old-to-young write barriers;
- stack maps that expose only declared frame slots;
- cyclic/repeated legacy-table import and typed scalar round trips.

These checks run in `scripts/test.sh` with warnings denied.

## Remaining U2 migration

U2 is complete only after production code uses canonical handles for dynamic
libraries and metatables, closures and native callables, userdata, errors,
coroutine frames, and the root `_ENV` table. The existing `Rc` trial-deletion
collector and typed arena must then cease owning production-visible objects.
The exit audit must demonstrate one identity and reachability domain under
forced collection across mixed dynamic/typed calls.
