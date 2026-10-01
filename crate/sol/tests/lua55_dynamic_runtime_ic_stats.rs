//! Correctness coverage for U8's `debug.icstats()` - the cumulative
//! hit/miss/eviction counters for the call-target (item 2), field-access
//! (item 3), and global-access (item 4) inline caches, see
//! `lua_runtime::ic::IcStats`'s own doc comment for why it mirrors
//! `gc::GcStats`/`debug.gcstats()` exactly. These exercise the counters
//! purely through observable Lua semantics, matching
//! `lua55_dynamic_runtime_gc.rs`'s existing `debug.gcstats()` tests.
//!
//! Also covers `debug.icprofile()` - the per-call-site mono/poly/megamorphic
//! occupancy text dump, see `lua_bytecode::Proto::ic_profile_dump`'s own doc
//! comment for its scope (occupancy only, no per-call-site hit counts).

use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

fn parse(source: &[u8]) -> sol::ast::Program {
    sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap()
}

fn run_with_debug(source: &[u8]) -> LuaValue {
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    runtime.run(&parse(source)).unwrap()
}

#[test]
fn dynamic_lua_runtime_debug_icstats_counts_call_cache_hits_and_misses() {
    // `probe` is the same closure object at every one of the 50 calls
    // through this one `Call` call site - the first call is necessarily a
    // cache miss (nothing cached yet), every later call must be a hit.
    let source = br#"
        local function probe() return 1 end
        for i = 1, 50 do
            probe()
        end
        local stats = debug.icstats()
        return stats.call_hits == 49.0 and stats.call_misses == 1.0
    "#;
    assert_eq!(run_with_debug(source), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icstats_counts_field_cache_hits_and_misses() {
    // One `GetField` call site repeatedly reading the same table's own
    // field: first read is a miss (populates the cache), the rest hit.
    let source = br#"
        local t = { x = 1 }
        local sum = 0
        for i = 1, 50 do
            sum = sum + t.x
        end
        local stats = debug.icstats()
        return stats.field_hits == 49.0 and stats.field_misses >= 1.0
    "#;
    assert_eq!(run_with_debug(source), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icstats_counts_global_cache_hits_and_misses() {
    // `x = x + 1` reads and writes `x` through two distinct `GetGlobal`/
    // `SetGlobal` call sites (each its own `pc`, each its own cache), both
    // bucketed into the same `IcKind::Global` counter: the outer `x = 0`
    // (its own, once-only `SetGlobal` site), the loop's own `GetGlobal`,
    // and the loop's own `SetGlobal` each take exactly one miss on their
    // first access, then hit every time after - plus one more miss for the
    // one-off `debug` global read this very statement performs. Every other
    // one of the loop's 49 remaining iterations hits both the loop's
    // `GetGlobal` and `SetGlobal` sites, for 49 * 2 = 98 hits.
    let source = br#"
        x = 0
        for i = 1, 50 do
            x = x + 1
        end
        local stats = debug.icstats()
        return stats.global_hits == 98.0 and stats.global_misses == 4.0
    "#;
    assert_eq!(run_with_debug(source), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icstats_counts_evictions_under_megamorphic_pressure() {
    use sol::lua_bytecode::IC_SLOTS;

    // Cycling more than `IC_SLOTS` distinct tables through one shared
    // `GetField` call site forces repeated round-robin eviction - the same
    // megamorphic scenario the field-cache correctness tests already cover,
    // reused here to confirm `field_probe_raw`'s `cache.insert` eviction
    // report actually reaches `debug.icstats()`.
    let source = format!(
        r#"
        local function probe(t)
            return t.x
        end
        local tables = {{}}
        for i = 1, {count} do
            tables[i] = {{ x = i }}
        end
        for i = 1, 200 do
            probe(tables[(i - 1) % {count} + 1])
        end
        local stats = debug.icstats()
        return stats.field_evictions > 0.0
    "#,
        count = IC_SLOTS + 6
    );
    assert_eq!(run_with_debug(source.as_bytes()), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icprofile_classifies_mono_and_poly_call_sites() {
    // `probe`'s own field-access call site sees 3 distinct table identities
    // cycle through it (poly), while the 3 literal `probe(...)` call sites
    // in the loop body each always call the same `probe` closure (mono) -
    // `debug.icprofile()`'s text dump must report both states correctly,
    // and must recurse into the nested `probe` prototype to report its
    // call site at all.
    let source = br#"
        local function probe(t)
            return t.x
        end
        local t1, t2, t3 = { x = 1 }, { x = 2 }, { x = 3 }
        for i = 1, 20 do
            probe(t1)
            probe(t2)
            probe(t3)
        end
        local profile = debug.icprofile()
        return profile:find("kind=field state=poly(3/4)", 1, true) ~= nil
            and profile:find("kind=call state=mono(1/4)", 1, true) ~= nil
            and profile:find("proto 'probe'", 1, true) ~= nil
    "#;
    assert_eq!(run_with_debug(source), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icprofile_classifies_megamorphic_call_sites() {
    use sol::lua_bytecode::IC_SLOTS;

    // Reuses the same megamorphic-pressure scenario as the eviction-counter
    // test above, but checks the per-call-site text dump reports the
    // bounded-at-`IC_SLOTS` "megamorphic" state rather than growing past it.
    let source = format!(
        r#"
        local function probe(t)
            return t.x
        end
        local tables = {{}}
        for i = 1, {count} do
            tables[i] = {{ x = i }}
        end
        for i = 1, 200 do
            probe(tables[(i - 1) % {count} + 1])
        end
        local profile = debug.icprofile()
        return profile:find("kind=field state=megamorphic({slots}/{slots})", 1, true) ~= nil
    "#,
        count = IC_SLOTS + 6,
        slots = IC_SLOTS,
    );
    assert_eq!(run_with_debug(source.as_bytes()), LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_icprofile_requires_debug_capability() {
    use sol::lua_runtime::{run_source, LuaError};

    // Same capability gate as `debug.icstats()`/every other `debug.*`
    // native - see that test's own comment above.
    let source = br#"return debug.icprofile()"#;
    let error: LuaError = run_source(source).unwrap_err();
    assert!(error.to_string().contains("debug capability"));
}

#[test]
fn dynamic_lua_runtime_debug_icstats_requires_debug_capability() {
    use sol::lua_runtime::{run_source, LuaError};

    // `debug.icstats` is registered under the same `debug` capability gate
    // as every other `debug.*` native (see `natives.rs`'s capability-gating
    // match arm) - `run_source`'s default `Capabilities::SANDBOX` has
    // `debug: false`, so this must fail the same way `debug.gcstats()`
    // would, not silently succeed or panic.
    let source = br#"return debug.icstats()"#;
    let error: LuaError = run_source(source).unwrap_err();
    assert!(error.to_string().contains("debug capability"));
}
