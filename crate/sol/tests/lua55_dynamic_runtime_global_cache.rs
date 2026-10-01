//! Correctness regression coverage for U8's global-access inline cache
//! (`Instr::GetGlobal`/`SetGlobal` resolving the plain-`_ENV` path's own raw
//! hash-part slot through `Proto::global_cache` - see
//! `lua_bytecode::FieldCacheEntry` and
//! `LuaRuntime::field_cache_get`/`field_cache_set`/`field_probe_raw`/
//! `field_write_raw`, reused verbatim against the table extracted from
//! `Globals::as_value()`). These exercise the cache purely through
//! observable Lua semantics (never inspecting Rust internals): a bug here
//! would mean the cache served a stale global, aliased one `_ENV`'s global
//! onto another, or skipped a `__index`/`__newindex` metamethod fallback it
//! should have taken. The literal milestone deliverable - a bare
//! `_ENV = newtable` or `load(..., env)` reassignment must never serve a
//! stale cached value - is covered explicitly below.

#[test]
fn global_cache_monomorphic_repeated_global_access_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // The same `_ENV` hits the same `GetGlobal`/`SetGlobal` call site 1000
    // times - the common case the cache exists for.
    let source = br#"
        x = 0
        for i = 1, 1000 do
            x = x + 1
        end
        return x == 1000
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_env_reassignment_mid_execution_is_not_served_stale() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Warm the global-access cache against the original `_ENV`, then
    // reassign `_ENV` to a brand-new table partway through and confirm the
    // next reads/writes through the very same call site observe the new
    // environment, not a stale cached slot from the old one. This is the
    // literal `_ENV`-invalidation deliverable from the milestone text.
    let source = br#"
        local function probe()
            local seen = 0
            for i = 1, 5 do
                y = i
                seen = y
            end
            return seen
        end
        local first = probe()
        _ENV = { print = print }
        local second = probe()
        return first == 5 and second == 5 and y == 5
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_load_with_custom_env_keeps_each_environments_global_independent() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Two chunks loaded with two distinct custom `_ENV` tables (via
    // `load(chunk, name, mode, env)`) share no call-site cache entries with
    // each other at the Rust level (each is its own compiled `Proto`), but
    // this still confirms the global cache never aliases one environment's
    // global onto the other's when both are driven through repeated calls.
    let source = br#"
        local envA = { z = 100 }
        local envB = { z = 200 }
        local fA = load("for i = 1, 10 do z = z + 1 end return z", "chunkA", "t", envA)
        local fB = load("for i = 1, 10 do z = z + 1 end return z", "chunkB", "t", envB)
        local a = fA()
        local b = fB()
        return a == 110 and b == 210 and envA.z == 110 and envB.z == 210
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_does_not_bypass_index_metamethod_after_global_is_set_nil() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Warm the cache on a genuinely-present global, then nil it out
    // (tombstone-not-remove) and install an `__index` fallback on `_ENV`'s
    // metatable. A subsequent read must still observe the fallback value,
    // not a stale cached non-nil value or an incorrectly-cached nil.
    let source = br#"
        w = 1
        local sum = 0
        for i = 1, 5 do
            sum = sum + w
        end
        w = nil
        setmetatable(_ENV, { __index = function(_, k) if k == "w" then return 99 end end })
        local after = w
        for i = 1, 5 do
            after = w
        end
        return after == 99
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_set_on_new_global_still_charges_and_does_not_skip_newindex() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A brand-new global (no raw slot yet) must still go through the slow
    // path the first time (and so still honor `__newindex` on `_ENV`'s
    // metatable) rather than ever treating a cache miss as "safe to
    // speculatively create." Matches the field-cache precedent for new-key
    // writes exactly, just against `_ENV` instead of an arbitrary table.
    let source = br#"
        local log = {}
        setmetatable(_ENV, {
            __newindex = function(table, k, v)
                log[#log + 1] = k
                rawset(table, k, v)
            end,
        })
        for i = 1, 3 do
            newglobal = i
        end
        return newglobal == 3 and #log == 1
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_megamorphic_call_site_evicts_but_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `_ENV` is just a lexically-scoped local/upvalue in Lua 5.2+ - so
    // `local _ENV = env` inside `probe` redirects every global access in its
    // body to `env` for that call, and `probe`'s single `GetGlobal`/
    // `SetGlobal` call site is shared (same `pc`, same `Proto::global_cache`
    // entry) across every call regardless of which table is passed. Cycling
    // ten distinct environments (more than `IC_SLOTS` == 4) through that one
    // shared call site forces the cache to round-robin-evict well past its
    // capacity; every environment's own global count must still come out
    // correct afterward - "always miss" under megamorphic pressure must
    // never degrade into "sometimes wrong".
    let source = br#"
        local function probe(env)
            local _ENV = env
            g = (g or 0) + 1
            return g
        end
        local envs = {}
        for i = 1, 10 do
            envs[i] = {}
        end
        for i = 1, 100 do
            probe(envs[(i - 1) % 10 + 1])
        end
        local ok = true
        for i = 1, 10 do
            if envs[i].g ~= 10 then
                ok = false
            end
        end
        return ok
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn global_cache_survives_two_coroutines_sharing_one_call_site() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `bump`'s single `GetGlobal`/`SetGlobal` call site is the same shared
    // `Rc<Proto>`/`global_cache` entry for both coroutines, both running
    // under the same top-level `_ENV`. Interleaving resumes so each fiber's
    // writes alternate through that one shared cache slot must never
    // corrupt the shared global counter either fiber observes.
    let source = br#"
        counter = 0
        local function bump()
            counter = counter + 1
            return counter
        end
        local co1 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = bump()
                coroutine.yield(total)
            end
            return total
        end)
        local co2 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = bump()
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
        -- co1 resumes first in each interleaved step, so its own 20th
        -- `bump()` call observes `counter` one increment behind co2's: the
        -- last "live" value co1's `total` variable captured is 39 (co2's is
        -- 40), even though both coroutines' final resume (which just drains
        -- their for loop to completion without calling `bump()` again)
        -- leaves the shared global at 40.
        return final1 == 39 and final2 == 40 and counter == 40
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
