//! Regression coverage for sol::lua_runtime's dynamic .lua runtime, exercised
//! directly against the in-process API (split out of lua55_dynamic_runtime.rs,
//! grouped by theme: load/require/package and io/os).

/// Creates a fresh, uniquely-named scratch directory under the OS temp
/// directory for the filesystem-touching `require`/`io`/`os.remove`
/// regression tests below - real disk I/O, gated on
/// `Capabilities::filesystem`, needs actual paths rather than an in-memory
/// stand-in. Unique per call (a process-wide counter combined with the
/// process id) so parallel `cargo test` runs never collide on the same path.
fn make_scratch_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sol-lua55-dynamic-runtime-{tag}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn dynamic_lua_runtime_loads_sandboxed_modules_deterministically() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::new();
    assert!(!runtime.capabilities().package);
    runtime.add_module(
        "answer",
        br#"
            hidden = 40
            local exports = { value = hidden + 2, name = _NAME }
            return exports
        "#,
    );
    runtime.add_module(
        "cycle_a",
        br#"local b = require("cycle_b"); return { name = "a", b = b }"#,
    );
    runtime.add_module(
        "cycle_b",
        br#"local a = require("cycle_a"); return { name = "b", a = a }"#,
    );
    assert!(runtime.capabilities().package);
    let value = runtime
        .run(&parse(
            br#"
                local first = require("answer")
                local second = require("answer")
                local cycle = require("cycle_a")
                return first == second and first.value == 42 and
                    first.name == "answer" and hidden == nil and
                    package.loaded.answer == first and
                    cycle.name == "a" and cycle.b.name == "b" and cycle.b.a == true
            "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));

    // `require` on a name that isn't in `module_sources` (the sandboxed,
    // deterministic loader above) now falls through to the same
    // `package.preload`/`package.path`/`package.cpath` search real Lua's
    // `require` performs (see `crates/sol/src/lua_runtime/natives.rs`'s
    // `require_search`), rather than a blanket "package capability is
    // disabled" error - `run_source`'s default capabilities still leave
    // `package.path`/`cpath` at their defaults (`./?.lua;./?/init.lua` and
    // `""`) and `capabilities.filesystem` off, so every candidate is
    // reported missing without ever touching the real filesystem.
    let denied = sol::lua_runtime::run_source(b"return require('absent')").unwrap_err();
    assert_eq!(
        denied.message,
        "module 'absent' not found:\n\
         \tno field package.preload['absent']\n\
         \tno file './absent.lua'\n\
         \tno file './absent/init.lua'"
    );
}

#[test]
fn dynamic_lua_runtime_exposes_package_path_cpath_config_and_preload() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/attrib.lua` lines 13-18: `package.path`, `cpath`,
    // `loaded`, and `preload` must all be present with real Lua's types
    // (string/string/table/table), and `package.config` must be the
    // standard 5-line `dirsep\n;\n?\n!\n-\n` block (verified against the
    // pinned `lua5.5.1` oracle: `"/\n;\n?\n!\n-\n"` on a `/`-separated host).
    let source = br#"
        return type(package.path) == "string" and
            type(package.cpath) == "string" and
            type(package.loaded) == "table" and
            type(package.preload) == "table" and
            package.config == "/\n;\n?\n!\n-\n"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_require_reports_exact_module_not_found_message() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/attrib.lua` lines 51-71: byte-for-byte match against
    // real Lua 5.5.1's `require` error, once `package.path`/`cpath` are set
    // to templates with no real file backing them.
    let source = br#"
        package.path = "?.lua;?/?"
        package.cpath = "?.so;?/init"
        local ok, msg = pcall(require, 'XXX')
        local expected = "module 'XXX' not found:\n" ..
            "\tno field package.preload['XXX']\n" ..
            "\tno file 'XXX.lua'\n" ..
            "\tno file 'XXX/XXX'\n" ..
            "\tno file 'XXX.so'\n" ..
            "\tno file 'XXX/init'"
        return ok == false and msg == expected
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_package_searchpath_reports_every_candidate() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `package.searchpath` never loads or executes anything - it's a pure
    // string/filesystem-probe utility - so it's implemented fully (not
    // capability-gated to "always fail" the way real dynamic module
    // *loading* is). With `capabilities.filesystem` off (the `run_source`
    // default), every candidate the template search produces is reported
    // as missing, exactly like `require`'s own search over the same
    // `package.path`/`cpath` machinery.
    let source = br#"
        local ok, msg = package.searchpath('foo', './?.lua;./?/init.lua')
        return ok == nil and
            string.find(msg, "no file './foo.lua'", 1, true) ~= nil and
            string.find(msg, "no file './foo/init.lua'", 1, true) ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_require_calls_a_package_preload_loader() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `package.preload[name]` is checked before any `package.path`/`cpath`
    // search (real Lua's `searcher_preload`), and `require` caches the
    // loader's return value in `package.loaded` exactly like a
    // path-resolved module would be. `LuaRuntime` reads this straight off
    // its own `package_table` field (the same table object `install_base`
    // exposes as the `package` global), independent of `Globals::define`'s
    // separate constant-global bookkeeping for the `package` name itself
    // (which - unlike real Lua - Sol's dynamic runtime rejects
    // reassigning: `lua-5.5.1-tests/attrib.lua`'s "testing preload"
    // section, which does `package = {}`, lives inside the `_port`-guarded
    // block and is never exercised by a standalone run of that file for
    // this reason).
    let source = br#"
        package.preload['mymod'] = function(name)
            return { seen_name = name }
        end
        local first = require('mymod')
        local second = require('mymod')
        return first.seen_name == 'mymod' and first == second and
            package.loaded.mymod == first
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_drives_deeply_nested_dofile_chains_without_native_stack_overflow() {
    use sol::lua_runtime::{LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    // `dofile` recompiles a fresh module-local environment on every call
    // (unlike `require`, which caches), so a plain global reassignment
    // inside the module would only ever shadow itself in that call's own
    // fresh scope. A shared mutable table's field write is not a global
    // reassignment, so it carries the countdown across the whole chain.
    // Each level's `dofile("chain")` is dispatched through
    // `NativeCont::Once`, so this must not grow the native Rust call stack
    // even though every level also recompiles its chunk from source.
    let mut runtime = LuaRuntime::with_limits(50_000_000, 400_000);
    runtime.add_module(
        "chain",
        br#"
            state.n = state.n - 1
            if state.n > 0 then
                return dofile("chain")
            end
            return 42
        "#,
    );
    let source = br#"
        state = { n = 50000 }
        return dofile("chain")
    "#;
    let run = runtime.run(&parse(source)).unwrap();
    assert_eq!(run, LuaValue::Integer(42));
}

#[test]
fn dynamic_lua_runtime_gates_os_and_io_behind_explicit_capabilities() {
    use sol::lua_runtime::{run_source, Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();

    let denied = run_source(b"return os.time()").unwrap_err();
    assert!(
        denied.message.contains("clock capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"return os.getenv('HOME')").unwrap_err();
    assert!(
        denied
            .message
            .contains("environment capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"return os.exit(0)").unwrap_err();
    assert!(
        denied.message.contains("process capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"return io.write('x')").unwrap_err();
    assert!(
        denied.message.contains("stdout capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"return io.read('l')").unwrap_err();
    assert!(
        denied.message.contains("stdin capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"return dofile('missing')").unwrap_err();
    assert!(
        denied.message.contains("package capability is disabled"),
        "{}",
        denied.message
    );

    let denied = run_source(b"local f = function() end return debug.upvalueid(f, 1)").unwrap_err();
    assert!(
        denied.message.contains("debug capability is disabled"),
        "{}",
        denied.message
    );

    // Pure library operations remain available in a sandbox. Formatting an
    // explicit timestamp and subtracting timestamps do not consult the host.
    let value =
        run_source(br#"return os.difftime(10, 4) == 6 and os.date("%Y-%m-%d", 0) == "1970-01-01""#)
            .unwrap()
            .value;
    assert_eq!(value, LuaValue::Bool(true));

    // Enabling one authority cannot accidentally enable a neighboring one.
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        clock: true,
        ..Capabilities::SANDBOX
    });
    let denied = runtime
        .run(&parse(b"local _ = os.time() return os.getenv('HOME')"))
        .unwrap_err();
    assert!(
        denied
            .message
            .contains("environment capability is disabled"),
        "{}",
        denied.message
    );
}

#[test]
fn dynamic_lua_runtime_implements_os_time_clock_difftime_date_and_getenv() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        clock: true,
        environment: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local t = os.date("*t", 0)
            local rendered = os.date("%Y-%m-%d %H:%M:%S", 0)
            local now = os.time()
            local elapsed = os.clock()
            return type(now) == "number" and elapsed >= 0 and
                os.difftime(10, 4) == 6 and
                t.year == 1970 and t.month == 1 and t.day == 1 and
                t.hour == 0 and t.min == 0 and t.sec == 0 and
                t.wday == 5 and t.yday == 1 and t.isdst == false and
                rendered == "1970-01-01 00:00:00" and
                os.getenv("SOL_LUA55_TEST_VAR_DOES_NOT_EXIST_XYZ") == nil
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_implements_io_write_and_load() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        stdout: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            io.write("hello", " ", "world", 42)
            local chunk, err = load("return 1 + 2")
            local bad, bad_err = load("this is not } lua")
            local assign = assert(load("loaded_global = 17; return loaded_global"))
            local vararg_chunk = assert(load("return {...}"))
            local varargs = vararg_chunk(4, 5)
            local readonly, readonly_err = load("return function (... pack) pack = nil end")
            local env_const, env_const_err = load([[
                local function aux(... _ENV)
                    global <const> answer
                    answer = 10
                end
            ]])
            return err == nil and chunk() == 3 and bad == nil and
                type(bad_err) == "string" and assign() == 17 and loaded_global == 17 and
                varargs[1] == 4 and varargs[2] == 5 and readonly == nil and
                string.find(readonly_err, "const variable 'pack'") ~= nil and
                env_const == nil and string.find(env_const_err, "const variable 'answer'") ~= nil
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
    assert_eq!(runtime.take_output(), b"hello world42");

    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        stdout: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime
        .run(&parse(
            br#"
            local same_handle = io.write("a"):write("b") == io.stdout
            io.stdout:write("c"):write("d")
            return same_handle
        "#,
        ))
        .unwrap();
    assert_eq!(value, LuaValue::Bool(true));
    assert_eq!(runtime.take_output(), b"abcd");
}

#[test]
fn dynamic_lua_runtime_load_with_custom_env_redirects_globals() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local env = {Y = 10}
        local f = assert(load("X = Y + 1", nil, nil, env))
        f()
        return env.X == 11 and X == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_require_reloads_a_module_that_cached_a_falsy_value() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Real Lua's `require` (`loadlib.c`'s `ll_require`) treats
    // `package.loaded[name]` as "already loaded" only when it is *truthy*
    // (`lua_toboolean`), not merely non-nil: a module whose loader
    // legitimately returns/caches `false` (as
    // `lua-5.5.1-tests/attrib.lua`'s "default option" case does) must be
    // reloaded on every subsequent `require` of the same name, exactly like
    // a module that was never loaded at all. Sol used to cache any non-nil
    // value forever, which included `false`.
    let source = br#"
        local calls = 0
        package.preload.falsy_module = function(...)
            calls = calls + 1
            return false
        end
        local a = require("falsy_module")
        local b = require("falsy_module")
        return a == false and b == false and calls == 2
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_require_loads_real_lua_files_and_sub_packages_from_package_path() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let dir = make_scratch_dir("require-path");
    std::fs::write(dir.join("greet.lua"), b"return {hello = 'world'}").unwrap();
    let sub_dir = dir.join("pkg");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("child.lua"), b"return 99").unwrap();

    let source = format!(
        r#"
        package.path = "{dir}/?.lua"
        local mod, resolved = require("greet")
        local child = require("pkg.child")
        return mod.hello == "world" and child == 99 and
            resolved:find("greet.lua", 1, true) ~= nil
        "#,
        dir = dir.display(),
    );
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        filesystem: true,
        ..Capabilities::SANDBOX
    });
    let value = runtime.run(&parse(source.as_bytes())).unwrap();
    assert_eq!(value, LuaValue::Bool(true));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamic_lua_runtime_io_output_write_close_and_os_remove_use_the_real_filesystem() {
    use sol::lua_runtime::{Capabilities, LuaRuntime, LuaValue};

    let dir = make_scratch_dir("io-output");
    let file_path = dir.join("out.txt");
    let parse =
        |source: &[u8]| sol::parser::parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let mut runtime = LuaRuntime::with_capabilities(Capabilities {
        filesystem: true,
        stdout: true,
        ..Capabilities::SANDBOX
    });

    let write_source = format!(
        r#"
        local handle = io.output("{path}")
        handle:write("hello ", "file")
        io.close(handle)
        return true
        "#,
        path = file_path.display(),
    );
    let value = runtime.run(&parse(write_source.as_bytes())).unwrap();
    assert_eq!(value, LuaValue::Bool(true));
    assert_eq!(std::fs::read(&file_path).unwrap(), b"hello file");

    let remove_source = format!(r#"return os.remove("{path}")"#, path = file_path.display());
    let removed = runtime.run(&parse(remove_source.as_bytes())).unwrap();
    assert_eq!(removed, LuaValue::Bool(true));
    assert!(!file_path.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamic_lua_runtime_require_rejects_a_non_table_package_searchers() {
    use sol::lua_runtime::run_source;

    // `lua-5.5.1-tests/attrib.lua` replaces `package.searchers` with a
    // non-table value and asserts that `require` fails with this exact
    // message rather than silently ignoring the replacement.
    let source = br#"
        package.searchers = "not a table"
        require("anything")
    "#;
    let error = run_source(source).unwrap_err();
    assert!(
        error
            .message
            .contains("'package.searchers' must be a table"),
        "{error}"
    );
}

#[test]
fn dynamic_lua_runtime_require_calls_the_current_package_searchers_in_order() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local calls = {}
        package.searchers = {
            function(name)
                calls[#calls + 1] = "first:" .. name
                return "\n\tfirst miss"
            end,
            function(name)
                calls[#calls + 1] = "second:" .. name
                return function(module_name, extra)
                    return module_name .. ":" .. extra
                end, "custom"
            end,
            function()
                error("search continued after finding a loader")
            end,
        }
        local value, extra = require("replaceable")
        return value == "replaceable:custom" and extra == "custom" and
            calls[1] == "first:replaceable" and calls[2] == "second:replaceable" and
            calls[3] == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_implements_package_loadlib_as_permanently_absent() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Sol has no dynamic C-module (native-library) loader on any platform
    // (see `AGENTS.md`'s sandboxing guidance and
    // `Capabilities::native_modules`), so `package.loadlib` always answers
    // the way real Lua does when it was itself built without dynamic-load
    // support: `nil`, an error message, and `"absent"`.
    let source = br#"
        local ok, message, when = package.loadlib("/nonexistent/lib.so", "luaopen_lib")
        return ok == nil and type(message) == "string" and when == "absent"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_package_global_is_reassignable_like_real_lua() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Unlike most standard-library globals, real Lua's `package` is an
    // ordinary (non-const) global - `lua-5.5.1-tests/attrib.lua`'s
    // "testing preload" section temporarily reassigns it around a
    // `require` call, which must succeed rather than raising "attempt to
    // assign to const variable 'package'".
    let source = br#"
        local original = package
        package = {}
        package = original
        return package == original
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_load_accepts_a_reader_function_that_returns_pieces() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua`'s "test for generic load" section (line
    // ~342): `load`'s first argument may be a *reader* function instead of
    // a string. Real Lua's generic reader protocol (`lbaselib.c`'s
    // `generic_reader`, driven by `lzio.c`'s `luaZ_fill`) calls it
    // repeatedly with no arguments and concatenates each returned string
    // piece into the full chunk source, stopping once a call returns nil or
    // an empty string.
    let source = br#"
        local parts = {"return ", "1", " + ", "2"}
        local i = 0
        local function reader()
            i = i + 1
            return parts[i]
        end
        local f = assert(load(reader, "chunkname", "t"))
        return f() == 3
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_load_reader_returning_nil_immediately_yields_an_empty_chunk() {
    use sol::lua_runtime::{run_source, LuaValue};

    // Same section (lines ~349 and ~356-358): a reader whose very first call
    // returns nil (or an empty string) signals immediate end-of-input, so
    // `load` compiles a valid, empty, callable chunk rather than erroring -
    // matching real Lua's `luaZ_fill`, which treats a NULL buffer the same
    // as a zero-length one. The corpus's own "small bug" case additionally
    // checks that a reader whose *first* returned piece happens to be nil
    // stops there immediately, even though later, never-read pieces would
    // have formed valid source (`table.remove` on `{nil, "return ", "3"}`
    // returns the leading nil on its first call).
    let source = br#"
        local f = assert(load(function () return nil end))
        f()  -- must not error: an empty chunk is a valid, callable no-op

        local pieces = {nil, "return ", "3"}
        local g = assert(load(function () return table.remove(pieces, 1) end))
        return g() == nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_load_rejects_a_text_chunk_in_binary_mode() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua` line 346: `load`'s mode argument (`"b"`,
    // `"t"`, or the default `"bt"`) restricts whether a text or a binary
    // chunk is accepted (real Lua's `lauxlib.c` `checkmode`), independent of
    // whether the source itself would otherwise parse successfully.
    let source = br#"
        local ok, message = load("return 1", "modname", "b", {})
        return ok == nil and string.find(message, "attempt to load a text chunk") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_load_syntax_error_reports_unexpected_symbol() {
    use sol::lua_runtime::{run_source, LuaValue};

    // `lua-5.5.1-tests/calls.lua` lines 410-411:
    // `cannotload("unexpected symbol", load(read1("*a = 123")))` and the
    // same source given directly as a string. Real Lua's parser reports
    // every "this token can't start a statement/expression" error as
    // "unexpected symbol near '...'" (`lparser.c`) - the corpus matches on
    // that literal wording via `string.find`, so it is part of the
    // compatibility contract, not cosmetic phrasing.
    let source = br#"
        local ok, message = load("*a = 123")
        return ok == nil and string.find(message, "unexpected symbol") ~= nil
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}

#[test]
fn dynamic_lua_runtime_load_eof_table_diagnostic_matches_lua() {
    use sol::lua_runtime::{run_source, LuaValue};

    let source = br#"
        local _, message = load("local a = {4\n\n")
        return message == "[string \"local a = {4...\"]:3: '}' expected (to close '{' at line 1) near <eof>"
    "#;
    assert_eq!(run_source(source).unwrap().value, LuaValue::Bool(true));
}
