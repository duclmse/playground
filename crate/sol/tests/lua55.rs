//! Feature probes through Sol's own bytecode and Cranelift backends.
use std::path::Path;
use std::process::Command;
use std::sync::Once;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lua55");

fn ensure_staticlib() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--offline",
                "--manifest-path",
                concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
            ])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "failed to build Sol's AOT runtime static library"
        );
    });
}

#[test]
fn native_lua_features_agree_across_tiers() {
    let mut files: Vec<_> = std::fs::read_dir(Path::new(FIXTURES).join("native"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for file in files {
        for (mode, promote, osr) in [
            ("bytecode", "4294967295", "4294967295"),
            ("native", "1", "4294967295"),
            ("OSR", "4294967295", "1"),
        ] {
            let out = Command::new(env!("CARGO_BIN_EXE_sol"))
                .arg("run")
                .arg(&file)
                .env("SOL_PROMOTE_THRESHOLD", promote)
                .env("SOL_OSR_THRESHOLD", osr)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{} ({mode}): {}",
                file.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "true",
                "{} ({mode})",
                file.display()
            );
        }
    }
}

#[test]
fn invalid_control_flow_and_const_writes_are_rejected() {
    for (source, reason) in [
        ("break; return 1", "break outside a loop"),
        ("local x <const> = 1; x=2; return x", "read-only"),
        ("for i=1,2 do i=3 end; return 1", "read-only"),
        ("local x <unknown> = 1; return x", "unknown attribute"),
        (
            "repeat local done=true until done; return done",
            "undefined variable",
        ),
    ] {
        let error = sol::compile(source).unwrap_err();
        assert!(error.contains(reason), "{source}: {error}");
    }
}

#[test]
fn lua_strings_lex_as_bytes_with_correct_line_numbers() {
    use sol::lexer::{lex, Token};
    let tokens=lex("--[=[comment\r\nsecond line]=]\r\n'\\x41\\065\\u{1f600}\\0\\255'\r\n[==[\r\ntext\rline]==]").unwrap();
    assert_eq!(tokens[0].line, 3);
    assert_eq!(tokens[0].span.start, 30);
    assert_eq!(tokens[0].span.column, 1);
    assert_eq!(
        tokens[0].token,
        Token::StringLit(b"AA\xf0\x9f\x98\x80\0\xff".to_vec())
    );
    assert_eq!(tokens[1].line, 4);
    assert_eq!(tokens[1].span.column, 1);
    assert_eq!(tokens[1].token, Token::StringLit(b"text\nline".to_vec()));
    for invalid in [
        "0x",
        "1e+",
        "1..2",
        "'\\q'",
        "'\\256'",
        "[=[unfinished",
        "'\\u{}'",
    ] {
        assert!(lex(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn lua_and_sol_modes_share_the_lua_statement_and_expression_grammar() {
    use sol::ast::{ExprKind, GlobalBinding, GlobalName, Stmt, TableField};
    use sol::parser::{parse, parse_lua};

    let source = include_bytes!("fixtures/lua55/lua55_global_syntax.lua");
    let program = parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    // `function object.method:call(...)` is sugar for a field assignment
    // (`object.method.call = ...`), not a global-name declaration - it can't
    // be hoisted ahead of the statement that creates `object` (or, as here,
    // ahead of `object` even existing at all), so it stays an in-order
    // `Stmt::GlobalFunction` in `main`'s body instead of a separate
    // top-level `program.functions` entry.
    assert_eq!(
        program.functions.len(),
        1,
        "only the synthesized main chunk"
    );
    let body = &program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap()
        .body;
    assert!(matches!(
        &body[0],
        Stmt::Global { names, values, .. }
            if names == &vec![GlobalBinding { name: GlobalName::All, constant: true }] && values.is_empty()
    ));
    assert!(matches!(
        &body[1],
        Stmt::Global { names, values, .. }
            if names == &vec![GlobalBinding { name: GlobalName::Name("answer".into()), constant: false }] && values.len() == 1
    ));
    assert!(matches!(&body[2], Stmt::GlobalFunction(function) if function.name == "identity"));
    assert!(matches!(&body[3], Stmt::LocalFunction(function) if function.name == "local_identity"));
    assert!(
        matches!(&body[4], Stmt::GlobalFunction(function) if function.name == "object.method:call" && function.params[0].0 == "self")
    );
    assert!(matches!(
        &body[5],
        Stmt::Global { names, values, .. }
            if names == &vec![GlobalBinding { name: GlobalName::None, constant: false }] && values.is_empty()
    ));
    assert!(matches!(
        &body[6],
        Stmt::Local { value, .. }
            if matches!(&value.kind, ExprKind::Table(fields)
                if matches!(fields.as_slice(), [TableField::Value(_), TableField::Named(name, _), TableField::Key(_, _) ] if name == "key"))
    ));
    assert!(
        matches!(&body[7], Stmt::Expr(expr) if matches!(&expr.kind, ExprKind::Call(name, args) if name == "require" && args.len() == 1))
    );
    assert!(
        matches!(&body[8], Stmt::Expr(expr) if matches!(&expr.kind, ExprKind::Call(name, args) if name == "consume" && args.len() == 1))
    );
    assert!(matches!(
        &body[9],
        Stmt::Local { value, .. }
            if matches!(&value.kind, ExprKind::Function(function)
                if function.vararg && function.params.len() == 1
                    && function.vararg_name.as_deref() == Some("values")
                    && matches!(&function.body[0], Stmt::Local { value, .. } if matches!(value.kind, ExprKind::Vararg))
                    && matches!(&function.body[1], Stmt::MultiReturn { values, .. } if values.len() == 2))
    ));
    assert!(matches!(&body[10], Stmt::Label { name, .. } if name == "again"));
    assert!(matches!(&body[11], Stmt::Goto { name, .. } if name == "again"));
    assert!(matches!(
        &body[12],
        Stmt::GenericFor { vars, iterators, body, .. }
            if vars == &vec!["key".to_string(), "value".to_string()]
                && iterators.len() == 1
                && matches!(&body[0], Stmt::Expr(expr) if matches!(expr.kind, ExprKind::MethodCall(_, _, _)))
    ));
    assert!(matches!(
        &body[13],
        Stmt::Expr(expr) if matches!(expr.kind, ExprKind::CallExpr(_, _))
    ));

    let shared = b"global answer = 42\nfunction object:method() return self end";
    let lua_ast = parse_lua(sol::lexer::lex_bytes(shared).unwrap()).unwrap();
    let sol_ast = parse(sol::lexer::lex_bytes(shared).unwrap()).unwrap();
    assert_eq!(
        lua_ast, sol_ast,
        "annotation-free Lua must have one semantic AST"
    );
    let vararg_chunk = parse_lua(sol::lexer::lex_bytes(b"local value = ...").unwrap()).unwrap();
    let vararg_main = vararg_chunk
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap();
    assert!(vararg_main.vararg);
    assert!(matches!(
        &vararg_main.body[0],
        Stmt::Local { value, .. } if matches!(value.kind, ExprKind::Vararg)
    ));
    let runtime_error = sol::compile_bytes(source, sol::parser::SourceMode::Lua).unwrap_err();
    assert!(
        runtime_error.contains("dynamic Lua function/global syntax"),
        "{runtime_error}"
    );
}

#[test]
fn fn_remains_an_identifier_in_lua_mode() {
    use sol::ast::Stmt;
    use sol::lexer::Token;

    let tokens = sol::lexer::lex_bytes(b"local fn = 41\nreturn fn + 1\n").unwrap();
    assert!(matches!(&tokens[1].token, Token::Ident(name) if name == "fn"));
    let program = sol::parser::parse_lua(tokens).unwrap();
    let main = program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap();
    assert!(matches!(&main.body[0], Stmt::Local { name, .. } if name == "fn"));
}

#[test]
fn lua_mode_parses_the_l1_statement_and_expression_surface() {
    use sol::ast::{ExprKind, Stmt, TableField};
    use sol::parser::parse_lua;

    let source = include_bytes!("fixtures/lua55/lua55_parser_surface.lua");
    let program = parse_lua(sol::lexer::lex_bytes(source).unwrap()).unwrap();
    let main = program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .unwrap();

    // `function module.member:method(...)` is sugar for a field assignment,
    // not a global-name declaration - it stays an in-order
    // `Stmt::GlobalFunction` in `main`'s body rather than a separate
    // top-level `program.functions` entry (see the equivalent case in
    // `lua_mode_parses_global_declarations_and_sol_mode_rejects_them`).
    assert!(main.body.iter().any(
        |statement| matches!(statement, Stmt::GlobalFunction(function) if function.name == "module.member:method" && function.vararg)
    ));
    assert!(matches!(
        &main.body[1],
        Stmt::MultiLocal { names, values, .. } if names.len() == 2 && values.len() == 2
    ));
    assert!(matches!(
        &main.body[2],
        Stmt::Local { value, .. }
            if matches!(&value.kind, ExprKind::Table(fields)
                if matches!(fields.as_slice(), [
                    TableField::Value(_), TableField::Named(name, _), TableField::Key(_, _)
                ] if name == "named"))
    ));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::Block(_))));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::If { .. })));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::While { .. })));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::Repeat { .. })));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::NumericFor { .. })));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::GenericFor { .. })));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::LocalFunction(_))));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::Label { name, .. } if name == "again")));
    assert!(main
        .body
        .iter()
        .any(|statement| matches!(statement, Stmt::Goto { name, .. } if name == "again")));
    assert!(
        matches!(main.body.last(), Some(Stmt::MultiReturn { values, .. }) if values.len() == 2)
    );

    for invalid in [
        "local x = [==[unfinished",
        "local x = 0x1p+",
        "::missing_end",
        "function f(...) return function() return ... end end",
    ] {
        assert!(
            sol::lexer::lex_bytes(invalid.as_bytes())
                .and_then(parse_lua)
                .is_err(),
            "accepted invalid Lua syntax: {invalid}"
        );
    }
}

#[test]
fn cli_accepts_non_utf8_lua_source_bytes() {
    let path = std::env::temp_dir().join(format!("sol_lua55_raw_bytes_{}.lua", std::process::id()));
    std::fs::write(&path, b"return '\xff'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"\xff\n");
}

#[test]
fn lexer_and_parser_errors_include_source_columns() {
    let lexical = sol::lexer::lex("\n  @").unwrap_err();
    // `@` (byte 64) is not a printable token the parser could otherwise
    // report a name for, so - like every other lexer error - it carries
    // real Lua's `near '<\N>'` decimal-escaped suffix (see
    // `lexer_errors_report_lua_compatible_near_text` and the invalid-
    // character `checksyntax` cases in `errors.lua`) alongside its column.
    assert_eq!(
        lexical,
        "line 2, column 3: unexpected character [ELEX001] near '<\\64>'"
    );

    let tokens = sol::lexer::lex("function main(\n  1\nend").unwrap();
    let parsed = sol::parser::parse(tokens).unwrap_err();
    assert!(parsed.starts_with("line 2, column 3: expected identifier"));
    assert!(parsed.ends_with("[EPARSE001]"), "{parsed}");
}

/// Real Lua 5.5's `lexerror`/`txtToken` (`llex.c`) append a `near '<token>'`
/// or `near <eof>` suffix to every lexical error, and `lua-5.5.1-tests/
/// literals.lua`'s `lexerror` helper asserts on that suffix (unanchored:
/// `string.find(msg, "near .-" .. err)`). These cases mirror representative
/// `literals.lua` call sites across every near-text code path: quoted-buffer
/// text (including the one extra lookahead byte `esccheck` saves before
/// erroring) for escape/digit-validation failures - even ones that coincide
/// with end-of-input - and a literal `<eof>` only for the handful of
/// structural cases that never save a partial escape (a bare unterminated
/// string/long-string, and a trailing backslash with nothing after it).
#[test]
fn lexer_errors_report_lua_compatible_near_text() {
    let cases: &[(&str, &str)] = &[
        (r#""\x"#, r#"near '"\x'"#),
        (r#""\x."#, r#"near '"\x.'"#),
        (r#""\xAG"#, r#"near '"\xAG'"#),
        (r#""\g"#, r#"near '"\g'"#),
        (r#""\999"#, r#"near '"\999'"#),
        (r#""xyz\300"#, r#"near '"xyz\300'"#),
        (r#""abc\u{100000000}"#, r#"near '"abc\u{100000000'"#),
        (r#""abc\u11r"#, r#"near '"abc\u1'"#),
        (r#""abc\u"#, r#"near '"abc\u'"#),
        (r#""abc\u{11r"#, r#"near '"abc\u{11r'"#),
        (r#""abc\u{r"#, r#"near '"abc\u{r'"#),
        ("'alo\n", "near ''alo'"),
        ("[=[alo]", "near <eof>"),
        ("'alo", "near <eof>"),
        ("'alo \\z", "near <eof>"),
    ];
    for (source, expected_near) in cases {
        let err = sol::lexer::lex(source).unwrap_err();
        assert!(
            err.contains(expected_near),
            "source {source:?}: expected {expected_near:?} in {err:?}"
        );
    }
}

/// Real Lua's shebang skip lives only in `lauxlib.c`'s `luaL_loadfilex`
/// (`skipcomment`), a file-loading helper - never in the lexer itself
/// (`llex.c`) or `lua_load`'s generic reader path. `load()` on an in-memory
/// string must therefore parse a leading `#` as ordinary code (the unary
/// length operator, invalid at statement start), while file-oriented loads
/// (a `sol run` script argument) must still skip a shebang line.
#[test]
fn load_does_not_skip_a_shebang_but_file_loading_still_does() {
    let path = std::env::temp_dir().join(format!("sol_lua55_shebang_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br##"
        local chunk, err = load("#comment\nreturn 1")
        assert(chunk == nil, "load() must not treat a leading '#' as a shebang comment")
        assert(err ~= nil)
        print("load ok")
    "##,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("load ok"));

    let shebang_path =
        std::env::temp_dir().join(format!("sol_lua55_shebang_file_{}.lua", std::process::id()));
    std::fs::write(&shebang_path, b"#!/usr/bin/env lua\nprint(\"ran\")\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", shebang_path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&shebang_path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ran"));
}

/// Real Lua's lexer (`llex.c`'s `luaX_newstring`/`anchorstr`) anchors every
/// string-literal token it produces in a table scoped to the whole compile
/// (`LexState.h`, not per-function), so byte-identical literals share one
/// `TString` object even across nested function bodies - `%p` reports the
/// same address for a literal in an enclosing scope and a textually
/// identical literal compiled inside a separate nested closure. A
/// runtime-computed string of equal content (concatenation) must NOT share
/// that address, since the sharing is a compile-time/lexer phenomenon, not
/// a general same-content-implies-same-object rule. Mirrors
/// `lua-5.5.1-tests/literals.lua`'s "reuse of long strings" test; the
/// literal here is kept over Lua's 40-byte short-string cutoff so this
/// exercises long-string identity, not `LuaValue::String`'s existing
/// content-hash-based `%p` approximation for short strings.
#[test]
fn identical_long_string_literals_share_identity_across_nested_functions() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_string_reuse_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local function getadd(s) return string.format("%p", s) end
        local s1 = "01234567890123456789012345678901234567890123456789"
        local s2 = "01234567890123456789012345678901234567890123456789"
        local function foo() return s1 end
        local function inner()
            return "01234567890123456789012345678901234567890123456789"
        end
        local a1 = getadd(s1)
        assert(a1 == getadd(s2), "identical literals in the same scope must share identity")
        assert(a1 == getadd(foo()), "a literal read through an outer local must share identity")
        assert(
            a1 == getadd(inner()),
            "an identical literal in a separately-compiled nested function must share identity"
        )
        local sd = "0123456789" .. "0123456789012345678901234567890123456789"
        assert(sd == s1 and getadd(sd) ~= a1, "a runtime concat result must not share identity")
        print("string reuse ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("string reuse ok"));
}

/// Real Lua's `llex.c` reports a malformed numeral (e.g. a hex float with no
/// digits after its `p` exponent, or a decimal integer immediately followed
/// by a letter) as `"malformed number"` (`read_numeral`'s `lexerror(ls,
/// "malformed number", TK_FLT)`), which `lua-5.5.1-tests/literals.lua`'s
/// `malformednum` helper matches via `string.find(msg, "malformed number")`.
#[test]
fn lexer_reports_a_malformed_number_not_a_malformed_numeral() {
    for source in ["return 0xep-p", "return 1print()"] {
        let err = sol::lexer::lex(source).unwrap_err();
        assert!(
            err.contains("malformed number"),
            "source {source:?}: expected \"malformed number\" in {err:?}"
        );
    }
}

/// Real Lua's `luaG_opinterror` reports a bitwise-NOT type error using the
/// operand's Lua-visible type name (a `FILE*` userdata's synthetic label,
/// here), e.g. "attempt to perform bitwise operation on a FILE* value" - not
/// the generic "number expected" that a naive `coerce_integer` propagation
/// would produce. A float with no exact integer representation must still
/// report the more specific "number has no integer representation" instead.
#[test]
fn bitwise_not_on_a_non_number_operand_reports_the_operand_type() {
    let path = std::env::temp_dir().join(format!("sol_lua55_bnot_type_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local st1, err1 = pcall(function() return ~io.stdin end)
        assert(not st1 and string.find(err1, "on a FILE%* value"), err1)

        local st2, err2 = pcall(function() return ~(-3.009) end)
        assert(not st2 and string.find(err2, "no integer representation"), err2)

        local st3, err3 = pcall(function() return ~(-3e40) end)
        assert(not st3 and string.find(err3, "no integer representation"), err3)

        print("bnot type errors ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("bnot type errors ok"));
}

/// Real Lua's `luaG_typeerror` always reports an arithmetic type error as
/// "attempt to perform arithmetic on a {type} value", for any non-number,
/// non-numeric-string operand - a plain table with no `__name` metafield
/// included. Sol's binary arithmetic dispatch used to fall back to a
/// non-standard "attempt to perform arithmetic on incompatible Lua values"
/// whenever the operand's diagnostic label happened to equal its plain type
/// name (i.e. whenever it had no `__name` override), which silently
/// swallowed the type name for the overwhelmingly common case.
#[test]
fn arithmetic_on_a_plain_table_or_non_numeric_string_names_the_operand_type() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_arith_type_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local st1, err1 = pcall(function() return {} + 1 end)
        assert(not st1 and string.find(err1, "on a table value"), err1)

        local st2, err2 = pcall(function() return "abc" + 1 end)
        assert(not st2 and string.find(err2, "on a string value"), err2)

        print("arithmetic type errors ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("arithmetic type errors ok"));
}

/// Real Lua's `luaL_where` prepends a "{short_src}:{line}: " position to a
/// string error message - `error(msg)`'s default `level` of 1 uses the
/// position of whichever line called `error`, `level` can walk further up
/// the call stack, `level == 0` (or a non-string message) skips the prefix
/// entirely, and `assert`'s failure message (explicit or the default
/// "assertion failed!") gets the same treatment since real Lua's
/// `luaB_assert` is a plain tail call into `luaB_error`.
#[test]
fn error_and_assert_add_a_luals_where_style_position_prefix() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_where_prefix_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local ok1, msg1 = pcall(function() error("boom") end)
        assert(not ok1 and string.find(msg1, ":%d+: boom$"), msg1)

        local ok2, msg2 = pcall(function() error("boom", 0) end)
        assert(not ok2 and msg2 == "boom", msg2)

        local function inner() error("deep", 2) end
        local function outer() inner() end
        local ok3, msg3 = pcall(outer)
        assert(not ok3 and string.find(msg3, ":%d+: deep$"), msg3)

        local ok4, msg4 = pcall(function() error({code = 1}) end)
        assert(not ok4 and type(msg4) == "table" and msg4.code == 1)

        local ok5, msg5 = pcall(function() assert(false) end)
        assert(not ok5 and string.find(msg5, ":%d+: assertion failed!$"), msg5)

        local ok6, msg6 = pcall(function() assert(false, "custom") end)
        assert(not ok6 and string.find(msg6, ":%d+: custom$"), msg6)

        local ok7, msg7 = pcall(function() assert(false, {code = 9}) end)
        assert(not ok7 and type(msg7) == "table" and msg7.code == 9)

        local f = load("error('loaded boom')")
        local ok8, msg8 = pcall(f)
        assert(not ok8 and msg8 == "[string \"error('loaded boom')\"]:1: loaded boom", msg8)

        print("where prefix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("where prefix ok"));
}

/// Real Lua's `luaG_runerror`/`luaG_addinfo` give every runtime error the
/// VM itself synthesizes (not an explicit `error()`/`assert()` call, which
/// already gets its own prefix - see the test above) a "{short_src}:{line}:
/// " position prefix automatically, or the literal "?:?: " fallback when
/// the raising closure's debug info was stripped (`string.dump(f, true)`,
/// then `load()`ed back) - surfaced by `lua-5.5.1-tests/errors.lua`'s
/// "errors in functions without debug info" section
/// (`checkerr("^%?:%?:", f, {})`). Previously these errors carried no
/// position prefix at all.
#[test]
fn implicit_runtime_errors_get_an_automatic_position_prefix() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_runtime_error_prefix_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok1, msg1 = pcall(function() return {} + 1 end)
        assert(not ok1 and string.find(msg1, ":%d+: attempt to perform arithmetic"), msg1)

        local f = function (a) return a + 1 end
        f = assert(load(string.dump(f, true)))
        local ok2, msg2 = pcall(f, {})
        assert(not ok2 and string.find(msg2, "^%?:%?:"), msg2)
        assert(string.find(msg2, "table value"), msg2)

        print("runtime error prefix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("runtime error prefix ok"));
}

/// The automatic implicit-runtime-error position prefix above
/// (`implicit_runtime_errors_get_an_automatic_position_prefix`) only covers
/// errors `dispatch_step` raises directly and returns through its own `Err`
/// path. Calling a non-callable value - an ordinary `f()`, a metamethod
/// dispatch, or generic-`for`'s dedicated `TForCall` iterator invocation -
/// instead resolves through the separate `step_result_for_call`/`CallLeaf`
/// bridge (`dispatch.rs`), which never reached the prefixing catch site at
/// all: `local f = nil; f()` reported the bare "attempt to call a nil value"
/// with no `chunk:line:` prefix, for every callable value in the runtime, not
/// just `TForCall`. Surfaced by `lua-5.5.1-tests/errors.lua`'s `lineerror`
/// helper (`for k,v in 3 do ... end`, expecting the error's line to match the
/// `for`'s own line). Fixed by applying the same `runtime_error_prefix`
/// convention (only for an internally-synthesized error, i.e.
/// `error.value.is_none()`) in the `CallLeaf` arm's own `Err` handling,
/// applied last - after `annotate_call_error`/`annotate_bad_argument_error`
/// and the `(metamethod '...')` label, both of which pattern-match on the
/// unprefixed message text, so they still fire correctly.
#[test]
fn calling_a_non_callable_value_gets_the_same_automatic_position_prefix() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_call_error_prefix_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok1, msg1 = pcall(function() local f = nil; f() end)
        assert(not ok1 and string.find(msg1, ":%d+: attempt to call a nil value"), msg1)

        -- the position prefix must not interfere with the existing
        -- call-site naming annotation.
        local ok2, msg2 = pcall(function()
            aaa=1; bbbb=2; aaa=math.sin(3)+bbbb(3)
        end)
        assert(not ok2 and string.find(msg2, "attempt to call a number value"), msg2)
        assert(string.find(msg2, "global 'bbbb'", 1, true), msg2)

        print("call error prefix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("call error prefix ok"));
}

/// Real Lua's `lparser.c` line-attributes a call expression to its opening
/// `(`'s own line (`funcargs`), not the callee's line - so `a\n(23)`, a call
/// spanning two source lines, reports its "attempt to call" error on the
/// `(`'s line. Sol's `parser.rs` has two call-parsing sites: the general
/// postfix-chain loop (`parse_postfix`), which already captured a fresh
/// `line` per iteration before checking for `(`, and a duplicate inline fast
/// path inside `parse_primary`'s `Ident` arm (added for Sol's qualified/
/// struct-constructor name handling) that instead reused the callee name's
/// own `line`, captured once at the top of `parse_primary` before any of the
/// name's own dots or the following `(` were even consumed. Since a bare
/// identifier's call syntax is resolved by that inline fast path rather than
/// ever reaching `parse_postfix`'s loop, every `name(args)` call taken this
/// route misattributed a multi-line call to the callee's line instead of the
/// `(`'s line. Fixed by capturing a fresh `line` right at the `(` check in
/// `parse_primary`, matching `parse_postfix`'s existing convention.
#[test]
fn a_call_expressions_line_is_the_open_parens_line_not_the_callees_line() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_call_expr_line_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        b"
        local ok, msg = pcall(function()\n\
        a\n\
        (\n\
        23)\n\
        end)\n\
        assert(not ok and string.find(msg, \":4: attempt to call\"), msg)\n\
        print(\"call expr line ok\")\n\
        ",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("call expr line ok"));
}

/// Real Lua's `luaV_execute`'s `OP_UNM` falls back to `luaT_trybiniTM`'s own
/// `luaG_opinterror(L, p1, p1, "perform arithmetic on")` when unary `-` has no
/// number operand and no `__unm` metamethod, reporting the operand's type
/// ("attempt to perform arithmetic on a {type} value"), the same wording as
/// the binary arithmetic operators and the already-fixed `__bnot`/`BitNot`
/// case. Sol's `unary_resolve`'s `UnaryOp::Neg` arm instead propagated
/// `coerce_number`'s raw, generic "number expected" error verbatim. Fixing
/// the wording broke a second, unrelated thing silently: `dispatch/
/// bytecode.rs`'s `Instr::Neg` naming annotation (the `(local 'x')`/
/// `(global 'x')` suffix) decided whether to fire by exact-matching the OLD
/// message text (`error.message == "number expected"`), so it stopped firing
/// once the wording changed - fixed by matching the new message's prefix
/// instead, the same pattern the adjacent `Instr::Binary` arm already uses.
#[test]
fn unary_minus_on_a_non_number_reports_arithmetic_wording_and_keeps_its_name_annotation() {
    let path = std::env::temp_dir().join(format!("sol_lua55_unm_wording_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local ok, msg = pcall(function() aaa = {}; return -aaa end)
        assert(not ok and string.find(msg, "attempt to perform arithmetic on a table value", 1, true), msg)
        assert(string.find(msg, "global 'aaa'", 1, true), msg)
        print("unary minus wording ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("unary minus wording ok"));
}

/// Real Lua's `lparser.c` line-attributes a binary operator's instruction to
/// the operator token's own line (`luaK_posfix`, using `ls->linenumber` after
/// the operator is consumed), not the left operand's line - so a multi-line
/// expression like `a\n+\n{}` reports its arithmetic error on the `+`'s own
/// line. Sol's `parser.rs`'s `parse_precedence` captured `line` once at the
/// very top of the function, before the left operand (or any left-recursion)
/// was even parsed, and reused that stale line for the resulting `Binary`
/// node. Fixed by capturing a fresh `line` right after the operator token is
/// peeked, immediately before it's consumed.
#[test]
fn a_binary_operators_line_is_the_operators_own_line_not_the_left_operands_line() {
    let path = std::env::temp_dir().join(format!("sol_lua55_binop_line_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        b"
        local ok, msg = pcall(function()\n\
        return\n\
        a\n\
        +\n\
        {}\n\
        end)\n\
        assert(not ok and string.find(msg, \":5: attempt to perform arithmetic\"), msg)\n\
        print(\"binop line ok\")\n\
        ",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("binop line ok"));
}

/// Real Lua's `funcstat` (`lparser.c`) resolves a `function NAME...()`
/// declaration's assignment target - a plain name, or a `.`/`:`-chained
/// prefix for `function a.b.c()`/`function a:m()` - *before* parsing the
/// closure body, then explicitly re-tags the resulting store instruction
/// back to the declaration's own starting line via `luaK_fixline` ("the
/// definition happens in the first line"), even though the closure's own
/// `OP_CLOSURE` instruction is (correctly) tagged with the closing `end`'s
/// line. Sol's `compile_stmt.rs`'s `Stmt::GlobalFunction` arm tagged the
/// name-assignment store (`compile_function_name_assign`) with
/// `function.end_line` instead of `function.line`, so replacing `_ENV` with
/// a non-table value before a `global function foo() ... end` declaration
/// reported the resulting index error on the closing `end`'s line instead of
/// the declaration's own line. The same stale `end_line` also affected the
/// declaration's own Sol-specific "already defined" duplicate-global guard
/// (`emit_check_global_undefined`), fixed alongside it for consistency.
#[test]
fn a_global_function_declarations_assignment_is_line_attributed_to_its_own_declaration_not_the_closing_end(
) {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_globalfunc_line_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok, msg = pcall(load([[
        _ENV = 1
        global function foo ()
          local a = 10
          return a
        end
        ]]))
        assert(not ok and string.find(msg, ":2: attempt to index a number value", 1, true), msg)
        print("global function line ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("global function line ok"));
}

/// A compile-time "label already defined" error (`func_state.rs`'s
/// `record_label`) had no `chunk:line:` position prefix at all - unlike
/// every other compile-time diagnostic in `lua_bytecode/`, which uses the
/// `"line {line}: ..."` convention `natives_load.rs`'s
/// `format_chunk_diagnostic` recognizes and rewrites into the usual
/// `chunk:line:` form. Fixed by prefixing with the *new* (duplicate) label's
/// own line, matching real Lua's `checkrepeated`/`luaX_syntaxerror`, which
/// positions the error at the second occurrence. The message's own "already
/// defined on line N" text still names the first label's declared line,
/// which can differ from real Lua's in the presence of blank lines/comments
/// between the two labels (real Lua's stored label line is `ls->linenumber`
/// read only after the parser's one-token lookahead has already advanced
/// past them) - that narrower quirk is not reproduced here.
#[test]
fn a_duplicate_label_error_has_a_position_prefix_at_the_duplicates_own_line() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_dup_label_prefix_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok, msg = load("::L1::\n::L1::\n")
        assert(not ok and string.find(msg, ":2: label 'L1' already defined", 1, true), msg)
        print("duplicate label prefix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("duplicate label prefix ok"));
}

/// Real Lua's `new_localvar` (`lparser.c`) rejects a second `<close>`
/// variable in the same `local` statement ("multiple to-be-closed variables
/// in local list") - only one to-be-closed slot is tracked per declaration.
/// Sol's `parser.rs` parsed each name's `<const>`/`<close>` attribute (either
/// per-name, or via a shared leading `local <attrib> a, b, ...` form) without
/// ever checking for more than one `<close>` among the names, silently
/// accepting `local <close> a, b` (or `local a <close>, b <close>`) instead
/// of rejecting it.
#[test]
fn a_local_statement_with_more_than_one_close_attribute_is_rejected() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_multi_close_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok, msg = load("local <close> a, b\n")
        assert(not ok and string.find(msg, "multiple to-be-closed", 1, true), msg)
        print("multiple close rejected ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("multiple close rejected ok"));
}

/// `lua-5.5.1-tests/errors.lua`'s "error lines in stack overflow" test:
/// `xpcall(g, debug.traceback, 1)` (where `g` calls a recursive `auxy` that
/// overflows the call-depth budget) must produce a `debug.traceback` string
/// with one `"source:line:"`-shaped entry per discarded call-chain level -
/// real Lua's `luaL_traceback` walks every activation record between the
/// error site and the `xpcall` marker, printing innermost first. Sol's
/// stack-overflow error previously carried only a single, bare `"line {N}"`
/// entry (`dispatch.rs`'s `PushClosure` overflow arm), with no chunk-name/
/// colon-delimited shape at all, so the corpus's own `string.match(line,
/// ":(%d+):")` parser (which expects real Lua's format) never matched
/// anything. Fixed two ways: `LuaError::at`'s call sites now format each
/// frame as `{short_src}:{line}:` (via the new `traceback_frame_label`
/// helper, reusing `runtime_error_prefix`'s `chunk_sources` lookup) instead
/// of a bare `"line {N}"`, and `unwind_error_to_marker`'s frame-discarding
/// walk now records *every* discarded Lua frame's own paused line (via the
/// new `LuaError::at_outer_frame`, which prepends rather than appends so the
/// existing innermost-frame entry still prints first after the shared
/// `.rev()`), not just the single innermost raise site.
#[test]
fn a_stack_overflows_traceback_has_one_source_line_entry_per_discarded_call_frame() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_stack_overflow_traceback_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local l = debug.getinfo(1, "l").currentline + 1
        local function auxy() auxy() end
        local l1
        local function g(x)
          l1 = debug.getinfo(x, "l").currentline + 2
          collectgarbage("stop")
          auxy()
          collectgarbage("restart")
        end
        local _, stackmsg = xpcall(g, debug.traceback, 1)
        local stack = {}
        for line in string.gmatch(stackmsg, "[^\n]*") do
          local curr = string.match(line, ":(%d+):")
          if curr then table.insert(stack, tonumber(curr)) end
        end
        local i = 1
        while stack[i] ~= l1 do
          assert(stack[i] == l, "unexpected line at position " .. i)
          i = i + 1
        end
        assert(i > 15, "too few stack traceback entries: " .. i)
        print("stack overflow traceback lines ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("stack overflow traceback lines ok"));
}

/// `os.exit` must flush whatever `print`/`io.write` output was already
/// buffered before terminating the process - real Lua writes each call
/// straight to stdout as it happens, so anything printed before `os.exit`
/// has already reached the terminal by the time the process dies. Sol
/// instead buffers a whole run's `print`/`io.write` output in memory
/// (`LuaRuntime::output`) and only flushes it to real stdout once the
/// dynamic-tier call returns normally (`write_lua_run`) or errors
/// (`report_lua_error`) in `main.rs`. `NativeFunction::OsExit` previously
/// called `std::process::exit` directly, bypassing both of those flush
/// points and silently discarding every buffered `print` - a script's
/// entire output would vanish the moment it called `os.exit`, even
/// `os.exit(0)` after ordinary `print` calls with no error involved.
#[test]
fn os_exit_flushes_buffered_output_before_terminating() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_os_exit_flush_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        print("before exit")
        os.exit(0)
        print("never reached")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("before exit"), "{stdout}");
    assert!(!stdout.contains("never reached"), "{stdout}");
}

/// Real Lua's `liolib.c` installs the same `f_gc` C function under both
/// `__gc` and `__close` on the file-handle metatable (`LUA_FILEHANDLE`) - an
/// ordinary, directly Lua-callable function that argument-checks its
/// receiver like any other file method (`tolstream`'s `luaL_checkudata`), not
/// something only the collector/a `<close>` scope exit can invoke. Surfaced
/// by `lua-5.5.1-tests/errors.lua`'s "tests for field accesses after RK
/// limit" section (`checkmessage(s.."; local t = {}; t:bbb()", "field
/// 'bbb'")`'s preceding sibling, `getmetatable(io.stdin).__gc()`, run with no
/// arguments purely to observe this check). Previously Sol's `io.stdin`
/// metatable had no `__gc`/`__close` entry at all, so this failed with the
/// unrelated, generic "attempt to call a nil value (field '__gc')" instead.
#[test]
fn file_handle_metatable_exposes_a_callable_gc_and_close_with_a_filestar_argument_check() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_file_gc_argcheck_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local mt = getmetatable(io.stdin)
        assert(type(mt.__gc) == "function")
        assert(mt.__close == mt.__gc)

        local ok1, msg1 = pcall(mt.__gc)
        assert(not ok1 and string.find(msg1, "bad argument #1 to '__gc'", 1, true), msg1)
        assert(string.find(msg1, "FILE%* expected, got no value"), msg1)

        local ok2, msg2 = pcall(mt.__gc, {})
        assert(not ok2 and string.find(msg2, "FILE%* expected, got table"), msg2)

        -- a real FILE* receiver passes the check and no-ops (matching real
        -- Lua's own no-op outcome for the standard streams, whose `closef`
        -- is unset).
        assert(mt.__gc(io.stdin) == nil)

        print("file gc argcheck ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("file gc argcheck ok"));
}

/// Real Lua's `luaL_argerror` (`lauxlib.c`) treats a method call specially:
/// argument numbers exclude the implicit `self` (`arg--`), and if the
/// decremented number reaches 0 - the self argument itself failed the check -
/// the message becomes `"calling 'NAME' on bad self (EXTRAMSG)"` instead of
/// `"bad argument #N to 'NAME'"`. Surfaced by `lua-5.5.1-tests/errors.lua`'s
/// `checkmessage("aaa:sub()", "bad self")` and its siblings
/// `string.sub('a', {})`/`('a'):sub{}`, which check that the non-method forms
/// keep ordinary, un-renumbered argument counting. Fixed via
/// `checked_string`/`checked_integer` (`natives.rs`, used by
/// `NativeFunction::StringSub`) producing the base
/// `"bad argument #N to 'NAME'"` shape, plus `annotate_bad_argument_error`
/// (`dispatch.rs`) - a post-hoc rewrite alongside the existing
/// `annotate_call_error`/`annotate_index_error`, reusing `describe_register`'s
/// call-site resolution - that renumbers/rewords any such message when the
/// failing call was a method call.
#[test]
fn bad_argument_errors_on_a_method_calls_self_argument_use_reals_calling_on_bad_self_wording() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_bad_self_argcheck_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local aaa = {}
        setmetatable(aaa, {__index = string})

        local ok1, msg1 = pcall(function() return aaa:sub() end)
        assert(not ok1, "aaa:sub() should fail")
        assert(string.find(msg1, "calling 'sub' on bad self", 1, true), msg1)
        assert(string.find(msg1, "string expected, got table"), msg1)

        local ok2, msg2 = pcall(string.sub, 'a', {})
        assert(not ok2, "string.sub('a', {}) should fail")
        assert(string.find(msg2, "bad argument #2 to 'sub'", 1, true), msg2)

        local ok3, msg3 = pcall(function() return ('a'):sub{} end)
        assert(not ok3, "('a'):sub{} should fail")
        assert(string.find(msg3, "bad argument #1 to 'sub'", 1, true), msg3)

        print("bad self argcheck ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("bad self argcheck ok"));
}

/// Real Lua's `luaO_chunkid` (`lobject.c`) is the single function that
/// formats a chunk name for display, shared by `debug.getinfo`'s `short_src`
/// and `lua_load`'s syntax-error position prefix. It reserves
/// `LUA_IDSIZE = 60` bytes (one held back for the C string's `'\0'`) and
/// truncates each of the three chunk-name forms differently: `=name` keeps
/// the first 59 bytes verbatim with no ellipsis; `@path` keeps the last 56
/// bytes behind a `"..."` prefix so a long path's distinguishing tail stays
/// visible; a literal source chunk (e.g. from `load`) is wrapped as
/// `[string "..."]`, keeping the first line verbatim only if it (with no
/// embedded newline) is under a 45-byte budget, else truncating to that
/// budget and appending `"..."`. Sol previously had two independent,
/// diverging implementations of this - `LuaRuntime::short_src`
/// (`natives_debug.rs`) and `display_chunk_name` (`natives_load.rs`) - each
/// with its own off-by-one/wrong-threshold bugs. Fixed by rewriting
/// `short_src` to match `luaO_chunkid`'s exact truncation arithmetic and
/// making `display_chunk_name` delegate to it, so both call sites agree.
/// Surfaced by `lua-5.5.1-tests/errors.lua`'s `checksize` loop.
#[test]
fn chunk_name_truncation_matches_reals_luao_chunkid_for_equals_at_and_string_sources() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_chunk_name_truncation_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local f1 = load("return 1", "=" .. string.rep("x", 59))
        assert(debug.getinfo(f1, "S").short_src == string.rep("x", 59))
        local f2 = load("return 1", "=" .. string.rep("x", 65))
        assert(debug.getinfo(f2, "S").short_src == string.rep("x", 59))

        local f3 = load("return 1", "@" .. string.rep("y", 59))
        assert(debug.getinfo(f3, "S").short_src == string.rep("y", 59))
        local long_path = "/a/" .. string.rep("y", 65)
        local f4 = load("return 1", "@" .. long_path)
        local expect4 = "..." .. string.sub(long_path, -56)
        assert(debug.getinfo(f4, "S").short_src == expect4, debug.getinfo(f4, "S").short_src)

        local short_line = string.rep("z", 40)
        local _, msg1 = load(short_line)
        assert(string.find(msg1, '[string "' .. short_line .. '"]', 1, true), msg1)

        local long_line = string.rep("z", 80)
        local _, msg2 = load(long_line)
        local expect_prefix = '[string "' .. string.rep("z", 45) .. '..."]'
        assert(string.find(msg2, expect_prefix, 1, true), msg2)

        print("chunk name truncation ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("chunk name truncation ok"));
}

/// Real Lua's `luaX_syntaxerror` (used by both the lexer and the parser)
/// always appends a `near '<token>'`/`near <eof>` suffix. The lexer's own
/// error sites already had this (`lexer.rs`'s `error_near`), but the
/// parser's two generic "unexpected symbol" fallbacks (an unparseable
/// statement or expression start) did not - `Spanned::lexeme` already
/// carries each token's exact source bytes, so the fix reads that directly
/// rather than needing a separate token-to-text table.
#[test]
fn parser_reports_the_lua_compatible_near_token_or_near_eof_suffix_for_an_unexpected_symbol() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_near_suffix_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local s1, msg1 = load("return 0xe-")
        assert(not s1 and string.find(msg1, "near <eof>"), msg1)

        local s2, msg2 = load("return )")
        assert(not s2 and string.find(msg2, "near '%)'"), msg2)

        local s3, msg3 = load("end")
        assert(not s3 and string.find(msg3, "near 'end'"), msg3)

        print("near suffix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("near suffix ok"));
}

/// `LuaValue::key()`/its companion `LuaKey` enum used to handle every table
/// key type except `Userdata`/`LightUserdata`, so indexing a table with a
/// userdata key (e.g. `[io.stdin] = ...`, a `FILE*` handle) fell through to
/// the generic "table index has an unsupported type" catch-all instead of
/// keying by the userdata's object identity the way `CanonicalTable` already
/// does - surfaced by `nextvar.lua`'s "testing next with all kinds of keys"
/// section. `debug.upvalueid` values (`LightUserdata`) hit the same gap.
#[test]
fn a_userdata_or_light_userdata_value_can_be_used_as_a_table_key() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_userdata_key_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local t = {}
        t[io.stdin] = "stdin"
        t[io.stdout] = "stdout"
        assert(t[io.stdin] == "stdin")
        assert(t[io.stdout] == "stdout")
        assert(next(t) ~= nil)

        local function outer()
            local x = 1
            local function inner() return x end
            return inner
        end
        local c1, c2 = outer(), outer()
        local u = {}
        u[debug.upvalueid(c1, 1)] = "c1"
        assert(u[debug.upvalueid(c1, 1)] == "c1")
        assert(u[debug.upvalueid(c2, 1)] == nil)

        print("userdata key ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("userdata key ok"));
}

#[test]
fn aot_supports_the_lua_string_subset() {
    ensure_staticlib();
    let output = std::env::temp_dir().join(format!("sol_lua55_strings_{}", std::process::id()));
    let source = Path::new(FIXTURES).join("native/strings.lua");
    let build = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args([
            "build",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&output).output().unwrap();
    std::fs::remove_file(&output).ok();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "true");
}

#[test]
fn cli_accepts_lua_and_sol_sources() {
    let lua = Path::new(FIXTURES).join("native/operators.lua");
    let sol = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extension_probe.sol");
    for (source, expected) in [(lua, "true"), (sol, "42")] {
        let output = Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", source.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }
}

#[test]
fn annotation_free_lua_and_sol_use_the_same_generic_runtime_semantics() {
    let directory = std::env::temp_dir();
    let stem = format!("sol_unified_generic_{}", std::process::id());
    let lua = directory.join(format!("{stem}.lua"));
    let sol = directory.join(format!("{stem}.sol"));
    let source = b"function main() return 5 / 2 end";
    std::fs::write(&lua, source).unwrap();
    std::fs::write(&sol, source).unwrap();

    let run = |path: &Path| {
        Command::new(env!("CARGO_BIN_EXE_sol"))
            .args(["run", path.to_str().unwrap()])
            .output()
            .unwrap()
    };
    let lua_output = run(&lua);
    let sol_output = run(&sol);
    std::fs::remove_file(lua).ok();
    std::fs::remove_file(sol).ok();

    assert!(lua_output.status.success());
    assert!(sol_output.status.success());
    assert_eq!(lua_output.stdout, sol_output.stdout);
    assert_eq!(String::from_utf8_lossy(&lua_output.stdout).trim(), "2.5");
}

#[test]
fn annotation_free_lua_is_unchanged_across_type_policies() {
    let path = std::env::temp_dir().join(format!("sol_type_policy_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        b"local total = 0 for i = 1, 4 do total = total + i end local item = { value = total } return item.value",
    )
    .unwrap();
    let run = |policy: &str, explain: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sol"));
        command.args(["run", "--type-policy", policy]);
        if explain {
            command.arg("--explain-types");
        }
        command.arg(&path).output().unwrap()
    };
    let off = run("off", false);
    let infer = run("infer", true);
    let strict = run("strict", false);
    std::fs::remove_file(path).ok();
    for output in [&off, &infer, &strict] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "10");
    }
    assert_eq!(off.stdout, infer.stdout);
    assert_eq!(infer.stdout, strict.stdout);
    let explanation = String::from_utf8_lossy(&infer.stderr);
    assert!(explanation.contains("type-policy=infer"), "{explanation}");
    assert!(explanation.contains("numeric-for index"), "{explanation}");
    assert!(explanation.contains("field value"), "{explanation}");
}

#[test]
fn legacy_partition_path_remains_available_for_runtime_differential_checks() {
    let path = std::env::temp_dir().join(format!(
        "sol_runtime_differential_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        b"function main() local value = { answer = 42 } return value.answer end",
    )
    .unwrap();
    let run = |legacy: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sol"));
        command.args(["run", path.to_str().unwrap()]);
        if legacy {
            command.env("SOL_RUNTIME_PATH", "legacy-partition");
        }
        command.output().unwrap()
    };
    let unified = run(false);
    let legacy = run(true);
    std::fs::remove_file(path).ok();
    assert!(unified.status.success());
    assert!(legacy.status.success());
    assert_eq!(unified.stdout, legacy.stdout);
    assert_eq!(String::from_utf8_lossy(&unified.stdout).trim(), "42");
}

#[test]
fn cli_rejects_an_unknown_source_extension() {
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", "fixture.txt"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected .lua or .sol"));
}

/// M13 L4: a single dynamic construct (here, a table) in one top-level
/// function no longer forces every other function in the same `.lua` file
/// into the bytecode interpreter - a fully-typed helper is compiled
/// natively and called from dynamic code through the one-directional
/// bridge (`typeck::check_partitioned`, `lua_runtime::LuaValue::Native`).
#[test]
fn dynamic_lua_code_can_call_a_natively_typed_helper_function() {
    let path = std::env::temp_dir().join(format!("sol_lua55_bridge_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        function fib(n: i64): i64
            if n < 2 then
                return n
            end
            return fib(n - 1) + fib(n - 2)
        end

        function main()
            local t = {}
            t.value = fib(10)
            print(t.value)
            return 0
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "55\n0");
}

#[test]
fn dynamic_protected_call_catches_a_typed_boundary_error() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_bridge_pcall_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        function typed(value: i64): i64
            return value + 1
        end

        function main()
            local ok, message = pcall(function() return typed("wrong") end)
            print(ok, type(message))
            return 0
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "false\tstring\n0"
    );
}

#[test]
fn typed_main_calls_a_dynamic_function_through_the_unified_dispatcher() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_reverse_bridge_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        function dynamic_add_one(value: i64): i64
            local boxed = { value = value }
            print(boxed.value)
            return boxed.value + 1
        end

        function main(): i64
            return dynamic_add_one(41)
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .env("SOL_REQUIRE_UNIFIED_MIXED", "1")
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "41\n42");
}

#[test]
fn typed_to_dynamic_tail_call_keeps_the_generic_frame_depth_bounded() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_reverse_tail_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        function countdown(value: i64): i64
            local force_generic = { value = value }
            if force_generic.value == 0 then
                return 7
            end
            return countdown(force_generic.value - 1)
        end

        function main(): i64
            return countdown(5000)
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .env("SOL_REQUIRE_UNIFIED_MIXED", "1")
        .env("SOL_LUA_CALL_DEPTH_BUDGET", "8")
        .output()
        .unwrap();
    std::fs::remove_file(path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "7");
}

/// A dynamic-only function that's never called from `main` (or anything
/// `main` reaches) must not stop `main` itself from running through the
/// fast native/tiered-JIT path when `main`'s own body has no dynamic
/// construct.
#[test]
fn an_unreachable_dynamic_only_function_does_not_block_a_typed_main() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_unreachable_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        function unused()
            local t = {}
            t.x = 1
            return t.x
        end

        function main(): i64
            return 42
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

/// Only scalar (`i64`/`f64`/`bool`) parameters and returns can cross the
/// dynamic/native bridge - a dynamic call site targeting a natively-typed
/// function with an array parameter must fail with a clear, actionable
/// error instead of miscompiling or silently falling back.
#[test]
fn calling_a_natively_typed_function_with_a_non_scalar_signature_from_dynamic_code_is_a_clear_error(
) {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_bridge_reject_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        function sum_array(arr: Array<i64>): i64
            return arr[0]
        end

        function main()
            local t = {}
            print(sum_array(new_array_i64(1)))
            return 0
        end
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("sum_array"), "{stderr}");
    assert!(
        stderr.contains("cannot cross the runtime boundary"),
        "{stderr}"
    );
}

/// Regression test: calling a dynamic-only builtin like `os.time()` as the
/// very first construct in a `.lua` program must not fail the initial
/// whole-program typed compile attempt with a hard type error - typeck has
/// no static signature for these new stdlib functions, so it must classify
/// "unknown function" the same as its other dynamic-Lua-syntax triggers and
/// fall through to `sol run`'s partitioned dynamic-runtime path, where `os`
/// and `io` are enabled by default.
#[test]
fn cli_run_reaches_os_io_and_load_through_the_dynamic_runtime_fallback() {
    let path =
        std::env::temp_dir().join(format!("sol_lua55_os_io_load_{}.lua", std::process::id()));
    std::fs::write(
        &path,
        br#"
        local t = os.time()
        io.write("hello io\n")
        local chunk = load("return 1 + 2")
        print(type(t), chunk())
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello io"), "{stdout}");
    assert!(stdout.contains("number\t3"), "{stdout}");
}

/// `debug.getinfo(1).name`/`.namewhat`, read from inside the running function
/// itself, must reflect how the *caller* referred to it at the call site
/// (real Lua's `funcnamefromcode` in `ldebug.c`), not the callee's own
/// declared name - the same closure called through a local variable, a table
/// field, and a bare parameter register must report different `name`/
/// `namewhat` pairs each time, mirroring `lua-5.5.1-tests/db.lua` lines
/// 96-104.
#[test]
fn debug_getinfo_resolves_the_callers_call_site_name() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_call_site_name_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local function f()
            return debug.getinfo(1)
        end

        -- called through a local variable
        local a = f()
        assert(a.name == 'f' and a.namewhat == 'local', a.namewhat .. " " .. tostring(a.name))

        -- called through a table field
        local t = {f = f}
        local b = t.f()
        assert(b.name == 'f' and b.namewhat == 'field', b.namewhat .. " " .. tostring(b.name))

        -- called through a bare function parameter (no declared name at all);
        -- parens around the call disable Lua's tail-call optimization, so the
        -- caller's frame (and its call-site register) is still live to
        -- inspect - matching lua-5.5.1-tests/db.lua line 120's `(x('a', 'x'))`
        local function g(x) return (x()) end
        local c = g(f)
        assert(c.name == 'x' and c.namewhat == 'local', c.namewhat .. " " .. tostring(c.name))

        print("call site name ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("call site name ok"));
}

#[test]
fn debug_sethook_line_events_match_real_luas_lastline_convention() {
    // Mirrors `lua-5.5.1-tests/db.lua`'s `test(s, l)` helper: installs a
    // fresh `"l"`-mode hook *mid-frame* (after `f`'s own locals are already
    // running), then checks the exact sequence of lines it reports against
    // real Lua 5.5.1's oracle output for each construct. Regression coverage
    // for three related bugs found while chasing db.lua's first divergence:
    // (1) a freshly-installed hook must not fire a spurious event for the
    // line the frame is already on (stale `hook_last_line` bookkeeping);
    // (2) a synthetic control-flow instruction (an `if`/`while`/`repeat`'s
    // jump) must be tagged with the line of the last real statement
    // compiled so far, not the enclosing statement's own start line
    // (matches real Lua's `lastline`, since `lcode.c`'s `savelineinfo`
    // always uses it, never a construct's opening keyword line); (3) a
    // closure isn't observably created until its whole body has been
    // parsed, so `local function`/`function`/a function expression must
    // report the closing `end`'s line, not the declaration's own line.
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_sethook_lastline_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local function test(s, expected)
            collectgarbage()
            local function f(event, line)
                assert(event == 'line')
                local want = table.remove(expected, 1)
                assert(want == line, "wrong trace!! got " .. line .. " expected " .. tostring(want))
            end
            -- Real Lua's db.lua keeps these three calls on one line, so
            -- the hook's own installation doesn't itself cross a line
            -- boundary in the calling frame before `load(s)()` runs.
            debug.sethook(f, "l"); load(s)(); debug.sethook()
            assert(#expected == 0)
        end

        -- if/else: the else-skip jump must carry the then-branch's last
        -- line (4), not the `if`'s own line (1).
        test([[if
math.sin(1)
then
  a=1
else
  a=2
end
]], {2, 4, 7})

        -- while: the back-edge jump must carry the body's last line (3),
        -- not the `while`'s own line (2).
        test([[local i = 1
while i < 3 do
  i = i + 1
end
]], {1, 2, 3, 2, 3, 2, 4})

        -- repeat: the `until` test's jump must carry the condition's own
        -- line (4), not the `repeat`'s own line (2).
        test([[local i = 1
repeat
  i = i + 1
until i >= 3
]], {1, 3, 4, 3, 4})

        -- local function: the closure isn't created until its `end`
        -- (line 2), so no event fires for line 1 at all.
        test([[
local function foo()
end
foo()
A = 1
A = 2
A = 3
]], {2, 3, 2, 4, 5, 6})
        _G.A = nil

        print("sethook lastline ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("sethook lastline ok"));
}

/// Real Lua's `luaG_errormsg` converts a thrown `nil` error object to the
/// literal string `"<no error object>"`. This applies not only to a bare
/// `error(nil)` (`NativeFunction::Error`'s own `LuaValue::Nil` arm) but also
/// to `assert(condition, nil)`, whose message is *explicitly* `nil` - as
/// opposed to omitted, which gets the different default `"assertion
/// failed!"` message. `NativeFunction::Assert` previously let the generic
/// `Some(message)` arm re-raise an explicit `nil` verbatim instead of
/// applying this conversion.
#[test]
fn assert_with_an_explicit_nil_message_reports_no_error_object() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_assert_nil_message_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local ok, msg, kind = pcall(assert, nil, nil)
        assert(not ok and msg == "<no error object>" and kind == nil)
        print("assert nil message ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("assert nil message ok"));
}

/// `lua-5.5.1-tests/errors.lua`'s `checksyntax` helper asserts every
/// `load()` syntax error ends in real Lua's `near '<token>'`/`near <eof>`
/// suffix (`luaX_syntaxerror`/`txtToken`, `llex.c`). Two of the parser's own
/// primitives, `expect`/`expect_ident`, previously built their error
/// messages with no such suffix at all (unlike the parser's "unexpected
/// symbol" fallback, which already had one), and the lexer's invalid-byte
/// error omitted it too. These cases mirror `errors.lua`'s own
/// `checksyntax` call sites for each of those three gaps.
#[test]
fn checksyntax_style_errors_report_the_lua_compatible_near_suffix() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_checksyntax_near_suffix_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        local function doit(s)
          local f, msg = load(s)
          if not f then return msg end
          local cond, msg = pcall(f)
          return (not cond) and msg
        end

        -- `expect(&Token::Eq)` in a `for` numeric-loop header.
        assert(string.find(doit("for >> do end"), "near '>>'$"))
        -- `expect_ident` after a struct-like assignment target.
        assert(string.find(doit("syntax error"), "near 'error'$"))
        -- The lexer's own invalid-byte error, decimal-escaped like real
        -- Lua's `txtToken` renders any non-printable single-byte token.
        assert(string.find(doit("a\1a = 1"), "near '<\\1>'$"))
        assert(string.find(doit("\255a = 1"), "near '<\\255>'$"))

        print("checksyntax near suffix ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("checksyntax near suffix ok"));
}

/// An uncaught error with no enclosing `pcall`/`xpcall` anywhere on the call
/// stack must still carry every discarded frame's position in its
/// traceback, exactly like one caught by `xpcall(f, debug.traceback)` does -
/// real Lua's own `lua` CLI wraps the whole script in its own top-level
/// protected call with a `msghandler` that calls `luaL_traceback`, so a
/// plain uncaught script error shows the full call chain too.
/// `unwind_error_to_marker` previously only recorded a discarded frame's
/// position while walking *toward a found* `pcall`/`xpcall` marker; when no
/// marker exists anywhere on the stack, it returned immediately without
/// visiting any frame, silently losing every position entry except the one
/// the innermost erroring frame's own `dispatch_step` `Err` arm added
/// (`interp::tests::specialized_bytecode_calls_dynamic_lua_through_the_semantic_slot`
/// caught this same gap from the specialized-bytecode adapter's own call
/// path).
#[test]
fn an_uncaught_error_with_no_enclosing_pcall_still_records_every_frames_position() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_uncaught_traceback_no_marker_{}.lua",
        std::process::id()
    ));
    std::fs::write(
        &path,
        br#"
        function inner()
          error("boom")
        end
        function outer()
          inner()
        end
        outer()
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(":3:"), "{stderr}");
    assert!(stderr.contains(":6:"), "{stderr}");
    assert!(stderr.contains(":8:"), "{stderr}");
}

#[test]
fn pcall_catching_a_plain_runtime_error_does_not_leak_call_depth_budget() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_pcall_runtime_error_no_depth_leak_{}.lua",
        std::process::id()
    ));
    // A directly-raised, non-nested-call runtime error (`4 + nil` inside the
    // frame's own bytecode, no intervening call) that a pcall catches must
    // release the erroring frame's own call_depth charge exactly like a
    // normal return does - `maxdepth()` probes the live recursion budget
    // before and after 200 such catches, and the two must match.
    std::fs::write(
        &path,
        br#"
        local function maxdepth()
          local function f(n)
            local ok, res = pcall(f, n + 1)
            if ok then return res end
            return n
          end
          return f(0)
        end

        local before = maxdepth()
        for i = 1, 200 do
          pcall(function() return 4 + nil end)
        end
        local after = maxdepth()
        assert(before == after, "call_depth leaked: before=" .. before .. " after=" .. after)
        print("pcall runtime error depth ok")
    "#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("pcall runtime error depth ok"));
}

#[test]
fn a_function_with_too_many_local_variables_fails_to_compile() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_too_many_locals_{}.lua",
        std::process::id()
    ));
    // Real Lua caps a function's live local-variable count at MAXVARS (200,
    // `lparser.c`), checked once per variable as it enters scope. A single
    // `local a,a,...` declaration list past that count must fail to compile
    // rather than silently succeed, matching `errors.lua`'s bisection, whose
    // `checkerr`-style helpers only require the message to contain "too many".
    std::fs::write(
        &path,
        format!(
            "local ok, err = load(\"local a{} = 1\")\n\
             assert(not ok, \"expected 201 local variables to fail to compile\")\n\
             assert(string.find(err, \"too many\"), \"unexpected message: \" .. tostring(err))\n\
             print(\"too many locals rejected\")\n",
            ",a".repeat(200)
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("too many locals rejected"));
}

#[test]
fn a_function_using_too_many_registers_fails_to_compile() {
    let path = std::env::temp_dir().join(format!(
        "sol_lua55_too_many_registers_{}.lua",
        std::process::id()
    ));
    // Real Lua caps a function's register-stack window at MAX_FSTACK (255,
    // `lcode.c`'s `luaK_checkstack`). A call with enough arguments to need
    // more than 255 live registers (each argument expression needs its own
    // register slot leading into the call) must fail to compile rather than
    // silently succeed, matching `errors.lua`'s bisection: `checkmessage`
    // there asserts on the literal substring "too many registers" for
    // `f(x,x,...,x)` with 261 arguments.
    std::fs::write(
        &path,
        format!(
            "local ok, err = load(\"local function f(...) end; f(x{})\")\n\
             assert(not ok, \"expected 261 call arguments to fail to compile\")\n\
             assert(string.find(err, \"too many registers\"), \"unexpected message: \" .. tostring(err))\n\
             print(\"too many registers rejected\")\n",
            ",x".repeat(260)
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sol"))
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("too many registers rejected"));
}
