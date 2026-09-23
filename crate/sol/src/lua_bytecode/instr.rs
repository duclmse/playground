//! The instruction/constant value types: `Const`, `UpvalSource`, `Instr`,
//! and the compiled-function output type `Proto`.

use std::rc::Rc;

use sol_core::{ExecutablePrototype, PrototypeMetadata, SourceMap, ValueCount};

use crate::ast::BinaryOp;

use super::Reg;

#[derive(Debug, Clone)]
pub enum Const {
    Nil,
    Bool(bool),
    Integer(i64),
    Float(f64),
    Str(Rc<Vec<u8>>),
}

/// Where a nested function's upvalue comes from, relative to its
/// immediately-enclosing function.
#[derive(Debug, Clone, Copy)]
pub enum UpvalSource {
    ParentLocal(Reg),
    ParentUpval(u16),
}

#[derive(Debug, Clone)]
pub enum Instr {
    LoadConst(Reg, u32),
    LoadNil(Reg),
    LoadBool(Reg, bool),
    Move(Reg, Reg),
    /// Allocates a *fresh* cell at `dst` containing a clone of `src`'s
    /// current value. Used at every dynamic execution of a local
    /// declaration (including loop iterations), so closures created in one
    /// iteration keep their own cell even after a later iteration
    /// re-declares the same register number.
    /// Binds `dst` as a new local from `src`; the final operand names the
    /// local in `Proto::consts` for Lua debug/error diagnostics.
    NewLocal(Reg, Reg, u32),
    GetUpval(Reg, u16),
    SetUpval(u16, Reg),
    /// Loads the frame's default environment table. Lexically rebound `_ENV`
    /// variables compile as ordinary locals/upvalues instead.
    GetEnvironment(Reg),
    /// Replaces the frame's default environment table outright - a bare
    /// (no `local`) assignment to `_ENV` itself, which real Lua resolves
    /// through `_ENV`'s implicit per-chunk upvalue rather than as an
    /// ordinary global named `"_ENV"`: every later plain global read/write
    /// in this same frame (that doesn't have its own lexically-local
    /// `_ENV` shadowing it - that case already compiles as an ordinary
    /// local/upvalue and never reaches this instruction) must see the new
    /// table, while the caller's own environment stays untouched. Only
    /// emitted when `_ENV` has no local/upvalue binding in scope yet (see
    /// `emit_environment_set`); a declaring `global _ENV = ...` is a
    /// separate, already-guarded path and never reaches this either.
    SetEnvironment(Reg),
    GetGlobal(Reg, Rc<str>),
    /// Writes a global. `declare` distinguishes a `global` declaration
    /// (`Stmt::Global`/`GlobalFunction`: unconditionally overwrites,
    /// ignoring any existing binding's constness; the new binding's
    /// constness becomes `constant`) from a plain assignment to a
    /// global-resolved name (`declare` false: errors if an existing
    /// binding is const, otherwise updates its value in place; if no
    /// binding exists yet, creates a fresh non-const one — `constant` is
    /// unused in this mode).
    SetGlobal(Rc<str>, Reg, bool, bool),
    /// Runtime guard for a `global NAME = value` (or `global function NAME`)
    /// declaration-with-initializer: real Lua 5.5 raises `"global '%s'
    /// already defined"` if the name's current value (already loaded into
    /// this register by a preceding environment read) is non-nil at the
    /// moment of declaration - protecting against accidentally clobbering
    /// an existing binding (e.g. redeclaring the real `print`). A bare
    /// `global NAME` with no initializer never emits this check.
    ErrorIfGlobalDefined(Reg, Rc<str>),
    NewTable(Reg),
    NewClosure(Reg, u16),
    /// The `u32` is a constant-pool index into `Proto::consts` (always a
    /// `Const::Str`), not an inline `Rc<str>` - keeps `Instr` small and lets
    /// the VM reuse the pooled `Rc<Vec<u8>>` as a table key via a cheap
    /// `Rc::clone` instead of allocating a fresh byte vector per execution.
    GetField(Reg, Reg, u32),
    SetField(Reg, u32, Reg),
    GetIndex(Reg, Reg, Reg),
    SetIndex(Reg, Reg, Reg),
    /// `table[index] = value` for a compile-time-known array position.
    SetArrayItem(Reg, i64, Reg),
    /// Consumes the open multi-value region starting at `from` (produced by
    /// the immediately preceding instruction) as sequential array items
    /// starting at `start_index`.
    SetArrayMulti(Reg, i64, Reg),
    Len(Reg, Reg),
    Not(Reg, Reg),
    Neg(Reg, Reg),
    BitNot(Reg, Reg),
    Binary(BinaryOp, Reg, Reg, Reg),
    /// U4 proof-carrying integer arithmetic. The VM keeps a guard/deopt path
    /// for compiler bugs or invalidated metadata, but skips numeric coercion
    /// and metamethod resolution on the proven path.
    IntegerBinary(BinaryOp, Reg, Reg, Reg),
    Jump(i32),
    JumpIfFalse(Reg, i32),
    JumpIfTrue(Reg, i32),
    /// `base` holds the callee; args are `regs[base+1..]`. `nargs == -1`
    /// means "consume the open multi-value region up to the frame's
    /// current top" (a trailing call/vararg as the last argument).
    /// `nresults == -1` means "keep all results, extending the frame's
    /// top"; otherwise results are truncated/nil-padded to exactly
    /// `nresults` values starting at `base`.
    Call(Reg, ValueCount, ValueCount),
    /// Proper tail call: returns all callee results directly to this frame's
    /// caller. The trampoline replaces a Lua-closure frame in place.
    TailCall(Reg, ValueCount),
    /// `count == -1` means "all remaining varargs, extending the frame's
    /// open-multi top"; otherwise exactly `count` values (nil-padded).
    Vararg(Reg, ValueCount),
    /// `count == -1` means "return the open multi-value region starting at
    /// `base`"; otherwise return exactly `count` values.
    Return(Reg, ValueCount),
    /// `base` starts a hidden `[start, stop, step, var]` register block.
    /// Validates/coerces the loop bounds, and if the loop should run at
    /// least once, materializes a fresh `var` cell and falls through;
    /// otherwise jumps by `delta` (to just past the loop).
    ForPrep(Reg, i32),
    /// Placed at the end of the loop body: advances `start` by `step`, and
    /// if still in range, makes a fresh `var` cell for the next iteration
    /// and jumps by `delta` (back to the loop body's first instruction);
    /// otherwise falls through.
    ForLoop(Reg, i32),
    /// `base` starts a hidden `[f, s, ctrl]` register block; calls
    /// `f(s, ctrl)` and writes `nvars` results (fresh cells) starting at
    /// `base + 3`.
    TForCall(Reg, u16),
    /// If `regs[base + 3]` (the first result written by the preceding
    /// `TForCall`) is nil, falls through (loop ends); otherwise copies it
    /// into `ctrl` (`base + 2`) and jumps by `delta` (back to the loop
    /// body).
    TForLoop(Reg, i32),
    /// Lua 5.4+ `<close>` support: validates that `regs[0]` is closeable
    /// (`nil`, `false`, or has a `__close` metamethod - otherwise raises an
    /// error naming the pooled string at `consts[1]`, e.g. "variable 'x' got
    /// a non-closable value"), then pushes it onto the frame's to-be-closed
    /// stack.
    MarkClose(Reg, u32),
    /// Pops this many entries off the frame's to-be-closed stack (LIFO,
    /// innermost/most-recently-pushed first) and calls `__close(value, nil)`
    /// on each one that isn't `nil`/`false`. Emitted at every normal
    /// scope-exit path (fallthrough, `break`, `return`) for a scope that
    /// declared one or more `<close>` locals.
    CloseSlots(u16),
}

impl Instr {
    /// Projects a generic bytecode call onto the shared semantic ABI.
    pub fn call_site(&self) -> Option<sol_core::CallSite> {
        match self {
            Self::Call(base, arguments, results) => Some(sol_core::CallSite::new(
                *base as u32,
                *arguments,
                *results,
                sol_core::CallKind::Normal,
            )),
            Self::TailCall(base, arguments) => Some(sol_core::CallSite::new(
                *base as u32,
                *arguments,
                sol_core::ValueCount::Open,
                sol_core::CallKind::Tail,
            )),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Proto {
    pub metadata: PrototypeMetadata,
    pub instrs: Vec<Instr>,
    pub source_map: SourceMap,
    pub consts: Vec<Const>,
    pub upvals: Vec<UpvalSource>,
    /// Source names for `upvals`, kept alongside the capture descriptors so
    /// runtime diagnostics can preserve Lua's `upvalue 'name'` wording.
    pub upval_names: Vec<String>,
    pub vararg_name: Option<Reg>,
    pub nested: Vec<Rc<Proto>>,
    /// `captured_registers[i]` is true iff register `i` is ever captured as
    /// a `ParentLocal` upvalue by some nested closure. The VM only needs a
    /// heap-allocated `Rc<RefCell<LuaValue>>` cell for these registers;
    /// every other register can be a plain, unboxed value in a flat array.
    pub captured_registers: Vec<bool>,
    /// `captured_registers.iter().filter(|c| **c).count()`, precomputed once
    /// here instead of rescanned by the VM on every call to this `Proto`
    /// (used only to size an allocation-budget charge).
    pub captured_cell_count: usize,
    /// The line the `function` keyword itself appears on - real Lua's
    /// `debug.getinfo`'s `linedefined` (`lua_Debug::linedefined`). Distinct
    /// from `source_map.location(0)`'s line, which is the first *executable*
    /// instruction's line (e.g. the first statement inside the body) - those
    /// coincide for a one-line function but not for a multi-line signature
    /// or an empty body whose first instruction is the implicit `return`.
    pub line_defined: u32,
}

impl ExecutablePrototype for Proto {
    fn metadata(&self) -> &PrototypeMetadata {
        &self.metadata
    }

    fn source_map(&self) -> &SourceMap {
        &self.source_map
    }

    fn instruction_count(&self) -> usize {
        self.instrs.len()
    }
}
