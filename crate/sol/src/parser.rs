// Hand-written recursive-descent + Pratt-style expression parser (not a
// parser-generator, for error-message/recovery control).

use crate::ast::*;
use crate::diagnostic::Diagnostic;
use crate::lexer::{Spanned, Token};
use std::collections::HashSet;

type ParamList = (Vec<(String, TypeName)>, Vec<bool>, bool, Option<String>);

pub fn parse(tokens: Vec<Spanned>) -> Result<Program, String> {
    parse_with_config(tokens, LanguageConfig::SOL)
}

pub fn parse_lua(tokens: Vec<Spanned>) -> Result<Program, String> {
    parse_with_config(tokens, LanguageConfig::LUA)
}

pub fn parse_with_mode(tokens: Vec<Spanned>, mode: SourceMode) -> Result<Program, String> {
    parse_with_config(tokens, mode.into())
}

/// Compatibility selector for callers that only know the source extension.
/// New frontend integrations should use [`LanguageConfig`] so syntax and type
/// policy remain independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    Sol,
    Lua,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Lua55,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypePolicy {
    Dynamic,
    Infer,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageConfig {
    pub dialect: Dialect,
    pub sol_extensions: bool,
    pub type_policy: TypePolicy,
}

impl LanguageConfig {
    pub const LUA: Self = Self {
        dialect: Dialect::Lua55,
        sol_extensions: false,
        type_policy: TypePolicy::Dynamic,
    };

    pub const SOL: Self = Self {
        dialect: Dialect::Lua55,
        sol_extensions: true,
        type_policy: TypePolicy::Infer,
    };
}

impl From<SourceMode> for LanguageConfig {
    fn from(mode: SourceMode) -> Self {
        match mode {
            SourceMode::Sol => Self::SOL,
            SourceMode::Lua => Self::LUA,
        }
    }
}

pub fn parse_with_config(tokens: Vec<Spanned>, config: LanguageConfig) -> Result<Program, String> {
    Parser {
        tokens,
        pos: 0,
        config,
        vararg_allowed: false,
        generated_structs: Vec::new(),
        known_structs: HashSet::new(),
        imported_modules: HashSet::new(),
    }
    .parse_program()
}

/// Lossless frontend result used by tools that need both semantic structure
/// and exact source spelling. The AST is intentionally independent from the
/// source extension; `tokens` retain byte spans and lexemes.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProgram {
    pub ast: Program,
    pub tokens: Vec<Spanned>,
    pub config: LanguageConfig,
}

pub fn parse_document_with_config(
    tokens: Vec<Spanned>,
    config: LanguageConfig,
) -> Result<ParsedProgram, String> {
    let ast = parse_with_config(tokens.clone(), config)?;
    Ok(ParsedProgram {
        ast,
        tokens,
        config,
    })
}

/// Conservatively determines whether `function`'s body might reference any
/// name in `names` as a free variable. This intentionally does NOT track
/// shadowing (a nested `local x`/parameter named `x` still counts as a
/// reference to `x` if `names` contains `x`): a false positive only costs a
/// top-level function its (optional) hoisted/AOT-eligible representation,
/// falling back to the always-correct in-order `Stmt::GlobalFunction` path,
/// while a false negative would hoist a function that actually needs to
/// capture a chunk-scope local as an upvalue, silently breaking it (the
/// hoisted "prebound top-level declaration" representation has no enclosing
/// scope and can never capture anything). Same "costs a few extra registers/
/// eligibility, never correctness" tradeoff as `retired_floor` in
/// `lua_bytecode/func_state.rs`.
fn function_references_any_name(function: &Function, names: &HashSet<String>) -> bool {
    function
        .body
        .iter()
        .any(|stmt| stmt_references_any_name(stmt, names))
}

fn block_references_any_name(block: &Block, names: &HashSet<String>) -> bool {
    block
        .iter()
        .any(|stmt| stmt_references_any_name(stmt, names))
}

fn stmt_references_any_name(stmt: &Stmt, names: &HashSet<String>) -> bool {
    match stmt {
        Stmt::Global { values, .. } => values.iter().any(|e| expr_references_any_name(e, names)),
        Stmt::GlobalFunction(f) | Stmt::LocalFunction(f) => function_references_any_name(f, names),
        Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => false,
        Stmt::MultiLocal { values, .. } => {
            values.iter().any(|e| expr_references_any_name(e, names))
        }
        Stmt::MultiAssign {
            targets, values, ..
        } => {
            targets
                .iter()
                .any(|t| assign_target_references_any_name(t, names))
                || values.iter().any(|e| expr_references_any_name(e, names))
        }
        Stmt::Block(block) => block_references_any_name(block, names),
        Stmt::Repeat { body, cond, .. } => {
            block_references_any_name(body, names) || expr_references_any_name(cond, names)
        }
        Stmt::Expr(e) => expr_references_any_name(e, names),
        Stmt::Local { value, .. } => expr_references_any_name(value, names),
        Stmt::Assign { target, value, .. } => {
            assign_target_references_any_name(target, names)
                || expr_references_any_name(value, names)
        }
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr_references_any_name(cond, names)
                || block_references_any_name(then_block, names)
                || else_block
                    .as_ref()
                    .is_some_and(|b| block_references_any_name(b, names))
        }
        Stmt::While { cond, body, .. } => {
            expr_references_any_name(cond, names) || block_references_any_name(body, names)
        }
        Stmt::NumericFor {
            start,
            stop,
            step,
            body,
            ..
        } => {
            expr_references_any_name(start, names)
                || expr_references_any_name(stop, names)
                || step
                    .as_ref()
                    .is_some_and(|e| expr_references_any_name(e, names))
                || block_references_any_name(body, names)
        }
        Stmt::GenericFor {
            iterators, body, ..
        } => {
            iterators.iter().any(|e| expr_references_any_name(e, names))
                || block_references_any_name(body, names)
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .is_some_and(|e| expr_references_any_name(e, names)),
        Stmt::MultiReturn { values, .. } => {
            values.iter().any(|e| expr_references_any_name(e, names))
        }
    }
}

fn assign_target_references_any_name(target: &AssignTarget, names: &HashSet<String>) -> bool {
    match target {
        AssignTarget::Name(name) => names.contains(name),
        AssignTarget::Index(base, idx) => {
            expr_references_any_name(base, names) || expr_references_any_name(idx, names)
        }
        AssignTarget::Field(base, _) => expr_references_any_name(base, names),
    }
}

fn expr_references_any_name(expr: &Expr, names: &HashSet<String>) -> bool {
    match &expr.kind {
        ExprKind::Table(fields) => fields.iter().any(|f| match f {
            TableField::Value(e) => expr_references_any_name(e, names),
            TableField::Named(_, e) => expr_references_any_name(e, names),
            TableField::Key(k, v) => {
                expr_references_any_name(k, names) || expr_references_any_name(v, names)
            }
        }),
        ExprKind::Function(f) => function_references_any_name(f, names),
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_) => false,
        ExprKind::Name(name) => names.contains(name),
        ExprKind::Unary(_, e) => expr_references_any_name(e, names),
        ExprKind::Binary(_, l, r) => {
            expr_references_any_name(l, names) || expr_references_any_name(r, names)
        }
        ExprKind::Call(name, args) => {
            names.contains(name) || args.iter().any(|e| expr_references_any_name(e, names))
        }
        ExprKind::CallExpr(callee, args) => {
            expr_references_any_name(callee, names)
                || args.iter().any(|e| expr_references_any_name(e, names))
        }
        ExprKind::MethodCall(recv, _, args) => {
            expr_references_any_name(recv, names)
                || args.iter().any(|e| expr_references_any_name(e, names))
        }
        ExprKind::Index(base, idx) => {
            expr_references_any_name(base, names) || expr_references_any_name(idx, names)
        }
        ExprKind::Len(e) => expr_references_any_name(e, names),
        ExprKind::StructLiteral(_, fields) => fields
            .iter()
            .any(|(_, e)| expr_references_any_name(e, names)),
        ExprKind::Field(base, _) => expr_references_any_name(base, names),
        ExprKind::TypeTest(e, _) => expr_references_any_name(e, names),
        ExprKind::Cast(e, _) => expr_references_any_name(e, names),
        ExprKind::Paren(e) => expr_references_any_name(e, names),
    }
}

/// A local/global's optional `<const>`/`<close>` attribute.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Attrib {
    None,
    Const,
    Close,
}

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    config: LanguageConfig,
    vararg_allowed: bool,
    generated_structs: Vec<StructDef>,
    known_structs: HashSet<String>,
    imported_modules: HashSet<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|s| &s.token)
    }

    fn line(&self) -> u32 {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|s| s.line)
            .unwrap_or(0)
    }

    fn current_span(&self) -> crate::diagnostic::SourceSpan {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|token| token.span)
            .unwrap_or_else(|| crate::diagnostic::SourceSpan::new(0, 0, 1, 1))
    }

    fn previous_span(&self) -> crate::diagnostic::SourceSpan {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .map(|token| token.span)
            .unwrap_or_else(|| self.current_span())
    }

    fn error(&self, message: impl Into<String>) -> String {
        let span = self
            .tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|token| token.span)
            .unwrap_or_else(|| crate::diagnostic::SourceSpan::new(0, 0, 1, 1));
        Diagnostic::error(span, message)
            .with_code("EPARSE001")
            .to_string()
    }

    fn advance(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).map(|s| s.token.clone());
        self.pos += 1;
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        let span = self.tokens.get(self.pos).map(|token| token.span);
        match self.advance() {
            Some(ref t) if t == expected => Ok(()),
            Some(t) => Err(Diagnostic::error(
                span.expect("advanced token must have a span"),
                format!("expected {:?}, found {:?}", expected, t),
            )
            .with_code("EPARSE001")
            .to_string()),
            None => Err(self.error(format!("expected {:?}, found end of input", expected))),
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        let span = self.tokens.get(self.pos).map(|token| token.span);
        match self.advance() {
            Some(Token::Ident(name)) => Ok(name),
            Some(t) => Err(Diagnostic::error(
                span.expect("advanced token must have a span"),
                format!("expected identifier, found {:?}", t),
            )
            .with_code("EPARSE001")
            .to_string()),
            None => Err(self.error("expected identifier, found end of input")),
        }
    }

    fn check(&self, t: &Token) -> bool {
        self.peek() == Some(t)
    }

    fn eat(&mut self, t: &Token) -> bool {
        if self.check(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `fn` is a contextual Sol-only spelling of `function`. Keeping it out
    /// of the lexer preserves valid Lua programs that use `fn` as a name.
    fn check_function_keyword(&self) -> bool {
        self.check(&Token::Function)
            || (self.config.sol_extensions
                && matches!(self.peek(), Some(Token::Ident(name)) if name == "fn")
                && matches!(self.token_at(1), Some(Token::Ident(_))))
    }

    fn check_contextual(&self, keyword: &str) -> bool {
        self.config.sol_extensions
            && matches!(self.peek(), Some(Token::Ident(name)) if name == keyword)
    }

    fn token_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset).map(|token| &token.token)
    }

    fn check_named_declaration(&self, keyword: &str, trailer: &Token) -> bool {
        self.check_contextual(keyword)
            && matches!(self.token_at(1), Some(Token::Ident(_)))
            && self.token_at(2) == Some(trailer)
    }

    fn expect_contextual(&mut self, keyword: &str) -> Result<(), String> {
        if self.check_contextual(keyword) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(format!("expected '{keyword}'")))
        }
    }

    fn eat_function_keyword(&mut self) -> bool {
        if self.check_function_keyword() {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_function_keyword(&mut self) -> Result<(), String> {
        if self.eat_function_keyword() {
            Ok(())
        } else {
            Err(self.error("expected 'function' or 'fn'"))
        }
    }

    // -- top level ---------------------------------------------------

    fn parse_program(&mut self) -> Result<Program, String> {
        // A source file is itself a variadic Lua-compatible chunk (its
        // arguments are exposed through `...`, including chunks returned by
        // `load`). Sol keeps the same AST/runtime semantics because it is a
        // strict syntactic and typing superset, not a separate language core.
        let chunk_is_vararg = true;
        self.vararg_allowed = chunk_is_vararg;
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut aliases = Vec::new();
        let mut structs = Vec::new();
        let mut functions = Vec::new();
        let mut externs = Vec::new();
        let mut chunk = Vec::new();
        let mut chunk_locals = HashSet::new();
        let mut chunk_global_decl_seen = false;
        // Names of plain top-level `function NAME(...) end` declarations
        // that were forced in-order into `chunk` (for any reason - shadowing
        // a chunk local, following a `global` declaration, etc). Once a name
        // has gone in-order, every later redeclaration of that same name
        // must also go in-order: hoisted `functions` entries are always
        // compiled and bound before `chunk`'s statements run (see
        // `lua_runtime::init::load_in_globals`), so if only the later
        // redeclaration were hoisted it would run BEFORE the earlier one,
        // inverting real Lua's strict textual-order redefinition semantics.
        let mut chunk_function_names = HashSet::new();
        while self.peek().is_some() {
            let contextual = match self.peek() {
                Some(Token::Ident(name)) if self.config.sol_extensions => Some(name.as_str()),
                _ => None,
            };
            if contextual == Some("import") && matches!(self.token_at(1), Some(Token::Ident(_))) {
                let line = self.line();
                self.advance();
                let mut module = self.expect_ident()?;
                while self.eat(&Token::Dot) {
                    module.push('.');
                    module.push_str(&self.expect_ident()?);
                }
                self.imported_modules.insert(module.clone());
                imports.push(ImportDecl { module, line });
            } else if contextual == Some("export")
                && (matches!(self.token_at(1), Some(Token::Function))
                    || matches!((self.token_at(1), self.token_at(2)), (Some(Token::Ident(name)), Some(Token::Ident(_))) if name == "fn")
                    || matches!((self.token_at(1), self.token_at(2), self.token_at(3)), (Some(Token::Ident(keyword)), Some(Token::Ident(_)), Some(Token::LBrace)) if keyword == "struct")
                    || matches!((self.token_at(1), self.token_at(2), self.token_at(3)), (Some(Token::Ident(keyword)), Some(Token::Ident(_)), Some(Token::Eq)) if keyword == "type"))
            {
                self.advance();
                if self.check_function_keyword() {
                    let function = self.parse_function()?;
                    if exports.contains(&function.name) {
                        return Err(self.error(format!("duplicate export '{}'", function.name)));
                    }
                    exports.push(function.name.clone());
                    functions.push(function);
                } else if self.check_contextual("struct") {
                    let structure = self.parse_struct_def()?;
                    if exports.contains(&structure.name) {
                        return Err(self.error(format!("duplicate export '{}'", structure.name)));
                    }
                    exports.push(structure.name.clone());
                    structs.push(structure);
                } else if matches!(self.peek(), Some(Token::Ident(name)) if name == "type") {
                    let alias = self.parse_alias()?;
                    if exports.contains(&alias.name) {
                        return Err(self.error(format!("duplicate export '{}'", alias.name)));
                    }
                    exports.push(alias.name.clone());
                    aliases.push(alias);
                } else {
                    return Err(
                        self.error("export must precede a function, struct, or type declaration")
                    );
                }
            } else if self.check_named_declaration("type", &Token::Eq) {
                aliases.push(self.parse_alias()?);
            } else if self.check_named_declaration("struct", &Token::LBrace) {
                structs.push(self.parse_struct_def()?);
            } else if contextual == Some("extern")
                && (matches!(self.token_at(1), Some(Token::Function))
                    || matches!((self.token_at(1), self.token_at(2)), (Some(Token::Ident(name)), Some(Token::Ident(_))) if name == "fn"))
            {
                externs.push(self.parse_extern_function()?);
            } else if self.check_function_keyword()
                && !matches!(
                    self.tokens.get(self.pos + 1).map(|token| &token.token),
                    Some(Token::LParen)
                )
            {
                let function = self.parse_function()?;
                if function.name.contains('.') || function.name.contains(':') {
                    // `function t.f() end` / `function t:f() end` at chunk
                    // top level is sugar for a field assignment (`t.f = ...`),
                    // not a global-name declaration - it can't be hoisted
                    // ahead of the statement that creates `t` (e.g. a
                    // preceding `local t = {}`), so it stays a plain
                    // in-order chunk statement like any other assignment.
                    chunk.push(Stmt::GlobalFunction(function));
                } else if chunk_global_decl_seen
                    || functions
                        .iter()
                        .any(|existing| existing.name == function.name)
                    || chunk_locals.contains(&function.name)
                    || chunk_function_names.contains(&function.name)
                    || function_references_any_name(&function, &chunk_locals)
                {
                    // The first plain top-level declaration can use the
                    // compiler's ordinary prebound-function representation,
                    // but a declaration that resolves to an earlier local, or
                    // redeclares a global, is an executable Lua assignment and
                    // must happen at this exact point in the chunk. Hoisting it
                    // would either assign the wrong binding or make the final
                    // body visible before its declaration (notably in upstream
                    // pm.lua and vararg.lua). Once any `global` declaration
                    // has appeared earlier in this same chunk, every plain
                    // top-level `function NAME` must also stay in-order:
                    // it's an ordinary assignment that Lua 5.5's fuller
                    // `global`-declaration strict-checking (undeclared-name,
                    // `<const>`, collective, and `_ENV` checks) may apply to,
                    // and only in-order compilation sees that declaration
                    // state (see `goto.lua`'s `global none; function XX()
                    // end` and `global<const> foo; function foo` cases).
                    //
                    // The "prebound top-level declaration" representation
                    // compiles each function independently with no enclosing
                    // scope, so it can never capture a chunk-scope local as
                    // an upvalue - a plain top-level `function NAME(...) end`
                    // is just sugar for an ordinary assignment, the same as
                    // `local function NAME` already is, and real Lua's `.lua`
                    // dialect has no module-level-declaration concept that
                    // would justify hoisting it past a real local. So a
                    // function whose body might reference (as a free
                    // variable, ignoring shadowing - see
                    // `function_references_any_name`'s doc comment) a
                    // chunk-scope local declared earlier in this chunk must
                    // fall back to compiling in-order like any other
                    // statement - this applies uniformly to both dialects
                    // (the shared front end must parse identical `.lua`
                    // source to an identical AST regardless of `SourceMode`;
                    // see `tests/frontend_conformance.rs`). A function that
                    // provably can't see any prior chunk-local, though, is
                    // still safe to hoist, and doing so lets native
                    // (host-language) bindings substitute for a top-level Lua
                    // function by name (see
                    // `lua_runtime::init::run_in_globals`), which depends on
                    // every top-level function being reachable through the
                    // hoisted `functions` list rather than buried inside a
                    // synthesized `main`'s sequential body.
                    chunk_function_names.insert(function.name.clone());
                    chunk.push(Stmt::GlobalFunction(function));
                } else {
                    functions.push(function);
                }
            } else {
                let statement = self.parse_stmt()?;
                let is_return = matches!(statement, Stmt::Return { .. } | Stmt::MultiReturn { .. });
                match &statement {
                    Stmt::Local { name, .. } => {
                        chunk_locals.insert(name.clone());
                    }
                    Stmt::MultiLocal { names, .. } => {
                        chunk_locals.extend(names.iter().map(|(name, _, _, _)| name.clone()));
                    }
                    Stmt::LocalFunction(function) => {
                        chunk_locals.insert(function.name.clone());
                    }
                    Stmt::Global { .. } => {
                        chunk_global_decl_seen = true;
                    }
                    _ => {}
                }
                chunk.push(statement);
                if is_return {
                    // Same "return is only valid as a block's last statement"
                    // rule as `parse_block`, applied to the top-level chunk
                    // (itself a block) - `return;;`/`return 1 print()` must
                    // fail to compile, not silently accept trailing tokens.
                    self.eat(&Token::Semi);
                    if self.peek().is_some() {
                        return Err(self.error("'return' must be the last statement in a block"));
                    }
                    break;
                }
            }
        }
        let initializer = !chunk.is_empty();
        if initializer {
            if functions.iter().any(|f| f.name == "main") {
                return Err(
                    "top-level statements cannot be combined with an explicit main function".into(),
                );
            }
            functions.push(Function {
                name: "main".into(),
                source_file: None,
                params: vec![],
                param_annotations: vec![],
                vararg: chunk_is_vararg,
                vararg_name: None,
                return_type: None,
                body: chunk,
                line: 1,
                is_global_decl: false,
                source_span: match (self.tokens.first(), self.tokens.last()) {
                    (Some(first), Some(last)) => crate::diagnostic::SourceSpan::new(
                        first.span.start,
                        last.span.end,
                        first.span.line,
                        first.span.column,
                    ),
                    _ => crate::diagnostic::SourceSpan::new(0, 0, 1, 1),
                },
            });
        }
        structs.extend(std::mem::take(&mut self.generated_structs));
        Ok(Program {
            imports,
            exports,
            initializer,
            aliases,
            structs,
            functions,
            externs,
        })
    }

    fn parse_alias(&mut self) -> Result<AliasDef, String> {
        let line = self.line();
        match self.advance() {
            Some(Token::Ident(keyword)) if keyword == "type" => {}
            _ => return Err(self.error("expected 'type'")),
        }
        let name = self.expect_ident()?;
        self.expect(&Token::Eq)?;
        let target = self.parse_type()?;
        Ok(AliasDef { name, target, line })
    }

    fn parse_struct_def(&mut self) -> Result<StructDef, String> {
        let line = self.line();
        self.expect_contextual("struct")?;
        let name = self.expect_ident()?;
        self.expect(&Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.check(&Token::RBrace) {
            let fname = self.expect_ident()?;
            self.expect(&Token::Colon)?;
            let ty = self.parse_type()?;
            fields.push((fname, ty));
            if !self.eat(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RBrace)?;
        self.known_structs.insert(name.clone());
        Ok(StructDef { name, fields, line })
    }

    fn parse_function(&mut self) -> Result<Function, String> {
        let start = self.current_span();
        let line = start.line;
        self.expect_function_keyword()?;
        self.parse_function_after_keyword(line, start)
    }

    fn parse_function_after_keyword(
        &mut self,
        line: u32,
        start: crate::diagnostic::SourceSpan,
    ) -> Result<Function, String> {
        let name = self.expect_ident()?;
        let mut name = name;
        while self.eat(&Token::Dot) {
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        let method = if self.eat(&Token::Colon) {
            name.push(':');
            name.push_str(&self.expect_ident()?);
            true
        } else {
            false
        };
        let (mut params, mut param_annotations, vararg, vararg_name) = self.parse_param_list()?;
        if method {
            params.insert(0, ("self".into(), TypeName::Any));
            param_annotations.insert(0, false);
        }
        let return_type = if self.eat(&Token::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_function_body(vararg)?;
        let end = self.previous_span();
        Ok(Function {
            name,
            source_file: None,
            params,
            param_annotations,
            vararg,
            vararg_name,
            return_type,
            body,
            line,
            is_global_decl: false,
            source_span: crate::diagnostic::SourceSpan::new(
                start.start,
                end.end,
                start.line,
                start.column,
            ),
        })
    }

    /// `extern function name(params): ReturnType` - no body, no `end`.
    fn parse_extern_function(&mut self) -> Result<ExternFunction, String> {
        let line = self.line();
        self.expect_contextual("extern")?;
        self.expect_function_keyword()?;
        let name = self.expect_ident()?;
        let (params, _, vararg, _) = self.parse_param_list()?;
        if vararg {
            return Err(self.error("extern functions cannot use Lua varargs"));
        }
        self.expect(&Token::Colon)?;
        let return_type = self.parse_type()?;
        Ok(ExternFunction {
            name,
            params,
            return_type,
            line,
        })
    }

    fn parse_param_list(&mut self) -> Result<ParamList, String> {
        self.expect(&Token::LParen)?;
        let mut params = Vec::new();
        let mut annotations = Vec::new();
        let mut vararg = false;
        let mut vararg_name = None;
        if !self.check(&Token::RParen) {
            loop {
                if self.eat(&Token::Vararg) {
                    vararg = true;
                    if matches!(self.peek(), Some(Token::Ident(_))) {
                        vararg_name = Some(self.expect_ident()?);
                    }
                    break;
                }
                let pname = self.expect_ident()?;
                let annotated = self.eat(&Token::Colon);
                let ty = if annotated {
                    self.parse_type()?
                } else {
                    TypeName::Any
                };
                params.push((pname, ty));
                annotations.push(annotated);
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
        }
        self.expect(&Token::RParen)?;
        Ok((params, annotations, vararg, vararg_name))
    }

    fn parse_function_body(&mut self, vararg: bool) -> Result<Block, String> {
        let previous = self.vararg_allowed;
        self.vararg_allowed = vararg;
        let body = self.parse_block(&[Token::End]);
        self.vararg_allowed = previous;
        let body = body?;
        self.expect(&Token::End)?;
        Ok(body)
    }

    fn parse_type(&mut self) -> Result<TypeName, String> {
        if self.eat(&Token::Nil) {
            return Ok(TypeName::Nil);
        }
        if self.eat(&Token::LBrace) {
            let line = self.line();
            let mut fields = Vec::new();
            while !self.check(&Token::RBrace) {
                let name = self.expect_ident()?;
                self.expect(&Token::Colon)?;
                fields.push((name, self.parse_type()?));
                if !self.eat(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RBrace)?;
            if fields.is_empty() {
                return Err(self.error("a structural record type needs at least one field"));
            }
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            if fields.windows(2).any(|pair| pair[0].0 == pair[1].0) {
                return Err(self.error("duplicate field in structural record type"));
            }
            let name = format!(
                "$record{{{}}}",
                fields
                    .iter()
                    .map(|(name, ty)| format!("{name}:{ty:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if !self
                .generated_structs
                .iter()
                .any(|record| record.name == name)
            {
                self.generated_structs.push(StructDef {
                    name: name.clone(),
                    fields,
                    line,
                });
            }
            return Ok(TypeName::Struct(name));
        }
        let mut name = self.expect_ident()?;
        while self.eat(&Token::Dot) {
            name.push('.');
            name.push_str(&self.expect_ident()?);
        }
        match name.as_str() {
            "fn" => {
                self.expect(&Token::LParen)?;
                let mut params = Vec::new();
                if !self.check(&Token::RParen) {
                    loop {
                        params.push(self.parse_type()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Token::RParen)?;
                if !self.eat(&Token::Arrow) {
                    self.expect(&Token::Colon)?;
                }
                let return_type = self.parse_type()?;
                Ok(TypeName::Function {
                    params,
                    return_type: Box::new(return_type),
                })
            }
            "i64" => Ok(TypeName::I64),
            "f64" => Ok(TypeName::F64),
            "bool" => Ok(TypeName::Bool),
            "string" => Ok(TypeName::String),
            // "any" is reserved only in type position, not a keyword token.
            "any" => Ok(TypeName::Any),
            "Array" => {
                self.expect(&Token::Lt)?;
                let inner = self.parse_type()?;
                self.expect(&Token::Gt)?;
                Ok(TypeName::Array(Box::new(inner)))
            }
            "Map" => {
                self.expect(&Token::Lt)?;
                let key = self.parse_type()?;
                self.expect(&Token::Comma)?;
                let value = self.parse_type()?;
                self.expect(&Token::Gt)?;
                Ok(TypeName::Map(Box::new(key), Box::new(value)))
            }
            // Anything else: assumed struct name, validated later by typeck.rs.
            other => Ok(TypeName::Struct(other.to_string())),
        }
    }

    // -- statements ---------------------------------------------------

    /// Parses statements until (not consuming) one of `terminators`.
    fn parse_block(&mut self, terminators: &[Token]) -> Result<Block, String> {
        let mut stmts = Vec::new();
        while !terminators.iter().any(|t| self.check(t)) {
            if self.peek().is_none() {
                return Err("unexpected end of input inside a block".to_string());
            }
            let stmt = self.parse_stmt()?;
            let is_return = matches!(stmt, Stmt::Return { .. } | Stmt::MultiReturn { .. });
            stmts.push(stmt);
            if is_return {
                // Lua's grammar allows `return` only as a block's very last
                // statement, optionally followed by one `;` - not another
                // statement or a second `;` (`return;;` is a syntax error).
                self.eat(&Token::Semi);
                if !terminators.iter().any(|t| self.check(t)) {
                    return Err(self.error("'return' must be the last statement in a block"));
                }
                break;
            }
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        match self.peek() {
            Some(Token::Semi) => {
                self.advance();
                Ok(Stmt::Block(vec![]))
            }
            Some(Token::Do) => {
                self.advance();
                let body = self.parse_block(&[Token::End])?;
                self.expect(&Token::End)?;
                Ok(Stmt::Block(body))
            }
            Some(Token::Repeat) => {
                self.advance();
                let body = self.parse_block(&[Token::Until])?;
                self.expect(&Token::Until)?;
                let cond = self.parse_expr()?;
                Ok(Stmt::Repeat { body, cond, line })
            }
            Some(Token::Break) => {
                self.advance();
                Ok(Stmt::Break { line })
            }
            Some(Token::Goto) => {
                self.advance();
                Ok(Stmt::Goto {
                    name: self.expect_ident()?,
                    line,
                })
            }
            Some(Token::Label) => {
                self.advance();
                let name = self.expect_ident()?;
                self.expect(&Token::Label)?;
                Ok(Stmt::Label { name, line })
            }
            Some(Token::Local) => {
                self.advance();
                if self.eat_function_keyword() {
                    let start = self.previous_span();
                    return Ok(Stmt::LocalFunction(
                        self.parse_function_after_keyword(line, start)?,
                    ));
                }
                let prefix_attr = self.parse_attribute()?;
                let mut names = vec![];
                loop {
                    let name = self.expect_ident()?;
                    let attr = self.parse_attribute()?;
                    let constant = attr == Attrib::Const || prefix_attr == Attrib::Const;
                    let close = attr == Attrib::Close || prefix_attr == Attrib::Close;
                    if constant && close {
                        return Err(format!(
                            "line {line}: variable '{name}' cannot be both <const> and <close>"
                        ));
                    }
                    let ty = if self.eat(&Token::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    names.push((name, ty, constant, close));
                    if !self.eat(&Token::Comma) {
                        break;
                    }
                }
                let values = if self.eat(&Token::Eq) {
                    self.parse_values()?
                } else {
                    vec![]
                };
                if names.len() == 1 && values.len() <= 1 {
                    let (name, ty, constant, close) = names.remove(0);
                    let value = values.into_iter().next().unwrap_or(Expr {
                        kind: ExprKind::NilLit,
                        line,
                    });
                    Ok(Stmt::Local {
                        name,
                        ty,
                        constant,
                        close,
                        value,
                        line,
                    })
                } else {
                    Ok(Stmt::MultiLocal {
                        names,
                        values,
                        line,
                    })
                }
            }
            Some(Token::Global) => self.parse_global(),
            Some(Token::If) => self.parse_if(),
            Some(Token::While) => {
                self.advance();
                let cond = self.parse_expr()?;
                self.expect(&Token::Do)?;
                let body = self.parse_block(&[Token::End])?;
                self.expect(&Token::End)?;
                Ok(Stmt::While { cond, body, line })
            }
            Some(Token::For) => {
                self.advance();
                let first = self.expect_ident()?;
                if !self.eat(&Token::Eq) {
                    let mut vars = vec![first];
                    while self.eat(&Token::Comma) {
                        vars.push(self.expect_ident()?);
                    }
                    self.expect(&Token::In)?;
                    let iterators = self.parse_values()?;
                    self.expect(&Token::Do)?;
                    let body = self.parse_block(&[Token::End])?;
                    self.expect(&Token::End)?;
                    return Ok(Stmt::GenericFor {
                        vars,
                        iterators,
                        body,
                        line,
                    });
                }
                let var = first;
                let start = self.parse_expr()?;
                self.expect(&Token::Comma)?;
                let stop = self.parse_expr()?;
                let step = if self.eat(&Token::Comma) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                self.expect(&Token::Do)?;
                let body = self.parse_block(&[Token::End])?;
                self.expect(&Token::End)?;
                Ok(Stmt::NumericFor {
                    var,
                    start,
                    stop,
                    step,
                    body,
                    line,
                })
            }
            Some(Token::Return) => {
                self.advance();
                // No value iff followed directly by a block terminator.
                let value = if self.peek().is_none()
                    || self.check(&Token::End)
                    || self.check(&Token::Else)
                    || self.check(&Token::ElseIf)
                    || self.check(&Token::Until)
                    || self.check(&Token::RBrace)
                    || self.check(&Token::Semi)
                {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                if self.eat(&Token::Comma) {
                    let mut values = vec![value.expect("a comma follows a return expression")];
                    values.push(self.parse_expr()?);
                    while self.eat(&Token::Comma) {
                        values.push(self.parse_expr()?);
                    }
                    return Ok(Stmt::MultiReturn { values, line });
                }
                Ok(Stmt::Return { value, line })
            }
            Some(Token::Function) => {
                if matches!(
                    self.tokens.get(self.pos + 1).map(|token| &token.token),
                    Some(Token::LParen)
                ) {
                    self.parse_assign()
                } else {
                    Ok(Stmt::GlobalFunction(self.parse_function()?))
                }
            }
            Some(Token::Ident(_)) | Some(Token::LParen) => self.parse_assign(),
            // Real Lua's parser reports every "this token can't start a
            // statement/expression" error as "unexpected symbol near '...'"
            // (`lparser.c`'s `error_expected`/`luaX_syntaxerror`) - Lua 5.5's
            // own test corpus (`lua-5.5.1-tests/calls.lua`'s "test for
            // generic load" section) matches on that literal wording via
            // `string.find`, not just "a parse error occurred", so the
            // phrase itself is part of the compatibility contract, not
            // cosmetic.
            other => Err(format!(
                "line {}: unexpected symbol (unexpected token {:?}) at start of statement",
                line, other
            )),
        }
    }

    fn parse_global(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        self.expect(&Token::Global)?;
        let prefix_attr = self.parse_global_attribute()?;
        if self.eat(&Token::Function) {
            if prefix_attr {
                return Err(self.error("a global function declaration cannot have <const>"));
            }
            let start = self.previous_span();
            let mut function = self.parse_function_after_keyword(line, start)?;
            function.is_global_decl = true;
            return Ok(Stmt::GlobalFunction(function));
        }

        let mut names = Vec::new();
        if self.eat(&Token::Star) {
            names.push(GlobalBinding {
                name: GlobalName::All,
                constant: prefix_attr,
            });
        } else {
            let name = self.expect_ident()?;
            if name == "none" {
                names.push(GlobalBinding {
                    name: GlobalName::None,
                    constant: prefix_attr,
                });
            } else {
                names.push(GlobalBinding {
                    name: GlobalName::Name(name),
                    constant: self.parse_global_attribute()? || prefix_attr,
                });
                while self.eat(&Token::Comma) {
                    let name = self.expect_ident()?;
                    names.push(GlobalBinding {
                        name: GlobalName::Name(name),
                        constant: self.parse_global_attribute()? || prefix_attr,
                    });
                }
            }
        }
        let values = if self.eat(&Token::Eq) {
            self.parse_values()?
        } else {
            Vec::new()
        };
        Ok(Stmt::Global {
            names,
            values,
            line,
        })
    }

    /// `global`'s attribute slot only supports `<const>` - Lua has no
    /// `<close>` for globals (to-be-closed variables are strictly local).
    fn parse_global_attribute(&mut self) -> Result<bool, String> {
        match self.parse_attribute()? {
            Attrib::None => Ok(false),
            Attrib::Const => Ok(true),
            Attrib::Close => Err(format!(
                "line {}: global variables cannot be to-be-closed",
                self.line()
            )),
        }
    }

    fn parse_attribute(&mut self) -> Result<Attrib, String> {
        if !self.eat(&Token::Lt) {
            return Ok(Attrib::None);
        }
        let name = self.expect_ident()?;
        self.expect(&Token::Gt)?;
        match name.as_str() {
            "const" => Ok(Attrib::Const),
            "close" => Ok(Attrib::Close),
            _ => Err(format!("line {}: unknown attribute '{name}'", self.line())),
        }
    }

    fn parse_if(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        self.expect(&Token::If)?;
        let cond = self.parse_expr()?;
        self.expect(&Token::Then)?;
        let then_block = self.parse_block(&[Token::End, Token::Else, Token::ElseIf])?;
        let else_block = match self.peek() {
            // Desugar `elseif` into a nested `if` inside the `else` arm.
            Some(Token::ElseIf) => Some(vec![self.parse_if_elseif()?]),
            Some(Token::Else) => {
                self.advance();
                let block = self.parse_block(&[Token::End])?;
                self.expect(&Token::End)?;
                Some(block)
            }
            _ => {
                self.expect(&Token::End)?;
                None
            }
        };
        Ok(Stmt::If {
            cond,
            then_block,
            else_block,
            line,
        })
    }

    /// Like `parse_if`, but starting from an already-consumed `elseif`.
    fn parse_if_elseif(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        self.expect(&Token::ElseIf)?;
        let cond = self.parse_expr()?;
        self.expect(&Token::Then)?;
        let then_block = self.parse_block(&[Token::End, Token::Else, Token::ElseIf])?;
        let else_block = match self.peek() {
            Some(Token::ElseIf) => Some(vec![self.parse_if_elseif()?]),
            Some(Token::Else) => {
                self.advance();
                let block = self.parse_block(&[Token::End])?;
                self.expect(&Token::End)?;
                Some(block)
            }
            _ => {
                self.expect(&Token::End)?;
                None
            }
        };
        Ok(Stmt::If {
            cond,
            then_block,
            else_block,
            line,
        })
    }

    /// Parses the LHS as a postfix expression, then converts it to an
    /// `AssignTarget` - avoids duplicating postfix-chain parsing.
    fn parse_assign(&mut self) -> Result<Stmt, String> {
        let line = self.line();
        let target_expr = self.parse_postfix()?;
        if matches!(
            target_expr.kind,
            ExprKind::Call(..) | ExprKind::CallExpr(..) | ExprKind::MethodCall(..)
        ) && !self.check(&Token::Eq)
        {
            return Ok(Stmt::Expr(target_expr));
        }
        fn target(expr: Expr, line: u32) -> Result<AssignTarget, String> {
            match expr.kind {
                ExprKind::Name(name) => Ok(AssignTarget::Name(name)),
                ExprKind::Index(base, index) => Ok(AssignTarget::Index(*base, *index)),
                ExprKind::Field(base, field) => Ok(AssignTarget::Field(*base, field)),
                _ => Err(format!("line {line}: invalid assignment target")),
            }
        }
        let mut targets = vec![target(target_expr, line)?];
        while self.eat(&Token::Comma) {
            targets.push(target(self.parse_postfix()?, line)?);
        }
        self.expect(&Token::Eq)?;
        let mut values = self.parse_values()?;
        if targets.len() == 1 && values.len() == 1 {
            Ok(Stmt::Assign {
                target: targets.remove(0),
                value: values.remove(0),
                line,
            })
        } else {
            Ok(Stmt::MultiAssign {
                targets,
                values,
                line,
            })
        }
    }

    fn parse_values(&mut self) -> Result<Vec<Expr>, String> {
        let mut values = vec![self.parse_expr()?];
        while self.eat(&Token::Comma) {
            values.push(self.parse_expr()?);
        }
        Ok(values)
    }

    // -- expressions (Pratt, precedence climbing) ---------------------

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_precedence(0)
    }

    // Lua's precedence table: exponentiation is right associative and binds
    // more tightly than unary minus, including on its right-hand operand.
    fn parse_precedence(&mut self, min: u8) -> Result<Expr, String> {
        let line = self.line();
        let unary = match self.peek() {
            Some(Token::Minus) => Some(UnaryOp::Neg),
            Some(Token::Not) => Some(UnaryOp::Not),
            Some(Token::Tilde) => Some(UnaryOp::BitNot),
            _ => None,
        };
        let mut left = if let Some(op) = unary {
            self.advance();
            Expr {
                kind: ExprKind::Unary(op, Box::new(self.parse_precedence(11)?)),
                line,
            }
        } else if self.eat(&Token::Hash) {
            Expr {
                kind: ExprKind::Len(Box::new(self.parse_precedence(11)?)),
                line,
            }
        } else {
            self.parse_postfix()?
        };
        loop {
            let contextual = match self.peek() {
                Some(Token::Ident(name)) if self.config.sol_extensions => Some(name.as_str()),
                _ => None,
            };
            if contextual == Some("as") {
                if 11 < min {
                    break;
                }
                self.advance();
                let ty = self.parse_type()?;
                left = Expr {
                    kind: ExprKind::Cast(Box::new(left), ty),
                    line,
                };
                continue;
            }
            if contextual == Some("is") {
                if 3 < min {
                    break;
                }
                self.advance();
                let ty = self.parse_type()?;
                left = Expr {
                    kind: ExprKind::TypeTest(Box::new(left), ty),
                    line,
                };
                continue;
            }
            let (op, prec) = match self.peek() {
                Some(Token::Or) => (BinaryOp::Or, 1),
                Some(Token::And) => (BinaryOp::And, 2),
                Some(Token::EqEq) => (BinaryOp::Eq, 3),
                Some(Token::NotEq) => (BinaryOp::NotEq, 3),
                Some(Token::Lt) => (BinaryOp::Lt, 3),
                Some(Token::Le) => (BinaryOp::Le, 3),
                Some(Token::Gt) => (BinaryOp::Gt, 3),
                Some(Token::Ge) => (BinaryOp::Ge, 3),
                Some(Token::Pipe) => (BinaryOp::BitOr, 4),
                Some(Token::Tilde) => (BinaryOp::BitXor, 5),
                Some(Token::Amp) => (BinaryOp::BitAnd, 6),
                Some(Token::Shl) => (BinaryOp::Shl, 7),
                Some(Token::Shr) => (BinaryOp::Shr, 7),
                Some(Token::Concat) => (BinaryOp::Concat, 8),
                Some(Token::Plus) => (BinaryOp::Add, 9),
                Some(Token::Minus) => (BinaryOp::Sub, 9),
                Some(Token::Star) => (BinaryOp::Mul, 10),
                Some(Token::Slash) => (BinaryOp::Div, 10),
                Some(Token::FloorDiv) => (BinaryOp::FloorDiv, 10),
                Some(Token::Percent) => (BinaryOp::Mod, 10),
                Some(Token::Caret) => (BinaryOp::Pow, 12),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.advance();
            let right =
                self.parse_precedence(if matches!(op, BinaryOp::Pow | BinaryOp::Concat) {
                    prec
                } else {
                    prec + 1
                })?;
            left = Expr {
                kind: ExprKind::Binary(op, Box::new(left), Box::new(right)),
                line,
            };
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        // Only a `Name` or a parenthesized expression is a real Lua `prefixexp`
        // root - a bare table constructor, function literal, or literal cannot
        // take `()`/`[]`/`.`/`:` suffixes syntactically, even though the next
        // line may start with `(` (Lua's classic "ambiguous syntax" gotcha only
        // applies to genuine prefixexp roots). Without this check, a table
        // constructor immediately followed on the next line by a call-expression
        // statement (e.g. `local t = {}` then `(function (a) ... end)(1)`) would
        // be misparsed as the table being called.
        let is_paren_primary = self.check(&Token::LParen);
        let mut expr = self.parse_primary()?;
        let is_prefixexp = is_paren_primary
            || matches!(
                expr.kind,
                ExprKind::Name(_)
                    | ExprKind::Call(..)
                    | ExprKind::CallExpr(..)
                    | ExprKind::MethodCall(..)
            );
        loop {
            if !is_prefixexp {
                break;
            }
            let line = self.line();
            if self.eat(&Token::LParen) {
                let mut args = vec![];
                if !self.check(&Token::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Token::RParen)?;
                fn dotted(e: Expr) -> Option<String> {
                    match e.kind {
                        ExprKind::Name(n) => Some(n),
                        ExprKind::Field(base, field) => {
                            Some(format!("{}.{}", dotted(*base)?, field))
                        }
                        _ => None,
                    }
                }
                expr = match dotted(expr.clone()) {
                    Some(name) => Expr {
                        kind: ExprKind::Call(name, args),
                        line,
                    },
                    None => Expr {
                        kind: ExprKind::CallExpr(Box::new(expr), args),
                        line,
                    },
                };
            } else if matches!(self.peek(), Some(Token::StringLit(_)) | Some(Token::LBrace)) {
                let arg = self.parse_primary()?;
                fn dotted(e: Expr) -> Option<String> {
                    match e.kind {
                        ExprKind::Name(n) => Some(n),
                        ExprKind::Field(base, field) => {
                            Some(format!("{}.{}", dotted(*base)?, field))
                        }
                        _ => None,
                    }
                }
                let name = dotted(expr)
                    .ok_or_else(|| format!("line {line}: Lua call sugar needs a named function"))?;
                expr = Expr {
                    kind: ExprKind::Call(name, vec![arg]),
                    line,
                };
            } else if self.eat(&Token::Colon) {
                let method = self.expect_ident()?;
                let args = if self.eat(&Token::LParen) {
                    let mut args = vec![];
                    if !self.check(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.eat(&Token::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RParen)?;
                    args
                } else if matches!(self.peek(), Some(Token::StringLit(_)) | Some(Token::LBrace)) {
                    vec![self.parse_primary()?]
                } else {
                    return Err(self.error("expected arguments after Lua method name"));
                };
                expr = Expr {
                    kind: ExprKind::MethodCall(Box::new(expr), method, args),
                    line,
                };
            } else if self.eat(&Token::LBracket) {
                let index = self.parse_expr()?;
                self.expect(&Token::RBracket)?;
                expr = Expr {
                    kind: ExprKind::Index(Box::new(expr), Box::new(index)),
                    line,
                };
            } else if self.eat(&Token::Dot) {
                let field = self.expect_ident()?;
                expr = Expr {
                    kind: ExprKind::Field(Box::new(expr), field),
                    line,
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        let line = self.line();
        let start = self.current_span();
        match self.advance() {
            Some(Token::Function) => {
                let (params, param_annotations, vararg, vararg_name) = self.parse_param_list()?;
                let body = self.parse_function_body(vararg)?;
                let end = self.previous_span();
                Ok(Expr {
                    kind: ExprKind::Function(Box::new(Function {
                        name: format!("<anonymous@{line}>"),
                        source_file: None,
                        params,
                        param_annotations,
                        vararg,
                        vararg_name,
                        return_type: None,
                        body,
                        line,
                        is_global_decl: false,
                        source_span: crate::diagnostic::SourceSpan::new(
                            start.start,
                            end.end,
                            start.line,
                            start.column,
                        ),
                    })),
                    line,
                })
            }
            Some(Token::Vararg) => {
                if !self.vararg_allowed {
                    return Err(self.error("cannot use '...' outside a vararg function"));
                }
                Ok(Expr {
                    kind: ExprKind::Vararg,
                    line,
                })
            }
            Some(Token::LBrace) => self.parse_table(line),
            Some(Token::StringLit(bytes)) => Ok(Expr {
                kind: ExprKind::StringLit(bytes),
                line,
            }),
            Some(Token::Nil) => Ok(Expr {
                kind: ExprKind::NilLit,
                line,
            }),
            Some(Token::IntLit(n)) => Ok(Expr {
                kind: ExprKind::IntLit(n),
                line,
            }),
            Some(Token::FloatLit(n)) => Ok(Expr {
                kind: ExprKind::FloatLit(n),
                line,
            }),
            Some(Token::True) => Ok(Expr {
                kind: ExprKind::BoolLit(true),
                line,
            }),
            Some(Token::False) => Ok(Expr {
                kind: ExprKind::BoolLit(false),
                line,
            }),
            Some(Token::LParen) => {
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                // Lua truncates a parenthesized multi-value expression to
                // exactly one value (`(f())`, `(...)`) - everything else
                // is unaffected by the parens, so only wrap those kinds.
                if matches!(
                    inner.kind,
                    ExprKind::Call(..)
                        | ExprKind::CallExpr(..)
                        | ExprKind::MethodCall(..)
                        | ExprKind::Vararg
                ) {
                    Ok(Expr {
                        kind: ExprKind::Paren(Box::new(inner)),
                        line,
                    })
                } else {
                    Ok(inner)
                }
            }
            Some(Token::Ident(name)) => {
                // A qualified struct constructor (`geometry.Point { ... }`)
                // is the only Sol expression where dots belong to the name
                // itself. Keep ordinary dotted calls/field reads in the
                // postfix parser.
                let saved = self.pos;
                let mut qualified = name.clone();
                if self.config.sol_extensions {
                    while self.eat(&Token::Dot) {
                        let Some(Token::Ident(part)) = self.advance() else {
                            self.pos = saved;
                            break;
                        };
                        qualified.push('.');
                        qualified.push_str(&part);
                    }
                }
                let struct_constructor = self.config.sol_extensions
                    && (self.known_structs.contains(&qualified)
                        || self.imported_modules.iter().any(|module| {
                            qualified
                                .strip_prefix(module)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                        }));
                if !struct_constructor || !self.check(&Token::LBrace) || self.line() != line {
                    self.pos = saved;
                    qualified = name.clone();
                }
                if self.eat(&Token::LParen) {
                    let mut args = Vec::new();
                    if !self.check(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.eat(&Token::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr {
                        kind: ExprKind::Call(qualified, args),
                        line,
                    })
                } else if struct_constructor && self.check(&Token::LBrace) && self.line() == line {
                    self.advance();
                    // `Name { field = expr, ... }` - unambiguous, `{` never
                    // starts an expression on the same line. A brace on the
                    // next line is a statement block.
                    let mut fields = Vec::new();
                    while !self.check(&Token::RBrace) {
                        let fname = self.expect_ident()?;
                        self.expect(&Token::Eq)?;
                        let value = self.parse_expr()?;
                        fields.push((fname, value));
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::RBrace)?;
                    Ok(Expr {
                        kind: ExprKind::StructLiteral(qualified, fields),
                        line,
                    })
                } else {
                    Ok(Expr {
                        kind: ExprKind::Name(qualified),
                        line,
                    })
                }
            }
            // See the matching comment on the statement-level fallback above
            // - real Lua's wording ("unexpected symbol") is itself part of
            // the Lua 5.5 compatibility contract.
            other => Err(format!(
                "line {line}: unexpected symbol (unexpected token {other:?} in expression)"
            )),
        }
    }

    fn parse_table(&mut self, line: u32) -> Result<Expr, String> {
        let mut fields = Vec::new();
        while !self.check(&Token::RBrace) {
            if self.peek().is_none() {
                return Err(self.error("unexpected end of input inside table constructor"));
            }
            let field = if self.eat(&Token::LBracket) {
                let key = self.parse_expr()?;
                self.expect(&Token::RBracket)?;
                self.expect(&Token::Eq)?;
                TableField::Key(key, self.parse_expr()?)
            } else if matches!(self.peek(), Some(Token::Ident(_)))
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|token| token.token == Token::Eq)
            {
                let name = self.expect_ident()?;
                self.expect(&Token::Eq)?;
                TableField::Named(name, self.parse_expr()?)
            } else {
                TableField::Value(self.parse_expr()?)
            };
            fields.push(field);
            if !self.eat(&Token::Comma) && !self.eat(&Token::Semi) {
                break;
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(Expr {
            kind: ExprKind::Table(fields),
            line,
        })
    }
}
