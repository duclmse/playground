//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: garbage collection (weak tables, cycles, finalizers, stress mode)).

#[test]
fn dynamic_lua_runtime_weak_value_tables_drop_entries_once_unreachable() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A `__mode = "v"` table must not keep its values alive: once nothing
    // else references a stored table, `collectgarbage()` should prune the
    // entry to nil. A value still referenced elsewhere (`strong_ref`) must
    // survive the same sweep.
    let source = br#"
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        local strong_ref
        local function stash_unreachable()
            local obj = {}
            cache[1] = obj
        end
        local function stash_reachable()
            local obj = {}
            cache[2] = obj
            strong_ref = obj
        end
        stash_unreachable()
        stash_reachable()
        collectgarbage()
        return cache[1] == nil and cache[2] ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_weak_value_tables_do_not_retain_suspended_coroutines() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A suspended coroutine has an internal continuation in addition to the
    // reference held by the weak-table slot.  That bookkeeping must not make
    // the slot look like a live Lua reference after its last user-visible
    // binding is dropped (Lua 5.5's coroutine.lua exercises this through
    // `coroutine.wrap`).
    let source = br#"
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        local wrapped = coroutine.wrap(function () coroutine.yield() end)
        cache[1] = wrapped
        wrapped = nil
        collectgarbage()
        return cache[1] == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_weak_value_tables_never_prune_string_values() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua treats a `__mode = "v"` table's *string*-valued entries as
    // always alive: strings are interned, not heap-object references, so
    // `iscleared` exempts them from weak-value collection entirely (see
    // `lgc.c`'s handling of `LUA_VSHRSTR`/`LUA_VLNGSTR`). A sweep that instead
    // treats a string like any other heap value would wrongly clear this
    // entry the moment nothing *else* references the same string content,
    // which is exactly what a plain sweep-and-prune pass does for
    // tables/closures/coroutines.
    let source = br#"
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        cache[1] = string.rep("x", 8)
        collectgarbage()
        return cache[1] == "xxxxxxxx"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_scoped_out_local_does_not_keep_a_weak_value_alive() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A local's register is only ever *bookkept* as free once its enclosing
    // scope ends; nothing physically clears the register's runtime content
    // until some later value reuses it. Since a frame's precise-root scan
    // roots its entire fixed-size register array, a retired-but-uncleared
    // register holding `obj` would keep `obj` spuriously reachable for the
    // rest of the enclosing call, defeating weak-table pruning even though
    // `obj` is provably unreachable from Lua's own perspective the moment the
    // `do ... end` block exits. This is the same class of bug
    // `lua-5.5.1-tests/closure.lua`'s weak-table idioms depend on not
    // happening.
    let source = br#"
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        do
            local obj = {}
            cache[1] = obj
        end
        collectgarbage()
        return cache[1] == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_repeat_loop_clears_prior_iterations_local_on_the_back_edge() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Unlike `while`/`for`, `repeat ... until cond` loops back to the top on
    // every continuing iteration via the `until` check's own conditional
    // jump, never running `pop_scope`'s shrink-clear until the whole loop
    // has already exited. Without an explicit clear on that back-edge, a
    // body local's register keeps holding the *previous* iteration's value
    // (still rooted, since a frame's precise-root scan covers its entire
    // fixed register array) for as long as the loop keeps running, even
    // though that value is provably unreachable from Lua's own perspective
    // the moment the next iteration's `local obj = {}` shadows it.
    let source = br#"
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        local n = 0
        local cleared_on_back_edge
        repeat
            local obj = {}
            cache[n] = obj
            n = n + 1
            if n == 2 then
                collectgarbage()
                cleared_on_back_edge = (cache[0] == nil)
            end
        until n >= 2
        return cleared_on_back_edge
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_reclaims_reference_cycles_via_collectgarbage() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // A self-referential table (`t.self = t`) is a genuine reference cycle:
    // once the local `t` goes out of scope, the table's only remaining
    // strong reference is its own `self` field, which plain `Rc` refcounting
    // can never free on its own. Without cycle collection, each iteration
    // leaks a table and this tiny allocation budget is exhausted well before
    // the loop completes.
    let mut without_collection = LuaRuntime::with_budgets(10_000, 100, 512);
    let error = without_collection
        .run(&parse(
            b"for i = 1, 100 do local t = {} t.self = t end return true",
        ))
        .unwrap_err();
    assert!(error.message.contains("allocation budget"), "{error}");

    // Calling `collectgarbage()` every iteration lets the same loop complete
    // under a budget far too small to hold all 100 tables' worth of
    // allocation charges at once (100 tables would need >8,000 bytes; this
    // budget only ever needs to hold a couple of iterations' worth): the
    // trial-deletion cycle collector recognizes each previous iteration's
    // table as unreachable (its register slot was overwritten by the next
    // iteration's `local t = {}`, so its only remaining strong reference is
    // its own `self` field) and reclaims it, crediting its size back into
    // the allocation budget.
    let mut with_collection = LuaRuntime::with_budgets(10_000, 100, 2048);
    let result = with_collection
        .run(&parse(
            b"for i = 1, 100 do local t = {} t.self = t collectgarbage() end return true",
        ))
        .unwrap();
    assert_eq!(result, sol::lua_runtime::LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_charges_growth_of_a_single_table_against_allocation_budget() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Table *creation* charges only the fixed `LuaTable` header
    // (`Instr::NewTable`). Before `charge_new_table_entry`, growing that same
    // table with new keys (`t[i] = i`) was unmetered, so a loop like this one
    // ran until only the separate, non-resettable instruction budget stopped
    // it, never the allocation budget. A single long-lived table growing well
    // past a small allocation budget must now be caught here too.
    let mut runtime = LuaRuntime::with_budgets(1_000_000, 100, 512);
    let error = runtime
        .run(&parse(
            b"local t = {} for i = 1, 1000000 do t[i] = i end return true",
        ))
        .unwrap_err();
    assert!(error.message.contains("allocation budget"), "{error}");
}

#[test]
fn dynamic_lua_runtime_reclaims_a_non_cyclic_parent_table_dropped_by_plain_reassignment() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `nodes` itself is never part of a cycle - it only holds array
    // references *down* into `nodes[1]`/`nodes[2]`, which reference each
    // other (but never back up to `nodes`) to form the cycle. Each loop
    // iteration's `nodes` binding is dropped by plain `Rc` refcounting the
    // instant the next iteration's `local nodes = {}` re-declares it -
    // `collect_cycles`'s trial-deletion reachability walk never runs on it at
    // all, since its `Weak` entry in `gc_tables` is already dead by the time
    // any pass observes it. Only `collect_cycles`'s ledger-based rundown
    // (`record_charge_ledger`/`credit_dead_ledger_entries`) catches this: it
    // must still credit `nodes`'s header and growth charges back on the very
    // next `collectgarbage()` call, or this budget - sized to hold only a
    // couple of iterations' worth of tables - is exhausted long before the
    // loop completes.
    let mut runtime = LuaRuntime::with_budgets(1_000_000, 100, 2048);
    let result = runtime
        .run(&parse(
            br#"
                for i = 1, 200 do
                    local nodes = {}
                    nodes[1] = {}
                    nodes[2] = {}
                    nodes[1].next = nodes[2]
                    nodes[2].next = nodes[1]
                    collectgarbage()
                end
                return true
            "#,
        ))
        .unwrap();
    assert_eq!(result, sol::lua_runtime::LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_table_growth_allocation_budget_is_catchable_via_pcall() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Unlike the hard instruction/call-depth budgets, the allocation budget
    // that `charge_new_table_entry` enforces must be a genuine, `pcall`-
    // catchable Lua error - matching real Lua's allocator-driven out-of-
    // memory error - and the script must be able to keep running afterward
    // instead of the error unwinding the whole program. This mirrors the
    // upstream `heavy.lua` corpus file's own `toomanyidx()` shape exactly:
    // `t` is declared *outside* the `pcall`'d closure (captured as an
    // upvalue) and stays reachable through the catch. The budget here must
    // stay well above `charge_new_table_entry`'s per-charge size: boxing `t`
    // into an upvalue cell and allocating the closure itself both charge the
    // allocation budget too, and both happen in the *outer* frame before
    // `pcall` is ever called, so a budget too small to cover that setup
    // fails outside the protected call instead of inside it.
    let mut runtime = LuaRuntime::with_budgets(1_000_000, 100, 4096);
    let result = runtime
        .run(&parse(
            br#"
                local t = {}
                local ok, err = pcall(function()
                    for i = 1, 1000000 do t[i] = i end
                end)
                local size = #t
                return (not ok)
                    and type(err) == "string"
                    and string.find(err, "allocation budget") ~= nil
                    and size > 0
            "#,
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_reclaims_table_closure_reference_cycles() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // The common real-world idiom: a table stores a closure that captures
    // the same table back (e.g. a self-referencing "object" with a method
    // bound to its own fields upvalue). Neither the table nor the closure
    // is reachable from anywhere else once `obj` goes out of scope, but each
    // is kept alive by the other - a genuine two-object cycle spanning both
    // candidate types the collector tracks. The budget is generous enough to
    // absorb the (unrelated, pre-existing) per-iteration boxing-cell cost of
    // capturing `obj` as an upvalue, which this collector doesn't credit
    // back since a capture cell isn't itself a Lua value - only the table
    // and closure it points to are.
    let mut with_collection = LuaRuntime::with_budgets(10_000, 100, 4096);
    let result = with_collection
        .run(&parse(
            b"for i = 1, 100 do \
                local obj = {} \
                obj.method = function() return obj end \
                collectgarbage() \
              end return true",
        ))
        .unwrap();
    assert_eq!(result, sol::lua_runtime::LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_calls_gc_finalizers_for_collected_cycles() {
    use sol::lua_runtime::{run_source, LuaValue};

    // A table cycle's `__gc` metamethod (registered on its metatable) should
    // be called once collect_cycles reclaims it - not when the script ends,
    // and not more than once for the same table.
    let source = br#"
        local calls = 0
        local mt = { __gc = function(t) calls = calls + 1 end }
        for i = 1, 5 do
            local t = {}
            setmetatable(t, mt)
            t.self = t
            collectgarbage()
        end
        return calls
    "#;
    let LuaValue::Integer(calls) = run_source(source).unwrap().value else {
        panic!("expected an integer result");
    };
    // The very last iteration's table is still reachable (register-held)
    // when the script returns, so it never becomes garbage and its
    // finalizer never runs - only the 4 earlier iterations' tables do.
    assert_eq!(calls, 4);
}

#[test]
fn dynamic_lua_runtime_registers_a_finalizer_from_a_placeholder_gc_field_overwritten_later() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua's `luaC_checkfinalizer` marks an object to-be-finalized the
    // moment `setmetatable` attaches a metatable with *any* non-nil `__gc`
    // field - callability is only checked later, fresh, at actual
    // finalization time. So `setmetatable(u, {__gc = true})` (a placeholder,
    // not a function) already marks `u`; a later plain field write
    // (`getmetatable(u).__gc = function(...) ... end`, not another
    // `setmetatable` call) must still take effect as `u`'s real finalizer.
    // `lua-5.5.1-tests/gc.lua`'s "__gc x weak tables" section depends on
    // exactly this two-phase ordering.
    let source = br#"
        local u = setmetatable({}, { __gc = true })
        local ran = false
        getmetatable(u).__gc = function(o) ran = true end
        u = nil
        collectgarbage()
        return ran
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_reclaims_cycles_without_explicit_collectgarbage() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // With GC stress mode on, every allocation point and every dispatched
    // instruction runs a full collection pass (see `set_gc_stress`), so this
    // cycle-leaking loop needs no explicit `collectgarbage()` call to stay
    // within a tiny allocation budget - the L6 exit gate's stress
    // requirement: collecting under maximal pressure must reclaim every
    // dead cycle and never miss a live reference.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 100, 2048);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            b"for i = 1, 100 do local t = {} t.self = t end return true",
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_never_collects_a_still_reachable_value() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `live` stays reachable (held by a local register) for the whole run
    // while unrelated cyclic garbage is produced and reclaimed around it on
    // every allocation and every instruction. A stress-mode collector that
    // ever miscounted a live register's reference as garbage would clobber
    // `live` well before the loop finishes.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 500, 1024 * 1024);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            br#"
            local live = { marker = 42 }
            for i = 1, 200 do
                local garbage = {}
                garbage.self = garbage
            end
            return live.marker
        "#,
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_reclaims_table_closure_reference_cycles() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Same table+closure mutual-cycle idiom as
    // `dynamic_lua_runtime_reclaims_table_closure_reference_cycles`, but
    // under stress mode instead of an explicit per-iteration
    // `collectgarbage()` call.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 100, 4096);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            b"for i = 1, 100 do \
                local obj = {} \
                obj.method = function() return obj end \
              end return true",
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_calls_finalizers_correctly_for_collected_cycles() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Same table+`__gc`-cycle scenario as
    // `dynamic_lua_runtime_calls_gc_finalizers_for_collected_cycles`, but
    // with GC stress mode collecting automatically at every allocation and
    // instruction instead of the script calling `collectgarbage()` itself,
    // and with no per-iteration `collectgarbage()` call inside the loop
    // body at all - unlike that test, where the explicit mid-body call
    // still finds the current iteration's own `t` reachable (its `for`
    // block hasn't closed yet), here every iteration's `t`, including the
    // fifth, is already out of scope by the time any hook fires again, so
    // all five get finalized (confirmed against real Lua 5.5.1: an
    // otherwise-identical script with one `collectgarbage("collect")` after
    // the loop instead of stress mode also reports `calls == 5`, since a
    // local going out of scope - even at a loop's last iteration - is
    // unreachable before the enclosing function returns, with no need to
    // wait for the call to actually return).
    //
    // Finalizers must still fire exactly once per reclaimed cycle.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 100, 1024 * 1024);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            br#"
            local calls = 0
            local mt = { __gc = function(t) calls = calls + 1 end }
            for i = 1, 5 do
                local t = {}
                setmetatable(t, mt)
                t.self = t
            end
            return calls
        "#,
        ))
        .unwrap();
    let LuaValue::Integer(calls) = result else {
        panic!("expected an integer result");
    };
    assert_eq!(calls, 5);
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_survives_many_coroutine_resume_yield_cycles() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // The coroutine-stress leg of task #12's exit audit (see
    // docs/features/table-closure-coroutine-cutover.md §10): every
    // `resume`/`yield` runs its own dispatch loop with a collection forced at
    // every allocation and every instruction, so the coroutine's own live
    // locals (`sum`, reassigned every iteration; `tmp`, a fresh table
    // declared every iteration) must survive being suspended mid-frame
    // across 50 forced collections, and the still-running coroutine itself
    // (reachable only via `coroutine_stack` while suspended, per
    // `frame_roots`) must never be mistaken for garbage.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 100_000, 1024 * 1024);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            br#"
            local co = coroutine.create(function()
                local sum = 0
                for i = 1, 50 do
                    local tmp = { value = i }
                    sum = sum + tmp.value
                    coroutine.yield(sum)
                end
                return sum
            end)
            local last
            for i = 1, 50 do
                local ok, value = coroutine.resume(co)
                if not ok then
                    return false
                end
                last = value
            end
            return last
        "#,
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Integer(1275));
}

#[test]
fn dynamic_lua_runtime_collects_a_coroutine_kept_alive_only_by_a_cycle_through_its_own_frames() {
    use sol::lua_runtime::{run_source, LuaValue};

    // task #13 (see docs/features/table-closure-coroutine-cutover.md §11):
    // `t`'s only remaining path back to itself is a genuine reference cycle
    // routed entirely through `co`'s own suspended frame - `t` (via its
    // field `co`) points at the coroutine's `Thread` value, and the
    // coroutine's own conditionally-rooted frame (once reachable) points
    // back at its local `t`. Nothing outside the coroutine references any of
    // it once `spawn` returns. Under task #12's unconditional-registry-walk
    // fallback, every registered coroutine's frames were rooted regardless
    // of reachability, so `t` (and this finalizer) would never have been
    // collected at all - `calls` would stay 0 forever instead of reaching 1
    // here.
    //
    // `co` is created, resumed, and dropped entirely inside `spawn`'s own
    // frame (rather than the top-level chunk's) deliberately: this VM's
    // register file is conservatively rooted (an argument-staging temp
    // register used to pass `co` to `coroutine.resume` is never cleared
    // after the call returns), so a value like `co` that's ever touched by
    // the *top-level* chunk's own frame stays reachable for the rest of the
    // script, since that frame is never popped. A helper function's frame,
    // by contrast, is popped from `self.frames` entirely once it returns,
    // taking any such leftover temp register with it - which is what lets
    // this test actually exercise conditional coroutine rooting rather than
    // an unrelated whole-frame conservatism.
    let source = br#"
        local calls = 0
        local mt = { __gc = function(t) calls = calls + 1 end }
        local function spawn()
            local co
            co = coroutine.create(function()
                local t = setmetatable({}, mt)
                t.co = co
                coroutine.yield()
            end)
            coroutine.resume(co)
        end
        spawn()
        collectgarbage()
        return calls
    "#;
    let LuaValue::Integer(calls) = run_source(source).unwrap().value else {
        panic!("expected an integer result");
    };
    assert_eq!(calls, 1);
}

#[test]
fn dynamic_lua_runtime_gc_stress_mode_survives_a_coroutine_resuming_a_coroutine_resuming_a_coroutine() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // Task #13's nested-chain counterpart to
    // `dynamic_lua_runtime_gc_stress_mode_survives_many_coroutine_resume_yield_cycles`
    // (task #12, single-level): `a` resumes `b` resumes `c`, three deep, with
    // a collection forced on every allocation and every instruction
    // throughout. While `c` is actually executing, `coroutine_stack` holds
    // all three (`[a, b, c]`) and every level must stay rooted via that
    // unconditional active-chain walk; between top-level resumes of `a`,
    // `b`/`c` are off that chain entirely and rely on their `Thread` values
    // still being reachable through `a`'s and `b`'s own locals - the ordinary
    // root-tracing path that unlocks task #13's conditional frame roots.
    let mut stress = LuaRuntime::with_budgets(1_000_000, 200_000, 1024 * 1024);
    stress.set_gc_stress(true);
    let result = stress
        .run(&parse(
            br#"
            local c = coroutine.create(function()
                local sum = 0
                for i = 1, 50 do
                    local tmp = { value = i }
                    sum = sum + tmp.value
                    coroutine.yield(sum)
                end
                return sum
            end)
            local b = coroutine.create(function()
                local total = 0
                for i = 1, 50 do
                    local ok, value = coroutine.resume(c)
                    if not ok then return false end
                    total = value
                    coroutine.yield(total)
                end
                return total
            end)
            local a = coroutine.create(function()
                local total = 0
                for i = 1, 50 do
                    local ok, value = coroutine.resume(b)
                    if not ok then return false end
                    total = value
                    coroutine.yield(total)
                end
                return total
            end)
            local last
            for i = 1, 50 do
                local ok, value = coroutine.resume(a)
                if not ok then
                    return false
                end
                last = value
            end
            return last
        "#,
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Integer(1275));
}

#[test]
fn dynamic_lua_runtime_call_chain_resolves_in_order_with_no_metatable_cycle_hang() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua`'s "testing chains of '__call'" case (line
    // ~195): each `__call` hop must prepend its own (still-unresolved)
    // table onto the front of the pending argument list, exactly like real
    // Lua's `luaD_precall` "retry" loop, so a 15-deep chain ending in
    // `table.pack` receives the hops in order followed by the original call
    // arguments. This also exercises the chain-resolution loop's bound (a
    // `__call` cycle must report an error - "chain too long" - rather than
    // hang) via the one-too-long chain that immediately follows it in the
    // same upstream file (line ~221).
    let source = br#"
        local N = 15
        local u = table.pack
        for i = 1, N do
            u = setmetatable({i}, {__call = u})
        end
        local res = u("a", "b", "c")
        if res.n ~= N + 3 then return false end
        for i = 1, N do
            if res[i][1] ~= i then return false end
        end
        if not (res[N + 1] == "a" and res[N + 2] == "b" and res[N + 3] == "c") then
            return false
        end

        local a = {}
        for i = 1, 16 do
            a = setmetatable({}, {__call = a})
        end
        local ok, msg = pcall(a)
        return ok == false and string.find(msg, "too long") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_generational_mode_clears_weak_entries_in_a_single_step() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/gengc.lua`'s "bug introduced in commit 9cf3299fa"
    // case: under `collectgarbage("generational")`, a weak-value table entry
    // pointing at newly-dead young garbage must clear on the very next
    // `collectgarbage("step")` call - real Lua's generational minor
    // collection covers the whole young generation in one cheap pass, unlike
    // the default incremental/major path, which only makes bounded progress
    // per step and can take dozens of calls to reach the same object. This
    // exercises `step_garbage`'s `self.gc_mode == "generational"` branch
    // (`sol_core::Heap::collect_minor_with_conditional_roots`).
    let source = br#"
        collectgarbage("generational")
        local cache = {}
        setmetatable(cache, { __mode = "v" })
        local function stash_unreachable()
            local obj = {}
            cache[1] = obj
        end
        stash_unreachable()
        collectgarbage("step", 1)
        return cache[1] == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_gcstats_counts_minor_collections_scaling_with_explicit_steps() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // U7's "measured barriers" deliverable: `debug.gcstats()` must prove the
    // generational fast path (`step_garbage`'s `"generational"` branch,
    // `sol_core::Heap::collect_minor_with_conditional_roots`) is actually
    // cheap and actually distinct from major collections, not just
    // mechanically different. Each explicit `collectgarbage("step")` call
    // under `"generational"` mode always finishes in that one call (see
    // `step_garbage`'s own doc comment), so `minor_collections` must equal
    // exactly the number of step calls made, while nothing here is anywhere
    // near the 65,536-instruction auto-collect threshold
    // (`AUTO_GC_INSTRUCTION_INTERVAL`), so no major collection should ever
    // fire alongside them.
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let result = runtime
        .run(&parse(
            br#"
            collectgarbage("generational")
            local steps = 20
            for i = 1, steps do
                local junk = {}
                junk.x = i
                collectgarbage("step")
            end
            local stats = debug.gcstats()
            return stats.minor_collections == steps
                and stats.major_collections == 0
                and stats.minor_time >= 0.0
                and stats.major_time >= 0.0
        "#,
        ))
        .unwrap();
    assert_eq!(result, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_debug_gcstats_counts_major_collections_from_explicit_collect() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    // `collectgarbage("collect")` always runs a full major pass
    // (`collect_garbage_with`) regardless of `gc_mode` - distinct from
    // `"step"`, which only runs a major pass outside `"generational"` mode.
    // Three explicit collections must show up as exactly three major
    // collections and zero minor ones, confirming `GcStats::record` keys off
    // the `Collection`'s own reported `kind` rather than `gc_mode`.
    let source = br#"
        for i = 1, 3 do
            local junk = {}
            junk.x = i
            collectgarbage()
        end
        local stats = debug.gcstats()
        return stats.major_collections == 3 and stats.minor_collections == 0
    "#;
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        debug: true,
        ..Capabilities::SANDBOX
    });
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let result = runtime.run(&parse(source)).unwrap();
    assert_eq!(result, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_minor_collection_traces_remembered_old_objects_unreachable_from_roots() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/gengc.lua`'s "another bug in 5.4.0" case (block 3,
    // the `gcf` finalizer test): an `Old` object can become unreachable from
    // any current root (its only local went out of scope) yet still survive
    // every minor collection unconditionally (`should_trace_during_minor`
    // never re-evaluates `Old` reachability), waiting for the next major
    // collection to decide its fate via the finalizer-resurrection path. A
    // minor collection's *mark* phase must still trace such an object's
    // `remembered` write-barrier edges even though it was never reached via
    // a root this round - otherwise a `Young` child it alone references
    // (like its own metatable) is incorrectly swept as garbage, leaving a
    // dangling `ObjectId` for the finalizer to dereference once the object
    // is genuinely resurrected and run. This regressed when the minor
    // collector's `remembered` set gated *tracing through* an already-
    // enqueued id but never seeded the mark queue with `remembered` members
    // themselves, so an unreachable-but-surviving `Old` object was simply
    // never enqueued in the first place.
    let source = br#"
        collectgarbage("generational")
        local anchor = {}
        local function witness(obj)
            anchor.result = getmetatable(obj).x
        end
        collectgarbage()   -- make 'anchor' old
        do
            local obj = {}
            collectgarbage("step")   -- promote 'obj' to old
            setmetatable(obj, { __gc = witness, x = "marker" })   -- young metatable, remembered edge
            obj = nil   -- drop the only root reference
        end
        collectgarbage("step")   -- minor: must not drop the remembered metatable
        collectgarbage()         -- major: resurrects 'obj', runs its finalizer
        return anchor.result == "marker"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
