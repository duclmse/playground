//! Property/fuzz tests for the L8 checklist item ("add property tests and
//! fuzzing for lexer/parser round trips, table operations, multi-result
//! adjustment, metamethod recursion, and GC root handling" -
//! docs/features/lua-compatibility.md). This is a deliberately dependency-free
//! first increment: a small, seeded, self-contained PRNG instead of pulling in
//! `proptest`/`quickcheck`, matching this repo's existing preference for
//! hand-rolled tooling over new dependencies (see e.g. every `scripts/*.sh`
//! TOML reader). It covers lexer/parser crash-safety, table set/get round
//! trips, multi-result-call adjustment, metamethod-chain resolution/cycle
//! safety, and GC root handling.
//!
//! Every generator is seeded (`SOL_FUZZ_SEED`, default fixed) and bounded
//! (`SOL_FUZZ_CASES`, default fixed) so a run is reproducible by default,
//! but can be widened for an exploratory run without editing this file. A
//! failing case's source is printed on panic so it can be lifted into a
//! permanent regression fixture in `tests/lua55.rs`, per this same L8 item's
//! "differential fuzz failures become permanent fixtures" clause.
//!
//! The metamethod-chain tests below run on a worker thread with a large
//! explicit stack (see `run_with_large_stack`) rather than relying on
//! whatever default stack size the host test harness happens to use. This
//! was not a defensive default: designing the cyclic-chain regression test
//! surfaced a real bug (native stack overflow - a hard process abort, not a
//! graceful Lua error - on ordinary, non-pathological deep Lua recursion,
//! well below the documented `max_call_depth` budget), which is now also
//! fixed at the `sol` CLI layer in `crates/sol/src/main.rs`. Giving these
//! tests their own explicit large stack keeps them deterministic across
//! environments instead of depending on the test harness's own thread
//! defaults. See `docs/features/lua-compatibility.md` for the still-open,
//! separately-tracked gap this points at: Sol's dynamic interpreter recurses
//! natively per nested Lua call, so its practical recursion depth is bounded
//! by `max_call_depth` (1000) long before real Lua's effectively unbounded,
//! heap-stack-based recursion depth.

/// A tiny splitmix64 PRNG - not cryptographic, just deterministic and fast.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A pseudo-random value in `0..bound`. `bound` must be nonzero.
    fn next_range(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

fn seed() -> u64 {
    std::env::var("SOL_FUZZ_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0xC0FFEE_u64)
}

fn cases() -> usize {
    std::env::var("SOL_FUZZ_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(300)
}

#[test]
fn lexer_never_panics_on_arbitrary_bytes() {
    let mut rng = Rng::new(seed());
    for case in 0..cases() {
        let len = rng.next_range(96);
        let bytes: Vec<u8> = (0..len).map(|_| (rng.next_u64() % 256) as u8).collect();
        // Garbage input returning an Err is expected and fine; a panic is
        // the only failure mode this test cares about. lex_bytes takes raw
        // bytes specifically so non-UTF-8 input (including bytes >= 0x80)
        // reaches it, matching this crate's byte-oriented source contract.
        let _ = std::panic::catch_unwind(|| sol::lexer::lex_bytes(&bytes))
            .unwrap_or_else(|_| panic!("case {case} panicked on bytes {bytes:?}"));
    }
}

#[test]
fn lexer_and_parser_never_panic_on_pseudo_lua_token_soup() {
    const TOKENS: &[&str] = &[
        "local",
        "function",
        "end",
        "if",
        "then",
        "elseif",
        "else",
        "return",
        "for",
        "do",
        "while",
        "repeat",
        "until",
        "break",
        "goto",
        "in",
        "nil",
        "true",
        "false",
        "and",
        "or",
        "not",
        "x",
        "y",
        "t",
        "1",
        "0",
        "1.5",
        "-1",
        "\"str\"",
        "'s'",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        ",",
        ";",
        ":",
        "=",
        "==",
        "~=",
        "<",
        ">",
        "<=",
        ">=",
        "+",
        "-",
        "*",
        "/",
        "//",
        "%",
        "^",
        "#",
        "..",
        "...",
        "::lbl::",
        "[[long]]",
        "--comment",
        "\n",
    ];
    let mut rng = Rng::new(seed().wrapping_add(1));
    for case in 0..cases() {
        let token_count = 1 + rng.next_range(48);
        let mut source = String::new();
        for i in 0..token_count {
            if i > 0 {
                source.push(' ');
            }
            source.push_str(TOKENS[rng.next_range(TOKENS.len())]);
        }
        let bytes = source.into_bytes();
        let outcome = std::panic::catch_unwind(|| {
            if let Ok(tokens) = sol::lexer::lex_bytes(&bytes) {
                let _ = sol::parser::parse_with_mode(tokens, sol::parser::SourceMode::Lua);
            }
        });
        outcome.unwrap_or_else(|_| {
            panic!(
                "case {case} panicked on token soup: {:?}",
                String::from_utf8_lossy(&bytes)
            )
        });
    }
}

#[test]
fn dynamic_lua_table_string_key_set_get_matches_a_reference_model() {
    use sol::lua_runtime::{run_source, LuaValue};
    use std::collections::HashMap;

    const KEYS: &[&str] = &["a", "b", "c", "d", "42", "1", "name", "x", "y", "z"];
    let mut rng = Rng::new(seed().wrapping_add(2));
    for case in 0..cases().min(150) {
        let mut model: HashMap<&str, i64> = HashMap::new();
        let mut source = String::from("local t = {}\n");
        let op_count = 5 + rng.next_range(25);
        for _ in 0..op_count {
            let key = KEYS[rng.next_range(KEYS.len())];
            let value = (rng.next_u64() % 10_000) as i64;
            model.insert(key, value);
            source.push_str(&format!("t[\"{key}\"] = {value}\n"));
        }
        source.push_str("local ok = true\n");
        for (key, value) in &model {
            source.push_str(&format!("ok = ok and t[\"{key}\"] == {value}\n"));
        }
        source.push_str("return ok\n");
        let run = run_source(source.as_bytes())
            .unwrap_or_else(|error| panic!("case {case} failed to run: {error}\n{source}"));
        assert_eq!(run.value, LuaValue::Bool(true), "case {case}:\n{source}");
    }
}

#[test]
fn dynamic_lua_multi_result_assignment_matches_lua_rules_across_random_arities() {
    use sol::lua_runtime::{run_source, LuaValue};

    let mut rng = Rng::new(seed().wrapping_add(3));
    for case in 0..cases().min(150) {
        let returned_count = rng.next_range(5); // f() returns 0..=4 values
        let target_count = 1 + rng.next_range(5); // 1..=5 assignment targets
        let returns: Vec<i64> = (0..returned_count).map(|i| (i as i64 + 1) * 10).collect();
        let return_list = returns
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let targets: Vec<String> = (0..target_count).map(|i| format!("a{i}")).collect();

        let mut source = format!("local function f() return {return_list} end\n");
        source.push_str(&format!("local {} = f()\n", targets.join(", ")));
        source.push_str("local ok = true\n");
        for (index, name) in targets.iter().enumerate() {
            if index < returns.len() {
                source.push_str(&format!("ok = ok and {name} == {}\n", returns[index]));
            } else {
                source.push_str(&format!("ok = ok and {name} == nil\n"));
            }
        }
        source.push_str("return ok\n");
        let run = run_source(source.as_bytes()).unwrap_or_else(|error| {
            panic!(
                "case {case} (returns={returned_count}, targets={target_count}) failed: {error}\n{source}"
            )
        });
        assert_eq!(
            run.value,
            LuaValue::Bool(true),
            "case {case} (returns={returned_count}, targets={target_count}):\n{source}"
        );
    }
}

#[test]
fn dynamic_lua_table_constructor_expansion_follows_lua_rules_across_random_arities() {
    use sol::lua_runtime::{run_source, LuaValue};

    let mut rng = Rng::new(seed().wrapping_add(4));
    for case in 0..cases().min(150) {
        let returned_count = rng.next_range(5); // f() returns 0..=4 values
        let returns: Vec<i64> = (0..returned_count).map(|i| (i as i64 + 1) * 10).collect();
        let return_list = returns
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");

        let mut source = format!("local function f() return {return_list} end\n");
        // A call in the last position of a table constructor expands to all
        // of its results.
        source.push_str("local last = { f() }\n");
        // A call anywhere else is adjusted to exactly one result (nil if it
        // returned zero) - the position after it in the constructor is
        // still set from the *syntactic* argument list, not from a
        // sequential "array append" of however many values came back.
        source.push_str("local nonlast = { f(), 999 }\n");
        source.push_str("local ok = true\n");
        source.push_str(&format!("ok = ok and #last == {returned_count}\n"));
        for (index, value) in returns.iter().enumerate() {
            source.push_str(&format!("ok = ok and last[{}] == {value}\n", index + 1));
        }
        if returns.is_empty() {
            source.push_str("ok = ok and nonlast[1] == nil\n");
        } else {
            source.push_str(&format!("ok = ok and nonlast[1] == {}\n", returns[0]));
        }
        source.push_str("ok = ok and nonlast[2] == 999\n");
        source.push_str("return ok\n");

        let run = run_source(source.as_bytes()).unwrap_or_else(|error| {
            panic!("case {case} (returns={returned_count}) failed: {error}\n{source}")
        });
        assert_eq!(
            run.value,
            LuaValue::Bool(true),
            "case {case} (returns={returned_count}):\n{source}"
        );
    }
}

/// Runs `body` on a worker thread with a large explicit stack, so tests that
/// exercise deep metamethod/call recursion don't depend on the ambient test
/// harness's default thread stack size. See the module doc comment.
fn run_with_large_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(body)
        .expect("failed to spawn worker thread")
        .join()
        .expect("worker thread panicked")
}

#[test]
fn dynamic_lua_index_chain_of_random_depth_resolves_through_every_level() {
    use sol::lua_runtime::{run_source, LuaValue};

    let mut rng = Rng::new(seed().wrapping_add(5));
    for case in 0..cases().min(150) {
        let depth = 1 + rng.next_range(80);
        let payload = (rng.next_u64() % 100_000) as i64;

        // nodes[1] has no `payload` field of its own; it falls back through
        // nodes[2], nodes[3], ... via `__index` until it reaches nodes[depth],
        // which actually holds the value. A linear (non-cyclic) chain of
        // random depth should always resolve correctly, however long.
        let mut source = format!("local nodes = {{}}\nfor i = 1, {depth} do nodes[i] = {{}} end\n");
        source.push_str(&format!(
            "for i = 1, {} do setmetatable(nodes[i], {{ __index = nodes[i + 1] }}) end\n",
            depth - 1
        ));
        source.push_str(&format!("nodes[{depth}].payload = {payload}\n"));
        source.push_str("return nodes[1].payload\n");

        let run = run_source(source.as_bytes()).unwrap_or_else(|error| {
            panic!("case {case} (depth={depth}) failed: {error}\n{source}")
        });
        assert_eq!(
            run.value,
            LuaValue::Integer(payload),
            "case {case} (depth={depth}):\n{source}"
        );
    }
}

#[test]
fn dynamic_lua_cyclic_index_chain_errors_gracefully_instead_of_crashing() {
    use sol::lua_runtime::run_source;

    let mut rng = Rng::new(seed().wrapping_add(6));
    for case in 0..cases().min(150) {
        let ring_size = 2 + rng.next_range(49); // 2..=50

        // A ring of tables whose `__index` fallback always points to the
        // *next* table mod ring_size: no table ever holds the queried key,
        // so resolution can never succeed - it must be stopped by the
        // MAX_METATABLE_CHAIN guard (see lua_runtime.rs's index_chain),
        // erroring gracefully instead of recursing forever / overflowing
        // the native stack.
        let mut source =
            format!("local nodes = {{}}\nfor i = 1, {ring_size} do nodes[i] = {{}} end\n");
        source.push_str(&format!(
            "for i = 1, {ring_size} do setmetatable(nodes[i], {{ __index = nodes[(i % {ring_size}) + 1] }}) end\n"
        ));
        source.push_str("return nodes[1].missing\n");

        let error = match run_source(source.as_bytes()) {
            Err(error) => error,
            Ok(run) => panic!(
                "case {case} (ring_size={ring_size}) unexpectedly succeeded with {:?}:\n{source}",
                run.value
            ),
        };
        assert!(
            error.message.contains("chain too long"),
            "case {case} (ring_size={ring_size}): unexpected error {error:?}\n{source}"
        );
    }
}

#[test]
fn dynamic_lua_call_chain_of_random_depth_dispatches_through_every_level() {
    use sol::lua_runtime::{run_source, LuaValue};

    let mut rng = Rng::new(seed().wrapping_add(7));
    for case in 0..cases().min(60) {
        let depth = 1 + rng.next_range(300);
        let base = (rng.next_u64() % 10_000) as i64;

        // Each forwarder is a table whose `__call` metamethod delegates to
        // the next callable in the chain; the innermost one is a plain
        // function that adds 1. Dispatch must thread all the way through a
        // random-depth chain of these without misrouting arguments/results.
        let source = format!(
            "local function make_forwarder(next_callable)\n\
             \x20 return setmetatable({{}}, {{ __call = function(self, ...) return next_callable(...) end }})\n\
             end\n\
             local chain = function(x) return x + 1 end\n\
             for i = 1, {depth} do chain = make_forwarder(chain) end\n\
             return chain({base})\n"
        );

        // `LuaValue`/`LuaError` hold `Rc`s and so are not `Send`; reduce the
        // result to a plain, `Send`-safe `Result<i64, String>` before it
        // crosses back off the worker thread.
        let source_owned = source.clone();
        let outcome: Result<i64, String> = run_with_large_stack(move || {
            run_source(source_owned.as_bytes())
                .map_err(|error| error.message)
                .and_then(|run| match run.value {
                    LuaValue::Integer(value) => Ok(value),
                    other => Err(format!("expected an integer, got {other:?}")),
                })
        });
        let value = outcome.unwrap_or_else(|error| {
            panic!("case {case} (depth={depth}) failed: {error}\n{source}")
        });
        assert_eq!(value, base + 1, "case {case} (depth={depth}):\n{source}");
    }
}

#[test]
fn dynamic_lua_random_shaped_table_cycles_are_reclaimed_under_a_tight_budget() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    let mut rng = Rng::new(seed().wrapping_add(8));
    for case in 0..cases().min(30) {
        let ring_size = 2 + rng.next_range(5); // 2..=6 tables per garbage ring
        const ITERATIONS: usize = 20;
        // Tight enough that leaking even a couple of iterations' worth of
        // ring_size-table cycles exhausts it, generous enough that
        // reclaiming everything but the current iteration's ring does not.
        let allocation_budget = ring_size * 2000;

        // Each iteration builds a fresh ring of `ring_size` tables linked
        // `nodes[i].next = nodes[i+1]` and closed with `nodes[N].next =
        // nodes[1]` - a genuine cycle spanning a randomized number of
        // tables - then immediately becomes unreachable (the loop's `nodes`
        // local is overwritten next iteration) and must be collected.
        let mut source = format!("for i = 1, {ITERATIONS} do\n");
        source.push_str(&format!(
            "  local nodes = {{}}\n  for j = 1, {ring_size} do nodes[j] = {{}} end\n"
        ));
        source.push_str(&format!(
            "  for j = 1, {ring_size} do nodes[j].next = nodes[(j % {ring_size}) + 1] end\n"
        ));
        source.push_str("  collectgarbage()\nend\nreturn true\n");

        let mut runtime = LuaRuntime::with_budgets(50_000, 200, allocation_budget);
        let result = runtime
            .run(&parse(source.as_bytes()))
            .unwrap_or_else(|error| {
                panic!("case {case} (ring_size={ring_size}) failed: {error}\n{source}")
            });
        assert_eq!(
            result,
            sol::lua_runtime::LuaValue::Bool(true),
            "case {case} (ring_size={ring_size}):\n{source}"
        );
    }
}

#[test]
fn dynamic_lua_gc_never_collects_a_table_still_reachable_through_a_random_surviving_path() {
    use sol::lua_runtime::LuaRuntime;

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    let mut rng = Rng::new(seed().wrapping_add(9));
    for case in 0..cases().min(30) {
        let path_depth = 1 + rng.next_range(15); // 1..=15 field hops to the root's payload
        let tag = (rng.next_u64() % 1_000_000) as i64;
        const ITERATIONS: usize = 20;
        let allocation_budget = (path_depth + 5) * 400;

        // `keeper` is reachable the whole time via a randomized-length chain
        // of `.next` field hops from a live local. Interleaved with it, each
        // loop iteration creates and discards an unrelated self-referential
        // table cycle. The keeper chain must survive every collection,
        // proving the collector treats a transitively-reachable object as a
        // real root and never reclaims it, however much unrelated cyclic
        // garbage is collected around it.
        let mut source = String::from("local keeper = {}\nlocal cursor = keeper\n");
        source.push_str(&format!(
            "for i = 1, {path_depth} do cursor.next = {{}} cursor = cursor.next end\n"
        ));
        source.push_str(&format!("cursor.tag = {tag}\n"));
        source.push_str(&format!(
            "for i = 1, {ITERATIONS} do local t = {{}} t.self = t collectgarbage() end\n"
        ));
        source.push_str("local walker = keeper\n");
        source.push_str(&format!(
            "for i = 1, {path_depth} do walker = walker.next end\n"
        ));
        source.push_str("return walker.tag\n");

        let mut runtime = LuaRuntime::with_budgets(50_000, 200, allocation_budget);
        let result = runtime
            .run(&parse(source.as_bytes()))
            .unwrap_or_else(|error| {
                panic!("case {case} (path_depth={path_depth}) failed: {error}\n{source}")
            });
        assert_eq!(
            result,
            sol::lua_runtime::LuaValue::Integer(tag),
            "case {case} (path_depth={path_depth}):\n{source}"
        );
    }
}
