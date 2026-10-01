//! Correctness regression coverage for U8's call-target inline cache
//! (`Instr::Call`/`TailCall`/`TForCall` resolving a `LuaValue::Closure`
//! callee through `Proto::call_cache` - see `lua_bytecode::BoundedCache`/
//! `CallCacheEntry` and `LuaRuntime::closure_parts_cached`). These exercise
//! the cache purely through observable Lua semantics (never inspecting Rust
//! internals): a bug here would mean the cache served a stale or
//! wrong-identity `(prototype, upvalues)` pair for some closure, which is
//! exactly the hazard a real bug would produce (e.g. aliasing one closure's
//! independent upvalue cell onto another's).

#[test]
fn call_cache_monomorphic_tight_loop_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // The same closure identity hits the same call site 1000 times - the
    // common case the cache exists for.
    let source = br#"
        local function add1(x) return x + 1 end
        local sum = 0
        for i = 1, 1000 do
            sum = add1(sum)
        end
        return sum == 1000
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn call_cache_tail_call_monomorphic_loop_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Same shape, but through `Instr::TailCall`'s own cache-check copy.
    let source = br#"
        local function count_down(n, acc)
            if n == 0 then return acc end
            return count_down(n - 1, acc + 1)
        end
        return count_down(2000, 0) == 2000
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn call_cache_polymorphic_call_site_keeps_each_closures_upvalue_independent() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Three distinct closures (independent upvalue cells) cycle through one
    // call site - exactly `IC_SLOTS`-worth of distinct identities, so every
    // one should stay resident in the cache (no eviction), and a broken
    // cache that aliased one closure's cached prototype/upvalues onto
    // another would corrupt one counter's count.
    let source = br#"
        local function counter()
            local n = 0
            return function() n = n + 1; return n end
        end
        local a, b, c = counter(), counter(), counter()
        local fns = { a, b, c }
        for i = 1, 30 do
            local f = fns[(i - 1) % 3 + 1]
            f()
        end
        return a() == 11 and b() == 11 and c() == 11
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn call_cache_megamorphic_call_site_evicts_but_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Ten distinct closures (more than `IC_SLOTS` == 4) cycle through one
    // call site, forcing the cache to round-robin-evict well past its
    // capacity. Every closure's own independent upvalue must still be
    // correct afterward - the cache degrading to "always miss" under
    // megamorphic pressure must never degrade into "sometimes wrong".
    let source = br#"
        local function counter()
            local n = 0
            return function() n = n + 1; return n end
        end
        local fns = {}
        for i = 1, 10 do
            fns[i] = counter()
        end
        for i = 1, 100 do
            local f = fns[(i - 1) % 10 + 1]
            f()
        end
        local ok = true
        for i = 1, 10 do
            if fns[i]() ~= 11 then
                ok = false
            end
        end
        return ok
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn call_cache_survives_debug_upvaluejoin_aliasing() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `debug.upvaluejoin` rebinds an upvalue *cell*, not a closure's
    // `prototype`/`environment` (fixed at construction) - so it must never
    // need special-case cache invalidation. Call `get_b` enough times first
    // to populate its call-site cache entry with its own (pre-join)
    // prototype/upvalues, then join, then confirm later calls still observe
    // the shared cell correctly (the prototype/upvalues the cache remembers
    // for `get_b` never needed to change - only the upvalue cell's own
    // content did, through the ordinary `Cell::set` aliasing mechanism).
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local function make_a()
                local x = 1
                return function() return x end, function(v) x = v end
            end
            local function make_b()
                local y = 100
                return function() return y end
            end
            local get_a, set_a = make_a()
            local get_b = make_b()
            -- Warm both closures' call-site caches before aliasing.
            for i = 1, 8 do
                get_a()
                get_b()
            end
            debug.upvaluejoin(get_b, 1, get_a, 1)
            set_a(42)
            local after_join = get_b()
            -- Keep calling through the same, now-cached call site.
            for i = 1, 8 do
                get_b()
            end
            return after_join == 42 and get_b() == 42
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn call_cache_shared_proto_across_two_coroutines_stays_correct() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `caller`'s single `Instr::Call` call site is the same shared
    // `Rc<Proto>`/`call_cache` entry for both coroutines (per-function
    // bytecode is interned once and shared across every fiber that resumes
    // through it). Interleaving resumes so each fiber's distinct closure
    // identity alternates through that one shared cache slot must never
    // corrupt either fiber's own independent counter.
    let source = br#"
        local function caller(f) return f() end
        local function counter()
            local n = 0
            return function() n = n + 1; return n end
        end
        local ca, cb = counter(), counter()
        local co1 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = caller(ca)
                coroutine.yield(total)
            end
            return total
        end)
        local co2 = coroutine.create(function()
            local total = 0
            for i = 1, 20 do
                total = caller(cb)
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
