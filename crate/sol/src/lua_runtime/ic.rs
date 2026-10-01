//! U8's inline-cache stats counters: cumulative hits/misses/evictions per
//! cache kind (call-target, field-access, global-access), aggregated across a
//! `LuaRuntime`'s whole lifetime - the same "plain counters struct, exposed
//! read-only via a `debug.*stats()` native" shape as `gc::GcStats`, see that
//! module's own doc comment for the precedent this mirrors.
//!
//! Stored as `LuaRuntime::ic_stats: Cell<IcStats>` (not `RefCell`, since every
//! cache-consulting method - `field_cache_get`/`field_cache_set`/
//! `field_probe_raw`/`field_write_raw`/`closure_parts_cached` - takes `&self`,
//! not `&mut self`, matching the rest of `table.rs`'s interior-mutability
//! style): `Cell::get`/`set` round-trips the whole small `Copy` struct per
//! update rather than requiring a borrow.

/// Which U8 inline cache an access went through. `Field` and `Global` share
/// the same underlying mechanism (`field_cache_get`/`field_cache_set`/
/// `field_probe_raw`/`field_write_raw` against a `FieldCacheEntry` cache -
/// see `table.rs`), so those shared methods take this as a parameter purely
/// to bucket stats into the right counter; `Call` never shares its mechanism
/// with another kind, so `closure_parts_cached` bumps its counters directly
/// without needing this type at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IcKind {
    Call,
    Field,
    Global,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct IcKindStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Exposed read-only to Lua through `debug.icstats()` (`natives_debug.rs`).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct IcStats {
    pub call: IcKindStats,
    pub field: IcKindStats,
    pub global: IcKindStats,
}

impl IcStats {
    fn kind_mut(&mut self, kind: IcKind) -> &mut IcKindStats {
        match kind {
            IcKind::Call => &mut self.call,
            IcKind::Field => &mut self.field,
            IcKind::Global => &mut self.global,
        }
    }
}

impl super::LuaRuntime {
    pub(super) fn ic_record_hit(&self, kind: IcKind) {
        let mut stats = self.ic_stats.get();
        stats.kind_mut(kind).hits += 1;
        self.ic_stats.set(stats);
    }

    pub(super) fn ic_record_miss(&self, kind: IcKind) {
        let mut stats = self.ic_stats.get();
        stats.kind_mut(kind).misses += 1;
        self.ic_stats.set(stats);
    }

    pub(super) fn ic_record_eviction(&self, kind: IcKind) {
        let mut stats = self.ic_stats.get();
        stats.kind_mut(kind).evictions += 1;
        self.ic_stats.set(stats);
    }
}
