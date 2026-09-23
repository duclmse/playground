// Memory inspection: read-only allocation/GC stats surfaced from
// `gc-arena`'s `Metrics` (reached via `vm::Lua::gc_metrics()`), plus a
// manual GC trigger. `gc-arena` only tracks byte totals - no live object
// count, no per-type (table/string/closure/userdata) breakdown, no
// allocation-over-time history - so that's the ceiling of what this module
// can report without much deeper VM instrumentation than this pass scopes.
// Previously the only way to see any of this was to type
// `collectgarbage("count")` into a watch/REPL expression.

use wasm_bindgen::prelude::*;

use super::{DebugSession, MemoryStats};

#[wasm_bindgen]
impl DebugSession {
    /// Snapshots the paused program's current allocation stats. Unlike
    /// every other `get_*` method in `session`, this needs no
    /// `self.lua.enter(...)` call - `gc_metrics()` needs no `'gc` context,
    /// it just reads counters `gc-arena` already maintains.
    pub fn get_memory_stats(&self) -> MemoryStats {
        let metrics = self.lua.gc_metrics();
        MemoryStats {
            total_allocation: metrics.total_allocation() as f64,
            gc_allocation: metrics.total_gc_allocation() as f64,
            external_allocation: metrics.total_external_allocation() as f64,
            allocation_debt: metrics.allocation_debt(),
        }
    }

    /// Forces a full garbage-collection cycle (`Lua::gc_collect`) so a user
    /// can observe its effect on `get_memory_stats()` on demand, rather than
    /// only ever seeing the incremental collector's own pacing.
    pub fn force_gc(&mut self) {
        self.lua.gc_collect();
    }
}
