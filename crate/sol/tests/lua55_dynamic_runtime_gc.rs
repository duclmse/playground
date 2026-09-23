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
    // instruction instead of the script calling `collectgarbage()` itself -
    // finalizers must still fire exactly once per reclaimed cycle, and the
    // still-reachable final iteration's table must not be finalized early.
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
    assert_eq!(calls, 4);
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
