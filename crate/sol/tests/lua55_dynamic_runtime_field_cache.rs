//! Correctness regression coverage for U8's field-access inline cache
//! (`Instr::GetField`/`SetField` resolving a plain `LuaValue::Table`'s own
//! raw hash-part slot through `Proto::field_cache` - see
//! `lua_bytecode::FieldCacheEntry` and
//! `LuaRuntime::field_cache_get`/`field_cache_set`/`field_cache_populate`).
//! These exercise the cache purely through observable Lua semantics (never
//! inspecting Rust internals): a bug here would mean the cache served a
//! stale value, aliased one table's field onto another, or skipped a
//! `__index`/`__newindex` metamethod fallback it should have taken.

#[test]
fn field_cache_monomorphic_repeated_field_access_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // The same table identity hits the same `GetField`/`SetField` call site
    // 1000 times - the common case the cache exists for.
    let source = br#"
        local t = { x = 0 }
        for i = 1, 1000 do
            t.x = t.x + 1
        end
        return t.x == 1000
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_polymorphic_call_site_keeps_each_tables_field_independent() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Three distinct tables cycle through one `GetField`/`SetField` call
    // site - a broken cache that aliased one table's cached hash index onto
    // another would corrupt a field read/write meant for a different table.
    let source = br#"
        local a, b, c = { x = 1 }, { x = 100 }, { x = 10000 }
        local ts = { a, b, c }
        for i = 1, 30 do
            local t = ts[(i - 1) % 3 + 1]
            t.x = t.x + 1
        end
        return a.x == 11 and b.x == 110 and c.x == 10010
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_megamorphic_call_site_evicts_but_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Ten distinct tables (more than `IC_SLOTS` == 4) cycle through one call
    // site, forcing the cache to round-robin-evict well past its capacity.
    // Every table's own field must still be correct afterward - degrading to
    // "always miss" under megamorphic pressure must never degrade into
    // "sometimes wrong".
    let source = br#"
        local ts = {}
        for i = 1, 10 do
            ts[i] = { x = i * 100 }
        end
        for i = 1, 100 do
            local t = ts[(i - 1) % 10 + 1]
            t.x = t.x + 1
        end
        local ok = true
        for i = 1, 10 do
            if ts[i].x ~= i * 100 + 10 then
                ok = false
            end
        end
        return ok
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_alias_write_through_one_reference_is_visible_through_the_cached_read() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `t` and `alias` are two Lua references to the same table object. A
    // read call site for `alias.x` gets warmed first (caching `alias`'s own
    // table identity's hash index), then a write through the *other*
    // reference (`t.x = ...`) must still be observed by the next read
    // through `alias` - this is the literal alias-invalidation deliverable:
    // the cache is keyed on the table's `ObjectId`, which is the same for
    // both references, so a write through either name is visible to a
    // cached read through the other.
    let source = br#"
        local t = { x = 1 }
        local alias = t
        local seen = 0
        for i = 1, 5 do
            seen = alias.x
        end
        t.x = 42
        for i = 1, 5 do
            seen = alias.x
        end
        return seen == 42
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_does_not_bypass_index_metamethod_after_field_is_set_nil() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Warm the cache on a genuinely-present field, then nil it out (Lua
    // tables use tombstone-not-remove semantics internally, so the key may
    // still occupy the same hash-part slot) and install a metatable
    // `__index` fallback. A subsequent read must still observe the fallback
    // value, not a stale cached non-nil value or an incorrectly-cached nil.
    let source = br#"
        local t = { x = 1 }
        local sum = 0
        for i = 1, 5 do
            sum = sum + t.x
        end
        t.x = nil
        setmetatable(t, { __index = function(_, k) if k == "x" then return 99 end end })
        local after = t.x
        for i = 1, 5 do
            after = t.x
        end
        return after == 99
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_set_on_new_key_still_charges_and_does_not_skip_newindex() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A brand-new key (no raw slot yet) must still go through the slow path
    // the first time (and so still honor `__newindex`) rather than ever
    // treating a cache miss as "safe to speculatively create." Real Lua
    // semantics (confirmed against the reference `lua` 5.5 interpreter):
    // `__newindex` fires only while the raw key is still absent, so once
    // the handler's own `rawset` creates it, every later assignment
    // (i = 2, 3) writes directly and bypasses `__newindex` - exactly the
    // raw-exists fast path the field cache should also be learning and
    // using from i = 2 onward. `#log == 1` is the correct Lua answer here,
    // not a quirk of the cache.
    let source = br#"
        local log = {}
        local t = setmetatable({}, {
            __newindex = function(table, k, v)
                log[#log + 1] = k
                rawset(table, k, v)
            end,
        })
        for i = 1, 3 do
            t.y = i
        end
        return t.y == 3 and #log == 1
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn field_cache_survives_two_coroutines_sharing_one_call_site() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `bump`'s single `GetField`/`SetField` call site is the same shared
    // `Rc<Proto>`/`field_cache` entry for both coroutines. Interleaving
    // resumes so each fiber's distinct table identity alternates through
    // that one shared cache slot must never corrupt either fiber's own
    // independent counter.
    let source = br#"
        local function bump(t)
            t.n = t.n + 1
            return t.n
        end
        local ta, tb = { n = 0 }, { n = 0 }
        local co1 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = bump(ta)
                coroutine.yield(total)
            end
            return total
        end)
        local co2 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = bump(tb)
                coroutine.yield(total)
            end
            return total
        end)
        for i = 1, 20 do
            coroutine.resume(co1)
            coroutine.resume(co2)
        end
        local _, final1 = coroutine.resume(co1)
        local _, final2 = coroutine.resume(co2)
        return final1 == 20 and final2 == 20
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
