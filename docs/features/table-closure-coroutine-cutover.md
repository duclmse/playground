# Tables, closures, and coroutines onto the canonical heap

Status: design, implementation not started. Written 2026-09-25.

This is the design for the last slice of U2 (see
[unified-sol-runtime-plan.md](unified-sol-runtime-plan.md) and
[canonical-runtime-foundation.md](canonical-runtime-foundation.md)): moving
`LuaTable`, `LuaClosure`, and `LuaCoroutine` off their own `Rc` graphs and
`gc.rs`'s trial-deletion cycle collector onto `sol_core::Heap`, alongside the
already-migrated strings, userdata, and native callables. It is grounded in a
direct reading of `sol_core::heap.rs`'s existing object model and of the
production `Rc`-based representations in `crate/sol/src/lua_runtime` (`value.rs`,
`frame.rs`, `coroutine.rs`, `gc.rs`, `canonical.rs`).

## 1. Why one document for three object kinds

`unified-sol-runtime-plan.md` already notes these three "must land together...
there is no intermediate state where only one of the three is canonical while
the others still hold `Rc` references into it." **Correction (found while
starting implementation): this is not merely an exit/verification criterion —
it is a hard storage-layer blocker.** `sol_core::TableObject::array`/`hash`,
`UpvalueObject::value`, and `ThreadObject::stack`/`yielded` are all typed as
`sol_core::Value`, a compact tagged union that can reference *only* other
`sol_core` heap objects via `ObjectId`. It has no variant that can hold an
opaque `Rc<LuaClosure>` or `Rc<LuaCoroutine>` pointer. Real Lua programs
routinely store closures and coroutines as table values and as captured
upvalues (`t.callback = function() ... end`), so flipping a table's *storage*
onto `sol_core::TableObject` is not safely shippable — even behind a dual-rail
`LuaValue` variant — until closures and coroutines are themselves
`sol_core::Value`-representable (i.e. `ClosureRef`/`ThreadRef` over real
`ClosureObject`/`ThreadObject` allocations already exist). The same is true in
every other direction: an upvalue cell cannot hold a table value, and a
coroutine's stack cannot hold a table or closure value, unless that value's
kind is already canonical. The three storage backends are mutually
recursive and cannot flip one at a time.

What *can* happen incrementally, without triggering that blocker, is
everything that stops short of making any object's internal storage actually
hold `sol_core::Value` payloads sourced from real running programs:
`TableRef`/`ClosureRef`/`ThreadRef` newtypes, `Heap` wrapper/helper methods,
the closure-prototype registry, and unit tests against `sol_core::Heap`
directly (synthetic values, not values threaded through the live `LuaValue`
enum). The actual flip of `LuaValue`/`LuaKey`'s `Table`/`Closure`/`Thread`
variants to canonical payloads has to land as one coordinated change once all
three ref types and their allocation paths exist. Section 8 gives the
sequencing this document recommends, with this constraint folded in; the rest
of the document is the target shape each piece converges on.

## 2. Value representation: two tiers, not one

`sol::lua_runtime::value::CanonicalTable`/`CanonicalUserdata`/`CanonicalString`
are `Rc<CanonicalObjectRoot>` handles: constructing one calls
`Heap::add_root`, and `CanonicalObjectRoot::drop` calls `Heap::remove_root`.
Critically, `root_existing` mints a **new, independent `RootId`** every time
it's called, even for an `ObjectId` that's already rooted elsewhere — there is
no dedup on `(heap, object)`. That's the right cost model for the few
long-lived handles that use it today (the embedding registry, userdata
returned across the C API), but it is the wrong cost model for a `LuaValue`
that lives in a bytecode register, a table's array/hash slot, or a closure's
upvalue vector: those are copied, cloned, and dropped on every instruction,
and mirroring `CanonicalTable`'s `add_root`/`remove_root` pair onto that
volume would turn every register move into a heap-registry mutation.

So tables, closures, and coroutines need a second, cheap tier:

```rust
// Copy, no Rc, no Drop side effect. Distinct newtypes per kind purely so the
// type checker catches passing a table's id where a closure's is expected —
// `sol_core::ObjectId` itself doesn't distinguish kinds statically.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct TableRef(sol_core::ObjectId);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ClosureRef(sol_core::ObjectId);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct ThreadRef(sol_core::ObjectId);
```

`LuaValue::Table(TableRef)`, `LuaValue::Closure(ClosureRef)`,
`LuaValue::Thread(ThreadRef)` (and the matching `LuaKey` arms) replace the
`Rc<RefCell<LuaTable>>`/`Rc<LuaClosure>`/`Rc<LuaCoroutine>` variants. `LuaKey`'s
current hand-written `Rc::ptr_eq`-based `PartialEq`/`Hash` is replaced by
deriving from `ObjectId`'s own equality — `ObjectId` is already
generation-checked (a stale handle from a reclaimed slot compares unequal to a
live one addressing the same slot number), which is a *strictly* better
identity primitive than a raw pointer for exactly the "stale handle" bugs
`rawequal`/table-key hashing must not have.

`CanonicalTable` (the `Rc<CanonicalObjectRoot>` wrapper) does not go away: it
stays exactly what it is today, used only where an object must outlive any
single frame/root scan by construction — the registry table, and any table an
embedder holds via `lua_ref`/a persistent C handle. Everything reachable from
Lua-visible registers, table contents, upvalues, and coroutine frames uses the
cheap `*Ref` tier instead.

## 3. Root and safepoint discipline

The cheap tier's safety rests entirely on `sol_core::Heap::collect_major_with_roots(&mut self, frame_roots: &[Value])`
and the existing `StackMap`/root-registration interfaces already built and
tested in `sol-core` (this is precisely what `CanonicalAdapter::import_frame_roots`
already does for the adapter's snapshot tests — that method is the working
blueprint, not new design). Two things must be true for a `TableRef`/`ClosureRef`/
`ThreadRef` sitting in a Rust local to survive a collection:

1. **Collection only runs at defined safepoints**, never in the middle of a
   native function's own Rust-level logic. Concretely: the trigger moves from
   `gc.rs`'s per-instruction `charge_allocation` heuristic to a check made only
   at the top of `LuaRuntime::drive`'s dispatch loop (between two complete
   bytecode instructions) and immediately before/after a native-function
   invocation from the trampoline — never from inside a native builtin's own
   body. A native builtin that holds a `TableRef` across a call that could
   itself trigger a *nested* collection (e.g. `table.sort`'s comparator
   callback) must ensure that value is already reachable through something the
   collector will scan (its own arguments/return slot on the frame stack
   satisfy this automatically; a value stashed only in a local `Vec` inside the
   builtin's Rust closure would not, and such helpers must route values through
   the frame instead of a bare local across a reentrant call).
2. **Every live frame is walked as `frame_roots` at each such safepoint.** This
   is not new work to invent — `LuaFrame` (`frame.rs`) is already a
   heap-resident, explicit-stack struct sitting on `LuaRuntime::frames`, not a
   native Rust call frame, specifically so it can persist across
   non-recursive `dispatch_step` calls. Once `regs`, `cells`, `varargs`,
   `to_close`, and `upvals` hold `sol_core::Value`/`ObjectId` instead of
   `LuaValue`/`RcRef<LuaValue>`, walking `LuaRuntime::frames` end to end and
   collecting every slot's `Value` *is* the `frame_roots` argument. `Globals`
   (`_ENV`) and the registry are two more fixed fields to include the same way
   — no persistent `RootId` needed for them either, since `LuaRuntime` itself
   is always the thing driving collection and can always supply its own
   fields.

`Cells` (`Vec<Option<RcRef<LuaValue>>>`) becomes `Vec<Option<sol_core::ObjectId>>`
addressing `UpvalueObject`s directly — see §5.

## 4. Tables

`sol_core::TableObject` (`array: Vec<Value>`, `hash: IndexMap<TableKey, Value>`,
`metatable: Option<ObjectId>`, `weak_keys`/`weak_values: bool`, `version: u64`)
already matches `LuaTable`'s shape closely enough that this is the mechanical
part of the cutover. `Heap::table_get`/`table_set`/`table_entries` already
implement positive-integer array-part addressing, tombstone-on-nil-overwrite
(`table_entries` already filters `Value::NIL` tombstones out of both the array
and hash parts when snapshotting for `next`/`pairs`), and metatable/weak-mode
storage. Three gaps found by direct inspection that block a straight swap:

- **No `#`-operator length cache.** `TableObject` has no equivalent of
  `LuaTable::array_border`'s O(1)-amortized border cache; `table_entries`
  cloning the whole array/hash on every call is also too expensive to be the
  length operator's implementation. Add a border cache field to `TableObject`
  (or a cheap recompute-on-append heuristic) before tables go live — this is
  new `sol-core` work, not present today.
- **No allocation-byte accounting.** `Heap` has no `live_bytes`/`object_count`-style
  API. `collectgarbage("count")` currently reports `gc.rs`'s own
  `charge_ledger`; once that ledger is deleted with the rest of the trial
  collector, something must replace it. Add a running byte estimate to `Heap`
  (updated in `alloc`/sweep) rather than trying to preserve per-`LuaTable`
  charge bookkeeping across the cutover.
- **`__gc` finalization** should move onto `sol-core`'s already-generic
  `ObjectHeader::FinalizerState`/`register_finalizer`/`finish_finalizer`
  machinery (already covered by `sol-core`'s own finalizer-queue tests) instead
  of `gc.rs`'s ad hoc collector-triggered callback. This is a net simplification,
  not new work — the generic mechanism already exists and is unused by
  production tables today.

`Globals` becomes a thin wrapper: a `TableRef` for `_ENV`'s table plus whatever
non-heap bookkeeping it keeps today (it needs no separate migration step, matching
`unified-sol-runtime-plan.md`'s existing note that the global environment is
just a table).

## 5. Closures and upvalues

`sol_core::ClosureObject { prototype: u32, upvalues: Vec<ObjectId>, environment: usize }`
and `UpvalueObject { value: Value, open_stack_slot: Option<u32> }` already model
Lua's open/closed upvalue distinction (`open_stack_slot` is exactly the
"aliases a live register until closed" state `LuaFrame`'s own upvalue-closing
logic needs). Two things are missing before `alloc_closure` can be driven from
production:

- **A real prototype registry.** `ClosureObject::prototype` is a bare `u32`,
  not a `Proto`; today only `CanonicalAdapter`'s tests populate it, via an ad
  hoc counter. Production needs a `Vec<Rc<Proto>>` (or equivalent) on
  `LuaRuntime`, assigning each distinct `Proto` (memoized by `Rc::as_ptr`,
  mirroring how `CanonicalAdapter` already memoizes object pointers to
  preserve sharing/cycles on import) a stable `u32` the first time a closure is
  made from it, so `alloc_closure`'s `prototype` id can be resolved back to
  the real `Proto` (bytecode, constants, metadata) whenever the interpreter
  needs it.
- **Upvalue cells drop `Rc<RefCell<_>>` entirely.** `Cells` becomes
  `Vec<Option<ObjectId>>` over `UpvalueObject`s allocated with
  `Heap::alloc_upvalue`; reading/writing a captured variable becomes
  `heap.object(id)`/`Heap::set_upvalue(id, value)` instead of
  `RefCell::borrow`/`borrow_mut`. `debug.upvalueid`/`upvaluejoin`'s opaque
  identity becomes the `ObjectId` itself (already exactly the right shape:
  stable, comparable, generation-checked) in place of today's `Rc::as_ptr`.

## 6. Coroutines — the open question

This is the one place the design isn't fully closed, and it deserves its own
follow-up before implementation starts.

`LuaCoroutine` (`coroutine.rs`) is *not* a flat value today: `frames:
RefCell<Vec<Frame>>` is its own explicit Lua call-frame stack, separate from
`LuaRuntime::frames`, swapped in for the duration of each `resume()`. This
already matches the right shape for the cutover — the coroutine's *executable*
state (registers, cells, varargs, pending continuations, hooks,
`dead_error`) should stay an ordinary Rust structure in a side table, not
become new hand-traced `HeapObject` fields; `sol_core::ThreadObject` should
stay the thin, portable **identity and reachability proxy** for it, mirroring
`NativeCallableObject`'s existing `(provider, function)` portable-registry-key
pattern rather than trying to inline frame state into the heap.

The genuinely open part: today `LuaCoroutine` is kept alive purely by
`Rc<LuaCoroutine>` refcounting, and the code's own doc comment already states
the known consequence — *"coroutines are not tracked by `collect_cycles`... a
reference cycle routed through a coroutine... leaks."* Closing that gap (which
the U2 exit criterion — "no production object is owned by two independent
collectors" — implies we should) requires that a coroutine's frame contents
only count as GC roots **when the coroutine's own `ThreadObject` is itself
reachable from something else** — otherwise an unreachable coroutine's frames
would unconditionally keep alive everything they reference forever, and the
coroutine itself would never become collectible. That is a conditional-root
question, structurally identical to the ephemeron fixed-point marking
`Heap::mark_ephemerons` already implements for weak-keyed tables (a value is
kept only once its key is independently known reachable, recomputed to a fixed
point). `Heap::collect_major_with_roots` today only accepts one flat,
unconditional `frame_roots: &[Value]` list — there is no existing API for "these
external values are roots, but only once object X is independently marked."

Two ways forward, in order of preference:

1. **Extend `sol-core` with a conditional/deferred-root hook** — e.g. a callback
   the mark phase can invoke with newly-marked `ObjectId`s, letting the sol
   crate answer "this is a registered coroutine's `Thread` object; here are its
   frames' values to feed back into the mark queue," iterated to a fixed point
   exactly like ephemerons. This is new `sol-core` design/implementation work,
   not something to improvise inside the sol crate, and should be scoped and
   tested as its own slice before tables/closures/coroutines get their final
   cutover commit.
2. **Interim fallback**: keep treating every currently-registered (still
   `Rc<LuaCoroutine>`-alive) coroutine's frames as unconditional roots every
   collection. This is strictly simpler and requires no new `sol-core` API,
   but does not close the cycle-through-a-coroutine leak — a coroutine kept
   alive only by a cycle through its own frames never becomes `Rc`-dead, so its
   frames never stop being fed back as roots either. This is explicitly no
   worse than today's documented limitation, just now expressed through the
   new representation, and can be shipped as a scoped, called-out gap rather
   than blocking the rest of the cutover on it.

**Decided: (2), the interim fallback**, so table/closure migration isn't
blocked on a new tracing-collector feature. `canonical.rs`'s `CoroutineRegistry`
already implements this: every entry it holds is fed back as an unconditional
root on each collection while present, and its doc comment states the gap this
leaves open. Closing that gap via (1) — a conditional/deferred-root `sol-core`
hook mirroring `mark_ephemerons`'s fixed-point approach — is filed as its own
follow-up design task, scoped and tested independently of the rest of this
cutover, not a blocker for task #11's flip.

`LuaValue::Thread(ThreadRef)` addresses a lightweight `ThreadObject`; its
`LuaCoroutine` (executable frames, hook, `dead_error`) is owned by the
`LuaRuntime`-level `CoroutineRegistry`, keyed by the `ThreadObject`'s own
`ObjectId` (already unique and generation-checked, so no separate slot-index
field on `ThreadObject` is needed) rather than by an `Rc` refcount. Sweeping a
`Thread` object whose id has no live `CoroutineRegistry` entry removes nothing
further; sweeping one whose entry is confirmed unreachable (once (1) exists —
under (2), only once its owning `LuaRuntime` explicitly removes the entry)
drops that entry's `frames`, `hook`, and `dead_error` state.

## 7. What gets deleted

Once every call site is flipped: `gc.rs`'s entire trial-deletion collector
(`mark_reachable`/`collect_cycles`/`sweep_weak_tables`/`track_table`/
`track_closure` and `AUTO_GC_INSTRUCTION_INTERVAL`'s triggering), `LuaTable`,
`LuaClosure`, `RcRef<LuaTable>`/`WeakRef<LuaTable>`, the old `Cells` alias, and
`LuaRuntime`'s `gc_tables`/`gc_closures`/`weak_tables`/`charge_ledger` fields.
`collectgarbage()`'s handlers in `natives_core.rs` move from `gc.rs`'s
collector to `Heap::collect_minor`/`collect_major` plus whatever byte-count API
§4 adds.

## 8. Sequencing

The exit criterion is combined (§1), but not every step needs to be a single
patch. `LuaValue`/`LuaKey` already carry legacy and canonical variants side by
side for strings and for the embedding-only `CanonicalTable`, and that same
incremental pattern applies to steps 1–3 below: prerequisites, ref-type
plumbing, and the coroutine rooting decision can each land as their own
reviewable, always-green change. Only step 4 — the actual `LuaValue`/`LuaKey`
storage flip — cannot be split per object kind (§1's mutual-recursion
constraint), unlike the string migration, which had no such constraint:

1. **`sol-core` prerequisites**: `TableObject` border cache; `Heap` byte-count
   API; confirm `alloc_upvalue`/`alloc_thread`/`alloc_closure` cover every
   field production needs (mostly already true per §4–§6). *(Done.)*
2. **Ref-type and allocation plumbing, for all three at once, with no
   `LuaValue` change yet**: `TableRef`/`ClosureRef`/`ThreadRef` newtypes;
   `LuaRuntime`/`Heap` wrapper methods for table get/set/len/next, closure
   creation, and upvalue read/write; the closure prototype registry; the
   coroutine side-table registry per §6. This is tested directly against
   `sol_core::Heap` (synthetic values), not through the live `LuaValue` enum,
   so it carries no storage-layer risk and can land incrementally with its own
   coverage, same as the string migration's helpers did. *(Done.)*
3. **Coroutine conditional-root decision (§6)**: decide interim fallback vs.
   the conditional-root `sol-core` extension *before* the flip, since
   frame-rooting has to work correctly from the moment real coroutine frames
   go canonical, not be retrofitted after. *(Done — interim fallback (2); see
   §6.)*
4. **The coordinated flip**: because `TableObject`/`UpvalueObject`/
   `ThreadObject` storage is mutually recursive (§1), `LuaValue`/`LuaKey`'s
   `Table`, `Closure`, and `Thread` variants move to their canonical payloads
   together, and every construction/read/write call site across table
   literals/indexing/`table.*`, closure creation/upvalues, and
   `coroutine.*`/resume/yield is converted in the same change. This is
   necessarily the largest single patch in the sequence — it cannot be
   split further by object kind — but step 2's plumbing means it is a
   mechanical call-site rewrite onto already-built and already-tested
   primitives, not new design work. `LuaTable`, `LuaClosure`,
   `Rc<LuaCoroutine>`-as-ownership, and the old `Cells` alias are deleted once
   nothing constructs them.
5. **Safepoints and cleanup**: move collection triggering to the dispatch-loop
   safepoint (§3), delete `gc.rs`'s collector and the now-dead `Rc`/`RefCell`
   aliases, and run the U2 exit audit: identity, weak-reference, finalizer, and
   coroutine stress tests under forced collection across mixed dynamic/typed
   calls, with no object owned by two independent collectors.

Steps 1–3 should each land with their own focused regression coverage
(mirroring how the string migration's `alloc_string_fresh` fix got a targeted
test), and the existing `lua55.rs`/`lua55_dynamic_runtime_*.rs`/
`sol_conformance.rs` suites should stay green throughout those steps — none of
them touch `LuaValue`'s definition, so there is no point before step 4 where
the crate doesn't compile and pass its existing tests. Step 4 itself has to
land as one change that compiles and passes those same suites at the end,
since Rust's enum definition can't be half-migrated across a commit boundary
the way step-2's additive plumbing can.
