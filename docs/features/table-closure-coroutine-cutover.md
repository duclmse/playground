# Tables, closures, and coroutines onto the canonical heap

Status: step 4 (the coordinated flip) implemented 2026-09-26; steps 1-4 done,
step 5 (safepoints/cleanup) is task #12. Written 2026-09-25.

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

### Values with no `sol_core::Value` representation today

`TableObject::array`/`hash` and `UpvalueObject::value` are typed
`sol_core::Value`, so once `LuaValue::Table` is `TableRef`-backed, **every**
value a Lua program stores in a table or captures in an upvalue must be
`sol_core::Value`-representable — not just `Table`/`Closure`/`Thread`
themselves. Four more `LuaValue` variants were checked against this
requirement by direct inspection:

- **`Native(Rc<NativeBridge>)` — not a gap.** `init.rs` always converts this to
  `RegisteredNative(NativeCallableId)` (already canonical, via
  `alloc_native_callable`) before the value is ever installed into a table or
  global; a raw `Native` value never reaches table/upvalue storage.
- **`NativeFunction` (`#[repr(u32)] enum`, ~120 builtin discriminants,
  installed into the globals table by `init.rs`) and `LightUserdata(usize)`
  (produced by `debug.upvalueid`, and independently by the C API's
  `lua_pushlightuserdata`/`lua_rawgetp`/`lua_rawsetp` wrapping an arbitrary
  host pointer with no backing heap object at all) and `GMatchIterator`
  (`Rc<RefCell<GMatchState>>`: `source`/`pattern` byte strings plus mutable
  `position`/`last_end`) all resolve the same way: as a `NativeCallableObject
  { provider, function, captures }`, using the `provider` field purely as a
  disambiguating namespace rather than a real bridge identity —
  `LuaRuntime` reserves one fixed provider id per kind
  (`Heap::reserve_native_provider`, the same call already used per imported
  native bridge) at construction:
  - `NativeFunction`: `function` holds the discriminant; `captures` is empty.
    Because `alloc_native_callable` mints a fresh `ObjectId` on every call (no
    dedup), a small caller-side registry (`HashMap<u32, ObjectId>`, populated
    lazily, one entry per discriminant ever referenced) is required so two
    references to the same builtin — e.g. `t[print] = 1; print(t[print])` —
    resolve to the same object. `NativeFunction` also needs a `from_u32`
    reverse mapping alongside its existing `.name()` method.
  - `LightUserdata`: `function` is unused (`0`); `captures` holds a single
    `Value::integer(bits as i64)` — the full 64-bit pointer value bit-cast
    into an `i64`, not truncated through `function: u32`. This is *not* the
    same as encoding a light userdata directly as `Value::integer`: two
    values with the same `ValueTag::Integer` payload would then be
    indistinguishable from a real Lua integer once read back out of a table
    cell, which is a real information-loss bug, not just a style choice. A
    distinct `provider` id keeps `Object`-tagged light userdata unambiguous
    against both real integers and real (`lua_newuserdata`) host userdata,
    which is also `Object`-tagged but through `HeapObject::Userdata`, a
    different heap-object kind entirely. Real Lua light userdata compares
    equal by pointer *value*, not by identity, so this must be memoized the
    same way as `NativeFunction` — a `HashMap<usize, ObjectId>` keyed by the
    raw pointer bits, so two light userdata values with equal bits always
    resolve to the same `ObjectId`.
  - `GMatchIterator`: `function` is unused; `captures` holds
    `[Value::object(source_string_id), Value::object(pattern_string_id),
    Value::integer(position as i64), Value::integer(last_end.map_or(-1, |v| v
    as i64))]` (source/pattern interned via `Heap::alloc_string_fresh`, like
    every other runtime-computed string). Unlike the other two, this is
    *never* memoized — each `string.gmatch(...)` call must produce a distinct,
    independently mutable iterator even over identical subject/pattern text,
    matching real Lua. Advancing the iterator calls
    `Heap::set_native_callable_captures` to rewrite `position`/`last_end` in
    place rather than allocating a new object per step.

  Decoding a `Value::Object(id)` back into a `LuaValue` therefore requires
  checking `provider` against the three reserved ids (and the dynamic native-
  bridge/`CFunction` providers already in use) before matching on
  `HeapObject::NativeCallable` vs `HeapObject::Userdata` vs the rest — the
  `provider` field is the only disambiguator once several unrelated `LuaValue`
  kinds share the same underlying `HeapObject` variant.

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
   nothing constructs them. *(Done — see §9 for bugs the flip found and fixed,
   and the residual gaps it carries into task #12.)*
5. **Safepoints and cleanup**: move collection triggering to the dispatch-loop
   safepoint (§3), delete `gc.rs`'s collector and the now-dead `Rc`/`RefCell`
   aliases, and run the U2 exit audit: identity, weak-reference, finalizer, and
   coroutine stress tests under forced collection across mixed dynamic/typed
   calls, with no object owned by two independent collectors. *(Task #12.)*

## 9. Implementation notes from the flip (task #11)

Landing step 4 surfaced four bugs not visible from the design in §2–§6, all
fixed as part of the same change, plus two residual gaps that are real but
out of this task's scope and are carried forward to task #12 rather than
papered over.

**Bugs found and fixed:**

- **Frame-popped-during-dispatch-loop rooting gap, three instances.** The
  interpreter's dispatch loop (`dispatch_step`) pops the currently-executing
  `Frame::Lua` out of `LuaRuntime::frames` into a Rust local for the duration
  of its instruction loop (so registers/cells/upvalues can be mutated without
  re-indexing `frames` every instruction). Any GC-triggering call reachable
  from inside that loop therefore cannot rely on walking `self.frames` alone
  for roots — it must also be told about that popped frame. This surfaced in
  three places before the frame-roots walk correctly accounted for all of
  them: table/closure allocation (`new_table`/`new_closure`) and general
  allocation charging (`charge_allocation`/`tick`) needed the popped frame
  threaded through as an explicit `active_frame: Option<&LuaFrame>` parameter
  from every `dispatch_step`-internal call site (`None` everywhere a
  collection can't be mid-dispatch); table-growth charging (see below) needed
  the same threading once it existed; and `__gc` finalizer invocation needed
  a *second*, independent mechanism (`LuaRuntime::pinned_roots: Vec<Vec<sol_core::Value>>`,
  pushed/popped around `collect_garbage_with`'s finalizer-invocation loop in
  `gc.rs`) because a finalizer's own `self.call` drives a *nested* dispatch
  loop with its own, different `active_frame` — the outer, still-in-flight
  popped frame that triggered the finalizing collection in the first place is
  invisible to that nested loop's own rooting unless pinned separately. A
  `Vec<Vec<_>>` rather than one flat `Vec` so a finalizer whose own execution
  triggers another collection with finalizers of its own unwinds cleanly.
- **Table-growth allocation charging was dropped, not reimplemented.** The
  deleted `Rc`-based collector charged `2 * size_of::<LuaValue>()` against the
  allocation budget for every genuinely new table key written (checked via
  "was the existing raw value at this key `Nil`?"), at exactly three call
  sites: `raw_set_index`, `set_index_resolve`'s no-metamethod fallback, and
  `Instr::SetArrayMulti`'s per-element loop. Nothing carried this forward
  across the cutover, leaving a loop like `for i = 1, huge do t[i] = i end`
  unmetered by anything but the instruction budget. Reimplemented as
  `LuaRuntime::charge_new_table_entry` in `table.rs`, called from the same
  three sites, with the same charge — deliberately *without* the old
  collector's credit-back-on-reclaim half, which is a different, still-open
  gap (see below).
- **A synthetic upvalue cell broke `debug.*` upvalue introspection.**
  `new_closure` appends a synthetic trailing environment cell to a closure's
  canonical upvalue-object vector (for `_ENV`) whenever the closure has an
  implicit environment. `debug.getupvalue`/`setupvalue`/`upvalueid`/
  `upvaluejoin` (`natives_debug.rs`) previously assumed the upvalue-cell count
  equaled `proto.upvals.len()`, which was true under the old `Rc` vector but
  is no longer true with the trailing synthetic cell present — an
  out-of-range Lua-visible upvalue index could resolve to that internal cell
  instead of correctly erroring. Fixed by bounding every such lookup to
  `proto.upvals.len()` and routing `_ENV` access only through the existing
  explicit `has_implicit_environment` branch.
- **A too-broad `.expect()` turned a legitimate `t[nil]`/`t[0/0]` read into a
  panic.** `TableRef::get` (`value.rs`) asserted every `Heap::table_get`
  error meant a dead/stale table handle. That was true before this flip
  (nothing else called `table_get` with a key that could legitimately fail),
  but the table-growth-charging fix above added new `table_get`-before-write
  calls (to check whether a key is new) that are reachable with a nil key on
  a write — surfacing `HeapError::NilTableKey`, a normal, expected outcome
  for a *read* (Lua returns `nil` for `t[nil]`/`t[nan]`), not a bug. Fixed by
  having `TableRef::get` return `Value::NIL` for `NilTableKey`/`NanTableKey`
  specifically, while still panicking on any other `HeapError` (a genuine
  dead-handle bug). The *write* path is unaffected: `table_set` already
  converts any `HeapError`, including a nil/NaN key, into a catchable
  `LuaError` — `t[nil] = v` still raises `"table index is nil"` as before.

**Residual gaps, out of scope for task #11, carried to task #12/#13:**

These are pre-existing, documented deferrals, not new problems introduced by
the flip; task #11's Lua-compatibility test suite deliberately leaves seven
tests red rather than force them green or quietly skip them:

- **No credit-back to the allocation budget after a collection.**
  `LuaRuntime::allocation_remaining` only ever decreases; nothing restores
  bytes a collection just reclaimed. This is the collector-triggering/exit
  work §8 scopes to task #12, not this flip. It was already visible as six
  failing tests in `lua55_dynamic_runtime_gc.rs` (reference-cycle and
  `gc_stress` reclamation tests that assert on a tight budget) before this
  task's work began. Correctly reimplementing table-growth charging (above)
  newly exposed the same gap in a seventh test,
  `lua55_fuzz.rs::dynamic_lua_random_shaped_table_cycles_are_reclaimed_under_a_tight_budget`,
  which explicitly relies on `collectgarbage()` crediting reclaimed cyclic
  garbage back to the budget across 20 iterations. It only passed before this
  task's changes by accident — table growth wasn't charged at all, so the
  test's tight budget was never actually exercised. All seven are the same
  underlying gap and are left failing, deferred to task #12, rather than
  masked by loosening the newly-correct charging above.
- **Every registered coroutine is an unconditional GC root forever** (§6,
  decided as the interim fallback). A coroutine referenced only through a
  weak-value table never becomes collectible. Deferred to task #13's
  conditional/deferred-root `sol-core` hook.

Steps 1–3 should each land with their own focused regression coverage
(mirroring how the string migration's `alloc_string_fresh` fix got a targeted
test), and the existing `lua55.rs`/`lua55_dynamic_runtime_*.rs`/
`sol_conformance.rs` suites should stay green throughout those steps — none of
them touch `LuaValue`'s definition, so there is no point before step 4 where
the crate doesn't compile and pass its existing tests. Step 4 itself has to
land as one change that compiles and passes those same suites at the end,
since Rust's enum definition can't be half-migrated across a commit boundary
the way step-2's additive plumbing can.
