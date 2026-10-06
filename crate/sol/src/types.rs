// Typed AST: `typeck.rs` produces it, `codegen.rs` compiles it. Locals are
// resolved to `LocalId`s 1:1 with Cranelift `Variable`s.

use std::collections::HashMap;

use crate::ast::BinaryOp;
use crate::diagnostic::SourceSpan;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    I64,
    F64,
    Bool,
    Nil,
    String,
    Array(Box<Type>),
    Map(Box<Type>, Box<Type>),
    /// A statically typed function pointer. Function values currently refer
    /// to top-level Sol functions; closures arrive with M11.
    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },
    /// Named struct - indexes into `TProgram::structs`.
    Struct(String),
    /// Runtime-checked value boxed as `{tag, payload}` (see `value.rs`).
    /// Reference payloads retain their original identity.
    Any,
}

impl Type {
    /// True when a value of this type is itself a pointer into the GC arena -
    /// i.e. storing it into an existing heap object can create a new
    /// inter-object edge the collector must be able to find. Used to decide
    /// where a generational write barrier is needed (see `gc::write_barrier`):
    /// `Function` is a raw code address, never a `gc.rs` allocation, so it's
    /// excluded even though it's a pointer.
    pub fn is_gc_pointer(&self) -> bool {
        matches!(
            self,
            Type::String | Type::Array(_) | Type::Map(_, _) | Type::Struct(_) | Type::Any
        )
    }
}

/// Compact GC descriptor for a sequence of 8-byte typed slots. A set bit
/// identifies a managed pointer. If a pointer occurs beyond the 64 slots the
/// compact form can describe, fall back to the collector's conservative
/// layout instead of dropping an edge.
pub fn pointer_layout_mask<'a>(types: impl IntoIterator<Item = &'a Type>) -> u64 {
    let mut mask = 0u64;
    for (index, ty) in types.into_iter().enumerate() {
        if !ty.is_gc_pointer() {
            continue;
        }
        if index >= u64::BITS as usize {
            return u64::MAX;
        }
        mask |= 1u64 << index;
    }
    mask
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::I64 => write!(f, "i64"),
            Type::F64 => write!(f, "f64"),
            Type::Bool => write!(f, "bool"),
            Type::Nil => write!(f, "nil"),
            Type::String => write!(f, "string"),
            Type::Array(inner) => write!(f, "Array<{inner}>"),
            Type::Map(key, value) => write!(f, "Map<{key}, {value}>"),
            Type::Function {
                params,
                return_type,
            } => {
                write!(f, "fn(")?;
                for (i, param) in params.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{param}")?;
                }
                write!(f, ") -> {return_type}")
            }
            Type::Struct(name) => write!(f, "{name}"),
            Type::Any => write!(f, "any"),
        }
    }
}

pub type LocalId = usize;

/// Struct layout: field declaration order is the byte offset (`index * 8`, every field is 8 bytes).
#[derive(Debug, Clone)]
pub struct StructLayout {
    pub fields: Vec<(String, Type)>,
}

impl StructLayout {
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|(n, _)| n == name)
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn pointer_layout_marks_only_managed_slots() {
        let fields = [
            Type::I64,
            Type::String,
            Type::Bool,
            Type::Array(Box::new(Type::I64)),
            Type::Function {
                params: Vec::new(),
                return_type: Box::new(Type::I64),
            },
        ];
        assert_eq!(pointer_layout_mask(&fields), 0b01010);
    }

    #[test]
    fn pointer_layout_falls_back_when_a_pointer_exceeds_the_compact_mask() {
        let mut fields = vec![Type::I64; 64];
        fields.push(Type::String);
        assert_eq!(pointer_layout_mask(&fields), u64::MAX);
    }
}

#[derive(Debug, Clone)]
pub struct TProgram {
    pub structs: HashMap<String, StructLayout>,
    pub functions: Vec<TFunction>,
    pub externs: Vec<TExternFunction>,
}

/// M7 §25 FFI: a native symbol resolved by name (typically via `dlsym`) -
/// no body. `name` doubles as the C symbol name.
#[derive(Debug, Clone)]
pub struct TExternFunction {
    pub name: String,
    pub params: Vec<Type>,
    pub return_type: Type,
}

#[derive(Debug, Clone)]
pub struct TFunction {
    pub name: String,
    pub source_file: Option<String>,
    /// Definition location retained through typed lowering and bytecode.
    pub source_line: u32,
    pub source_span: SourceSpan,
    pub params: Vec<(LocalId, Type)>,
    pub return_type: Type,
    pub body: TBlock,
    /// Every declared `LocalId` (params included) is in `0..local_count`.
    pub local_count: usize,
    /// Source spelling by LocalId. Optimizer-generated slots may have no
    /// entry; debug compilation deliberately preserves source slots.
    pub local_names: Vec<String>,
}

/// A typed statement paired with the source line it lowered from (U12 item
/// 3: closes the Tier-0 debugger's SourceMap gap - see
/// `docs/features/milestones/u12-wasm-playground.md`'s Work item 3 section).
/// `TStmt` itself carries no line field: adding one to every enum variant
/// would force exhaustive-pattern-match edits at ~224 call sites across both
/// jit-gated (`codegen.rs`, `jit.rs`) and always-compiled files. Wrapping the
/// *block* type instead keeps every existing `TStmt::Variant { .. }` match
/// arm unchanged - only the handful of functions that iterate over a block
/// need a one-line destructuring change (`for (line, stmt) in block` instead
/// of `for stmt in block`).
pub type TBlock = Vec<(u32, TStmt)>;

#[derive(Debug, Clone)]
pub enum TStmt {
    Break,
    Local {
        id: LocalId,
        value: TExpr,
    },
    Assign {
        id: LocalId,
        value: TExpr,
    },
    AssignIndex {
        array: TExpr,
        index: TExpr,
        value: TExpr,
    },
    AssignField {
        base: TExpr,
        field_index: usize,
        value: TExpr,
    },
    If {
        cond: TExpr,
        then_block: TBlock,
        else_block: TBlock,
    },
    While {
        cond: TExpr,
        body: TBlock,
    },
    /// Desugared counted loop; `start`/`stop`/`step` all `i64`.
    NumericFor {
        id: LocalId,
        stop_id: LocalId,
        step_id: LocalId,
        start: TExpr,
        stop: TExpr,
        step: TExpr,
        body: TBlock,
    },
    Return {
        value: Option<TExpr>,
    },
}

/// Every `LocalId`'s type, indexed by id. Moved here from `codegen.rs` (U12
/// item 3): this function has no Cranelift/codegen dependency - it only
/// walks `TFunction`/`TBlock`/`TStmt`/`Type`, all defined in this file - but
/// previously lived in a `jit`-feature-gated module, which made it
/// unreachable from the jit-free Tier-0 debugger (`debugger.rs`) needed for
/// `--no-default-features`/wasm32 builds. `codegen.rs`'s own callers
/// (`declare_var` needs every local's type up front) keep working via this
/// file's glob re-export, now sourced from one place instead of two copies.
pub fn collect_local_types(f: &TFunction) -> Vec<Type> {
    let mut types = vec![Type::I64; f.local_count]; // placeholder, all overwritten below
    for (id, ty) in &f.params {
        types[*id] = ty.clone();
    }
    fn walk(stmts: &TBlock, types: &mut [Type]) {
        for (_, s) in stmts {
            match s {
                TStmt::Local { id, value } => types[*id] = value.ty.clone(),
                TStmt::NumericFor {
                    id,
                    stop_id,
                    step_id,
                    body,
                    ..
                } => {
                    types[*id] = Type::I64;
                    types[*stop_id] = Type::I64;
                    types[*step_id] = Type::I64;
                    walk(body, types);
                }
                TStmt::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    walk(then_block, types);
                    walk(else_block, types);
                }
                TStmt::While { body, .. } => walk(body, types),
                TStmt::Break
                | TStmt::Assign { .. }
                | TStmt::AssignIndex { .. }
                | TStmt::AssignField { .. }
                | TStmt::Return { .. } => {}
            }
        }
    }
    walk(&f.body, &mut types);
    types
}

#[derive(Debug, Clone)]
pub struct TExpr {
    pub kind: TExprKind,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum TExprKind {
    StringLit(Vec<u8>),
    NilLit,
    Truth(Box<TExpr>),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Local(LocalId),
    /// Address of a declared top-level Sol function.
    FunctionRef(String),
    Neg(Box<TExpr>),
    Not(Box<TExpr>),
    /// Implicit `i64 -> f64` widening, inserted by `typeck.rs::coerce`.
    IntToFloat(Box<TExpr>),
    /// `+ - * / %`; both operands already the same type.
    Arith(BinaryOp, Box<TExpr>, Box<TExpr>),
    /// `== ~= < <= > >=`; always produces `Bool`.
    Compare(BinaryOp, Box<TExpr>, Box<TExpr>),
    /// `and`/`or`, non-short-circuiting, both operands `Bool`.
    Logical(BinaryOp, Box<TExpr>, Box<TExpr>),
    Call(String, Vec<TExpr>),
    /// Call through a typed function value.
    CallIndirect {
        callee: Box<TExpr>,
        args: Vec<TExpr>,
    },
    Index(Box<TExpr>, Box<TExpr>),
    Len(Box<TExpr>),
    /// `new_array_i64(n)` / `new_array_f64(n)`.
    NewArray {
        elem: Type,
        len: Box<TExpr>,
    },
    ArrayLiteral {
        elem: Type,
        values: Vec<TExpr>,
    },
    /// Monomorphized `map(Array<T>, fn(T) -> T)`, instantiated per call site
    /// on the concrete scalar `elem` type found there (`I64` or `F64`) -
    /// see `typeck.rs`'s `check_call` "map" branch.
    ArrayMap {
        array: Box<TExpr>,
        callback: Box<TExpr>,
        elem: Type,
    },
    NewMap {
        key: Type,
        value: Type,
    },
    MapLiteral {
        key: Type,
        value: Type,
        entries: Vec<(TExpr, TExpr)>,
    },
    /// Internal cursor primitives produced by typed `pairs(map)` lowering.
    /// Cursors are slot+1, so zero is both the initial cursor and end marker.
    MapNext {
        map: Box<TExpr>,
        cursor: Box<TExpr>,
    },
    MapKey {
        map: Box<TExpr>,
        cursor: Box<TExpr>,
    },
    MapValue {
        map: Box<TExpr>,
        cursor: Box<TExpr>,
    },
    /// `Name { field = expr, ... }`, fields already reordered to declaration order.
    StructLiteral {
        name: String,
        fields: Vec<TExpr>,
    },
    Field {
        base: Box<TExpr>,
        field_index: usize,
    },
    /// Implicit boxing into `any`, inserted by `coerce`.
    Box(Box<TExpr>),
    /// Implicit unboxing from `any`; traps at runtime on a type mismatch.
    Unbox(Box<TExpr>, Type),
}
