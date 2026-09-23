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
    assert_eq!(lexical, "line 2, column 3: unexpected character [ELEX001]");

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
    let path =
        std::env::temp_dir().join(format!("sol_lua55_shebang_{}.lua", std::process::id()));
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
    let path =
        std::env::temp_dir().join(format!("sol_lua55_bnot_type_{}.lua", std::process::id()));
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
