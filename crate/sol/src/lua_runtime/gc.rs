//! Garbage collection: opt-in GC-stress mode, allocation-budget charging
//! (`charge_allocation`, `tick`), weak-table sweeping, and the
//! trial-deletion cycle collector (`collect_cycles`) layered on top of `Rc`
//! refcounting. See `LuaRuntime`'s `gc_tables`/`gc_closures`/`weak_tables`
//! field docs for the design rationale.

use std::rc::Rc;

use super::*;

/// How many dispatched instructions (`tick`) trigger one automatic (non-
/// `gc_stress`) collection pass - see `LuaRuntime::instructions_since_gc`'s
/// field doc. Chosen small enough that a script relying on real Lua's
/// automatic collector to clear a weak reference (e.g.
/// `lua-5.5.1-tests/closure.lua`'s `while weak_table[k] do ... end`) still
/// finishes comfortably inside the default 1,000,000-instruction budget, but
/// large enough that `collect_cycles`'s O(tracked tables/closures) cost
/// stays a small fraction of total run time for ordinary scripts.
const AUTO_GC_INSTRUCTION_INTERVAL: u64 = 65_536;

impl LuaRuntime {
    /// Enables/disables GC stress mode (see the `gc_stress` field doc) for
    /// this runtime. Off by default for every constructor; opt in
    /// explicitly (tests) or via `SOL_LUA_GC_STRESS=1` (the `sol` CLI, see
    /// `main.rs`).
    pub fn set_gc_stress(&mut self, enabled: bool) {
        self.gc_stress = enabled;
    }

    /// Runs the same work `collectgarbage("collect")` does (weak-table
    /// pruning, then trial-deletion cycle collection) - shared by the
    /// explicit `collectgarbage` native and, when `gc_stress` is enabled, by
    /// every allocation point and every dispatched instruction.
    fn stress_collect_if_enabled(&mut self) {
        if self.gc_stress {
            self.sweep_weak_tables();
            self.collect_cycles();
        }
    }

    pub(super) fn tick(&mut self) -> LuaResult<()> {
        if self.instructions_remaining == 0 {
            return Err(LuaError::new("Lua instruction budget exhausted"));
        }
        self.instructions_remaining -= 1;
        self.stress_collect_if_enabled();
        self.maybe_auto_collect();
        Ok(())
    }

    /// The production (non-`gc_stress`) default's automatic collection
    /// trigger - see `instructions_since_gc`'s field doc and
    /// `AUTO_GC_INSTRUCTION_INTERVAL` for the rationale and pacing. A no-op
    /// under `gc_stress`, which already collects unconditionally on every
    /// instruction via `stress_collect_if_enabled`.
    fn maybe_auto_collect(&mut self) {
        if self.gc_stress {
            return;
        }
        self.instructions_since_gc += 1;
        if self.instructions_since_gc < AUTO_GC_INSTRUCTION_INTERVAL {
            return;
        }
        self.instructions_since_gc = 0;
        self.sweep_weak_tables();
        self.collect_cycles();
    }

    pub(super) fn charge_allocation(&mut self, bytes: usize) -> LuaResult<()> {
        self.stress_collect_if_enabled();
        if bytes > self.allocation_remaining {
            return Err(LuaError::new("Lua allocation budget exhausted"));
        }
        self.allocation_remaining -= bytes;
        Ok(())
    }

    /// Approximate the currently live, runtime-managed heap for Lua's
    /// `collectgarbage("count")`. Allocation-budget consumption is
    /// intentionally not used here: that counter is cumulative and would
    /// report temporary frames/tables forever after Rust's `Rc` has already
    /// reclaimed them.
    pub(super) fn live_heap_bytes(&self) -> usize {
        let table_bytes = self
            .gc_tables
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|table| {
                let table = table.borrow();
                std::mem::size_of::<LuaTable>()
                    + table.array.capacity() * std::mem::size_of::<LuaValue>()
                    + table.hash.capacity() * (std::mem::size_of::<LuaValue>() * 2)
            })
            .sum::<usize>();
        let closure_bytes = self
            .gc_closures
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|closure| {
                std::mem::size_of::<LuaClosure>()
                    + closure.upvals.borrow().capacity() * std::mem::size_of::<RcRef<LuaValue>>()
            })
            .sum::<usize>();
        table_bytes + closure_bytes
    }

    /// `collectgarbage("collect"/"step")`'s actual work: prune weakly-held
    /// entries out of every table registered as weak-mode by `setmetatable`,
    /// then run the cycle collector (`collect_cycles`) over every tracked
    /// table/closure.
    pub(super) fn sweep_weak_tables(&mut self) {
        self.weak_tables.retain(|weak| weak.strong_count() > 0);
        for weak in &self.weak_tables {
            let Some(table_rc) = weak.upgrade() else {
                continue;
            };
            let (weak_keys, weak_values) = match &table_rc.borrow().metatable {
                Some(meta) => table_weak_mode(meta),
                None => (false, false),
            };
            if weak_keys || weak_values {
                prune_weak_table(&mut table_rc.borrow_mut(), weak_keys, weak_values);
            }
        }
    }

    /// Registers an ordinary table as a cycle-collector candidate. Must not be
    /// called for the permanent library/bootstrap tables built by
    /// `install_base` (`string`/`math`/`table`/`utf8`/`package`/`os`/`io`) or
    /// `package_loaded` - those are always reachable via `globals`, which this
    /// collector treats as an opaque, untracked root, so tracking them would
    /// only add churn with no correctness benefit.
    pub(super) fn track_table(&self, table: RcRef<LuaTable>) -> RcRef<LuaTable> {
        self.gc_tables.borrow_mut().push(Rc::downgrade(&table));
        table
    }

    /// Registers an ordinary closure as a cycle-collector candidate. See
    /// `track_table` for the same caveat about permanent roots (closures
    /// created for bootstrap library entries don't exist, so this has no
    /// analogous exclusion today).
    pub(super) fn track_closure(&self, closure: Rc<LuaClosure>) -> Rc<LuaClosure> {
        self.gc_closures.borrow_mut().push(Rc::downgrade(&closure));
        closure
    }

    /// A CPython-style trial-deletion cycle collector layered on top of the
    /// ordinary `Rc<RefCell<...>>` value graph. Never needs to enumerate
    /// program roots (globals, live VM registers, upvalue cells outside a
    /// tracked object) directly: any strong reference reaching a candidate
    /// from outside the tracked set shows up automatically as a positive
    /// residual once every *inter-candidate* edge has been subtracted from
    /// each candidate's real `Rc::strong_count`.
    ///
    /// Safety note: under-counting an edge in the children-enumeration below
    /// only makes this more conservative (something stays alive that could
    /// have been collected) - never unsound. Over-counting (attributing the
    /// same stored `Rc` slot to more than one edge) would be unsound: it
    /// could drive a genuinely-reachable candidate's residual down to zero
    /// and cause it to be swept while still referenced. Every enumeration
    /// function below must therefore visit each stored `Rc` slot exactly
    /// once.
    pub(super) fn collect_cycles(&mut self) {
        let tables: Vec<RcRef<LuaTable>> = self
            .gc_tables
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let closures: Vec<Rc<LuaClosure>> = self
            .gc_closures
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();

        // `- 1` discounts the strong reference `tables`/`closures` themselves
        // just took when upgrading each `Weak` above - without it, every
        // candidate's count would be inflated by exactly one purely from
        // being snapshotted, masking real cycles (e.g. a lone self-referencing
        // table would show `strong_count == 2`, not `1`, and never reach zero
        // after subtracting its own self-edge).
        let mut refs: HashMap<usize, i64> = HashMap::new();
        for table in &tables {
            refs.insert(
                Rc::as_ptr(table) as usize,
                Rc::strong_count(table) as i64 - 1,
            );
        }
        for closure in &closures {
            refs.insert(
                Rc::as_ptr(closure) as usize,
                Rc::strong_count(closure) as i64 - 1,
            );
        }

        let table_children = |table: &RcRef<LuaTable>| -> Vec<usize> {
            let mut children = Vec::new();
            let borrowed = table.borrow();
            for value in &borrowed.array {
                if let Some(ptr) = candidate_ptr(value) {
                    children.push(ptr);
                }
            }
            for (key, value) in borrowed.hash.iter() {
                if let Some(ptr) = candidate_key_ptr(key) {
                    children.push(ptr);
                }
                if let Some(ptr) = candidate_ptr(value) {
                    children.push(ptr);
                }
            }
            if let Some(meta) = &borrowed.metatable {
                children.push(Rc::as_ptr(meta) as usize);
            }
            children
        };
        let closure_children = |closure: &Rc<LuaClosure>| -> Vec<usize> {
            closure
                .upvals
                .borrow()
                .iter()
                .filter_map(|cell| candidate_ptr(&cell.borrow()))
                .collect()
        };

        for table in &tables {
            for child in table_children(table) {
                if let Some(count) = refs.get_mut(&child) {
                    *count -= 1;
                }
            }
        }
        for closure in &closures {
            for child in closure_children(closure) {
                if let Some(count) = refs.get_mut(&child) {
                    *count -= 1;
                }
            }
        }

        let mut reachable: HashSet<usize> = HashSet::new();
        let mut worklist: Vec<usize> = refs
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(ptr, _)| *ptr)
            .collect();
        reachable.extend(worklist.iter().copied());

        let table_by_ptr: HashMap<usize, &RcRef<LuaTable>> = tables
            .iter()
            .map(|table| (Rc::as_ptr(table) as usize, table))
            .collect();
        let closure_by_ptr: HashMap<usize, &Rc<LuaClosure>> = closures
            .iter()
            .map(|closure| (Rc::as_ptr(closure) as usize, closure))
            .collect();

        while let Some(ptr) = worklist.pop() {
            let children = if let Some(table) = table_by_ptr.get(&ptr) {
                table_children(table)
            } else if let Some(closure) = closure_by_ptr.get(&ptr) {
                closure_children(closure)
            } else {
                Vec::new()
            };
            for child in children {
                if reachable.insert(child) {
                    worklist.push(child);
                }
            }
        }

        // Run `__gc` finalizers for newly-unreachable tables before clearing
        // anything, so the finalizer sees the table's fields intact - matches
        // real Lua's "finalizer runs while the object is still whole" timing.
        // Unlike real Lua, a table referenced from inside its own `__gc` call
        // is not resurrected: this collector always proceeds to clear it
        // afterward (see Phase 4b in `docs/features/lua-superset-plan.md`),
        // so a finalizer must not assume the table will keep working past the
        // call. A finalizer error is discarded rather than propagated -
        // `collectgarbage()` itself must not fail because of a broken `__gc`.
        let mut finalizers: Vec<(RcRef<LuaTable>, LuaValue)> = Vec::new();
        for table in &tables {
            let ptr = Rc::as_ptr(table) as usize;
            if reachable.contains(&ptr) || table.borrow().finalized {
                continue;
            }
            if let Some(finalizer) = table_finalizer(table) {
                finalizers.push((table.clone(), finalizer));
            }
        }
        for (table, finalizer) in finalizers {
            table.borrow_mut().finalized = true;
            let _ = self.call(finalizer, vec![LuaValue::Table(table)]);
        }

        let mut reclaimed: usize = 0;
        for table in &tables {
            let ptr = Rc::as_ptr(table) as usize;
            if !reachable.contains(&ptr) {
                let mut borrowed = table.borrow_mut();
                borrowed.array.clear();
                borrowed.array_border = 0;
                borrowed.hash.clear();
                borrowed.metatable = None;
                borrowed.version = borrowed.version.wrapping_add(1);
                reclaimed += std::mem::size_of::<LuaTable>();
            }
        }
        for closure in &closures {
            let ptr = Rc::as_ptr(closure) as usize;
            if !reachable.contains(&ptr) {
                closure.upvals.borrow_mut().clear();
                reclaimed += std::mem::size_of::<LuaClosure>();
            }
        }
        // Credit reclaimed memory back to the allocation budget - otherwise a
        // script that allocates cyclic garbage in a loop would exhaust its
        // budget even though `collectgarbage()` is actually reclaiming it.
        self.allocation_remaining =
            (self.allocation_remaining + reclaimed).min(self.allocation_budget);

        self.gc_tables
            .borrow_mut()
            .retain(|weak| weak.strong_count() > 0);
        self.gc_closures
            .borrow_mut()
            .retain(|weak| weak.strong_count() > 0);
    }
}
