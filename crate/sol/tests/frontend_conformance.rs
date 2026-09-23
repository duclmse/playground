use std::path::{Path, PathBuf};

use sol::lexer::Token;
use sol::parser::{parse_document_with_config, parse_with_config, LanguageConfig, TypePolicy};

fn lua_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            lua_files(&path, out);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("lua") {
            out.push(path);
        }
    }
}

#[test]
fn every_accepted_lua_fixture_has_the_same_sol_ast() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest.join("../..");
    let mut files = Vec::new();
    for root in [
        manifest.join("tests/fixtures/lua55"),
        repository.join("lua-5.5.1-tests"),
        repository.join("conformance/fixtures"),
        repository.join("benchmarks"),
        repository.join("crates/vm/tests/scripts"),
        repository.join("crates/dap-server/tests/fixtures"),
    ] {
        if root.is_dir() {
            lua_files(&root, &mut files);
        }
    }
    files.sort();

    let mut accepted = 0;
    for path in files {
        let source = std::fs::read(&path).unwrap();
        let tokens = match sol::lexer::lex_bytes(&source) {
            Ok(tokens) => tokens,
            Err(_) => continue,
        };
        let lua = match parse_with_config(tokens.clone(), LanguageConfig::LUA) {
            Ok(program) => program,
            Err(_) => continue,
        };
        accepted += 1;
        let sol = parse_with_config(tokens, LanguageConfig::SOL).unwrap_or_else(|error| {
            panic!("{} parsed as Lua but not Sol: {error}", path.display())
        });
        assert_eq!(lua, sol, "semantic AST fork for {}", path.display());
    }
    assert!(
        accepted >= 50,
        "only {accepted} Lua fixtures reached the differential gate"
    );
}

#[test]
fn contextual_sol_words_remain_lua_identifiers() {
    let source = b"fn = 1\nstruct = fn\nextern = struct\nimport = extern\nexport = import\ntype = export\nreturn type\n";
    let tokens = sol::lexer::lex_bytes(source).unwrap();
    for token in &tokens {
        if matches!(
            token.lexeme.as_slice(),
            b"fn" | b"struct" | b"extern" | b"import" | b"export" | b"type"
        ) {
            assert!(matches!(token.token, Token::Ident(_)), "{:?}", token);
        }
    }
    let lua = parse_with_config(tokens.clone(), LanguageConfig::LUA).unwrap();
    let sol = parse_with_config(tokens, LanguageConfig::SOL).unwrap();
    assert_eq!(lua, sol);
}

#[test]
fn newline_before_table_call_sugar_keeps_lua_semantics() {
    let source = b"local result = consume\n{ value = 42 }\nreturn result\n";
    let tokens = sol::lexer::lex_bytes(source).unwrap();
    let lua = parse_with_config(tokens.clone(), LanguageConfig::LUA).unwrap();
    let sol = parse_with_config(tokens, LanguageConfig::SOL).unwrap();
    assert_eq!(lua, sol);
}

#[test]
fn lossless_frontend_retains_exact_lexemes_and_byte_spans() {
    let source = b"local value = 0x2a\nreturn value\n";
    let tokens = sol::lexer::lex_bytes(source).unwrap();
    let document = parse_document_with_config(tokens, LanguageConfig::SOL).unwrap();
    for token in &document.tokens {
        assert_eq!(token.lexeme, source[token.span.start..token.span.end]);
    }
    let numeral = document
        .tokens
        .iter()
        .find(|token| matches!(token.token, Token::IntLit(42)))
        .unwrap();
    assert_eq!(numeral.lexeme, b"0x2a");
}

#[test]
fn invalid_annotations_report_the_offending_byte_location() {
    let tokens = sol::lexer::lex("local value: = 1").unwrap();
    let error = parse_with_config(tokens, LanguageConfig::SOL).unwrap_err();
    assert!(error.starts_with("line 1, column 14:"), "{error}");
    assert!(error.ends_with("[EPARSE001]"), "{error}");
}

#[test]
fn type_policy_does_not_change_parsing() {
    let source = b"local value: i64 = 42\nreturn value\n";
    let tokens = sol::lexer::lex_bytes(source).unwrap();
    let mut strict = LanguageConfig::SOL;
    strict.type_policy = TypePolicy::Strict;
    let inferred = parse_with_config(tokens.clone(), LanguageConfig::SOL).unwrap();
    let strict = parse_with_config(tokens, strict).unwrap();
    assert_eq!(inferred, strict);
}
