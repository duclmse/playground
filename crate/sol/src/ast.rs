// Untyped AST from the parser; `typeck.rs` lowers this to the typed AST in types.rs.

use crate::diagnostic::SourceSpan;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeName {
    I64,
    F64,
    Bool,
    Nil,
    String,
    Array(Box<TypeName>),
    Map(Box<TypeName>, Box<TypeName>),
    /// A statically typed callable value: `fn(i64, string) -> bool`.
    Function {
        params: Vec<TypeName>,
        return_type: Box<TypeName>,
    },
    /// Resolved against `Program::structs` by `typeck.rs`.
    Struct(String),
    /// Gradual typing (faster_lua.md §3): checked at runtime, not compile time.
    Any,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub imports: Vec<ImportDecl>,
    pub exports: Vec<String>,
    /// True when `main` was synthesized from top-level statements.
    pub initializer: bool,
    pub aliases: Vec<AliasDef>,
    pub structs: Vec<StructDef>,
    pub functions: Vec<Function>,
    pub externs: Vec<ExternFunction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AliasDef {
    pub name: String,
    pub target: TypeName,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    pub module: String,
    pub line: u32,
}

/// `extern function name(params): ReturnType` (M7 §25 FFI) - a native
/// symbol already loaded in the process (typically libc/libm), resolved by
/// name via `dlsym`. No body; `name` is also the C symbol name.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternFunction {
    pub name: String,
    pub params: Vec<(String, TypeName)>,
    pub return_type: TypeName,
    pub line: u32,
}

/// Fixed-layout aggregate; field declaration order is the memory layout.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, TypeName)>,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub source_file: Option<String>,
    pub params: Vec<(String, TypeName)>,
    /// Parallel to `params`: distinguishes an explicit contract (including
    /// explicit `any`) from Lua's omitted annotation. Runtime semantics are
    /// identical; U4 inference may specialize only the latter.
    pub param_annotations: Vec<bool>,
    /// Lua's `...` parameter pack. It is meaningful only in `.lua` source
    /// and is lowered by the dynamic call-frame runtime in M13 L4.
    pub vararg: bool,
    /// Lua 5.5 also permits a name after `...` (`...args`) for the implicit
    /// vararg table binding.
    pub vararg_name: Option<String>,
    pub return_type: Option<TypeName>,
    pub body: Block,
    pub line: u32,
    /// The line of this function's closing `end` (real Lua's own
    /// `lastlinedefined`, tracked in `close_func`/`lparser.c` at the
    /// `end`/EOZ token) - the top-level "main" chunk (no explicit `function`
    /// keyword) uses the source's last token's line instead, since it has no
    /// `end` of its own. Distinct from `source_span.line`, which is this
    /// function's *starting* line.
    pub end_line: u32,
    /// Full byte range of the declaration, from `function`/`fn` through its
    /// closing `end`. `source_file` supplies the file component when loaded as
    /// part of a module graph.
    pub source_span: SourceSpan,
    /// True only for the real Lua 5.5 `global function NAME(...) end` form
    /// (`Stmt::GlobalFunction` also carries a plain, non-`global` top-level
    /// `function NAME(...) end` when the parser can't hoist it - that form is
    /// ordinary assignment sugar, not a `global`-declaring statement, and
    /// must not gain `global`'s declare/shadow-outer-local semantics).
    pub is_global_decl: bool,
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone, PartialEq)]
pub enum AssignTarget {
    Name(String),
    Index(Expr, Expr),
    Field(Expr, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Lua 5.5 compatibility syntax. It remains a dynamic-runtime operation;
    /// `typeck` rejects it until M13's `_ENV` table exists.
    Global {
        names: Vec<GlobalBinding>,
        values: Vec<Expr>,
        line: u32,
    },
    /// `global function name(...) ... end` has Lua syntax but cannot be lowered
    /// through the typed function table before the dynamic closure runtime.
    GlobalFunction(Function),
    /// Local and nested functions require closures/upvalues in the dynamic
    /// runtime. They remain parser-visible before that runtime is available.
    LocalFunction(Function),
    /// Lua label and jump syntax. Validation and execution belong to the
    /// bytecode compiler because legality depends on scope entry/exit.
    Label {
        name: String,
        line: u32,
    },
    Goto {
        name: String,
        line: u32,
    },
    /// Each name's 4th element is Lua 5.4+'s `<close>` attribute (mutually
    /// exclusive with the 3rd element's `<const>` in source, but both are
    /// plain `bool`s here - the parser rejects any name carrying both).
    MultiLocal {
        names: Vec<(String, Option<TypeName>, bool, bool)>,
        values: Vec<Expr>,
        line: u32,
    },
    MultiAssign {
        targets: Vec<AssignTarget>,
        values: Vec<Expr>,
        line: u32,
    },
    Block(Block),
    Repeat {
        body: Block,
        cond: Expr,
        line: u32,
    },
    Break {
        line: u32,
    },
    Expr(Expr),
    Local {
        name: String,
        ty: Option<TypeName>,
        constant: bool,
        /// Lua 5.4+'s `<close>` attribute (`local x <close> = value`).
        close: bool,
        value: Expr,
        line: u32,
    },
    Assign {
        target: AssignTarget,
        value: Expr,
        line: u32,
    },
    If {
        cond: Expr,
        then_block: Block,
        else_block: Option<Block>,
        line: u32,
    },
    While {
        cond: Expr,
        body: Block,
        line: u32,
    },
    NumericFor {
        var: String,
        start: Expr,
        stop: Expr,
        step: Option<Expr>,
        body: Block,
        line: u32,
    },
    GenericFor {
        vars: Vec<String>,
        iterators: Vec<Expr>,
        body: Block,
        line: u32,
    },
    Return {
        value: Option<Expr>,
        line: u32,
    },
    /// Lua's non-empty return list. The dynamic VM applies Lua's final-value
    /// expansion rules; Sol's scalar return statement keeps its existing AST.
    MultiReturn {
        values: Vec<Expr>,
        line: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum GlobalName {
    Name(String),
    All,
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalBinding {
    pub name: GlobalName,
    pub constant: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    FloorDiv,
    Pow,
    Concat,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableField {
    Value(Expr),
    Named(String, Expr),
    Key(Expr, Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Table(Vec<TableField>),
    /// A nested Lua function. Its name is an internal parser label only; it
    /// does not declare a top-level typed Sol function.
    Function(Box<Function>),
    /// The current Lua function's `...` pack.
    Vararg,
    StringLit(Vec<u8>),
    NilLit,
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Name(String),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    /// A Lua call whose callee is not a statically named Sol function.
    CallExpr(Box<Expr>, Vec<Expr>),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Len(Box<Expr>),
    /// Field order in source is arbitrary; `typeck.rs` reorders to the struct's declared order.
    StructLiteral(String, Vec<(String, Expr)>),
    Field(Box<Expr>, String),
    /// Runtime type test over an `any` value: `value is Type`.
    TypeTest(Box<Expr>, TypeName),
    /// Explicit checked conversion: `value as Type`.
    Cast(Box<Expr>, TypeName),
    /// A parenthesized expression that wraps a multi-value-producing
    /// inner expression (`Call`/`CallExpr`/`MethodCall`/`Vararg`) - Lua
    /// truncates `(f())`/`(...)` to exactly one value, unlike the bare
    /// `f()`/`...` (which keep expanding in a trailing list/return/table-
    /// field position). The parser only wraps those four kinds; every
    /// other parenthesized expression is unaffected by parens and is
    /// returned as-is, without this node.
    Paren(Box<Expr>),
}
