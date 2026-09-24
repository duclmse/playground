//! L2: a register-based bytecode compiler for the dynamic Lua runtime.
//!
//! This replaces the previous name-lookup, `Rc<RefCell<Env>>`-chain
//! tree-walking execution model with a real compile step: each Lua
//! `Function` becomes a `Proto` (instructions, a constant pool, upvalue
//! descriptors resolved at compile time, and a source line per instruction),
//! and `lua_runtime.rs` owns the VM that executes it. Table/metatable/
//! arithmetic/string-library semantics are untouched — they live in
//! `lua_runtime.rs` and are called from the VM's instruction handlers
//! exactly like they were called from the old tree-walker.
//!
//! Most registers are plain, unboxed `LuaValue`s in a flat array. Only a
//! register the compiler proves is ever captured as a `ParentLocal` upvalue
//! by some nested closure (`Proto::captured_registers`) gets a heap-allocated
//! `Rc<RefCell<LuaValue>>` cell instead - that's the one case where a
//! closure outliving this call needs to keep sharing the register's storage,
//! so an upvalue is just a clone of the `Rc` handle to that cell. Every
//! other register never leaves the flat, unboxed register file.
//!
//! Branch-target fields (`Jump`, `JumpIfFalse`, `JumpIfTrue`, `ForPrep`,
//! `ForLoop`, `TForLoop`) all use one addressing convention: a signed delta
//! relative to the branching instruction's own index, i.e. the VM computes
//! `new_pc = instr_index as i32 + delta`. This lets every branch be
//! constructed the same way (emit a placeholder, remember its index, patch
//! the delta once the target is known) regardless of which instruction it
//! is.
//!
//! Register numbering never reuses a slot within a statement's lifetime
//! bookkeeping across different *locals*, but temporaries are recycled
//! after each statement (see `Compiler::end_statement`), so
//! `num_registers` stays roughly proportional to "max concurrently-live
//! locals + largest single expression's temp count", not to the
//! function's total statement count. Some register slots are deliberately
//! over-reserved (e.g. a call's fixed-result-count area) rather than
//! computed exactly; that only affects `num_registers` (a sizing hint for
//! the VM's initial register-file allocation), never correctness.
//!
//! Split across sibling files for maintainability; every file here is a
//! private descendant module of `lua_bytecode` (not part of the crate's
//! public module tree on its own), so cross-file access to originally
//! module-private items uses `pub(super)` rather than full `pub`, and this
//! module re-exports everything that was reachable at `lua_bytecode::*`
//! before the split:
//!
//! - `instr` - `Const`/`UpvalSource`/`Instr` (the compiled instruction set)
//!   and `Proto` (a compiled function).
//! - `func_state` - `FuncState` and its scope/goto-label bookkeeping types
//!   (`BlockScope`, `LoopCtx`): per-function register allocation and
//!   block-scope/goto/label resolution.
//! - `compile_stmt` - `Compiler`'s statement/control-flow compilation:
//!   blocks, `if`/`while`/`repeat`/numeric- and generic-`for`, `local`/
//!   `global` declarations, assignment, `return`, and goto/label statements.
//! - `compile_calls` - `Compiler`'s expression-list adjustment, call/
//!   method-call/vararg compilation, and name/`_ENV` resolution helpers.
//! - `compile_expr` - `Compiler::compile_expr`, the expression-level match
//!   over `ExprKind`.
//! - `tests` - unit tests.

use std::collections::HashMap;
use std::rc::Rc;

use sol_core::{PrototypeMetadata, SourceLocation, SourceMap, ValueCount};

use crate::ast::{Expr, Function};

mod compile_calls;
mod compile_expr;
mod compile_stmt;
mod func_state;
mod instr;
#[cfg(test)]
mod tests;

pub use instr::{Const, Instr, Proto, UpvalSource};

use func_state::{FuncState, GlobalScanState, LocalOrGlobal};

pub type Reg = u16;

fn value_count(count: i32) -> ValueCount {
    ValueCount::from_legacy(count)
        .expect("the compiler only emits fixed non-negative counts or the open -1 sentinel")
}

pub struct Compiler {
    stack: Vec<FuncState>,
    optimization_plan: Option<crate::typeck::inference::OptimizationPlan>,
    /// Anchors every string-literal token compiled anywhere in this chunk -
    /// including inside nested function bodies, which each get their own
    /// `FuncState`/constant pool - so byte-identical literals share one
    /// `Rc<Vec<u8>>`. Mirrors real Lua's `llex.c` `luaX_newstring`/
    /// `anchorstr`, which anchors string tokens in a table scoped to the
    /// whole `LexState` (one per compile), not per function; see
    /// `intern_string_literal`.
    string_literals: HashMap<Vec<u8>, Rc<Vec<u8>>>,
}

impl Compiler {
    pub fn compile_top_level(function: &Function) -> Result<Rc<Proto>, String> {
        let mut compiler = Compiler {
            stack: Vec::new(),
            optimization_plan: None,
            string_literals: HashMap::new(),
        };
        compiler.compile_function(function)
    }

    pub fn compile_top_level_with_plan(
        function: &Function,
        optimization_plan: &crate::typeck::inference::OptimizationPlan,
    ) -> Result<Rc<Proto>, String> {
        let mut compiler = Compiler {
            stack: Vec::new(),
            optimization_plan: Some(optimization_plan.clone()),
            string_literals: HashMap::new(),
        };
        compiler.compile_function(function)
    }

    /// Returns the chunk-wide shared `Rc<Vec<u8>>` for a string-literal
    /// token's bytes, allocating one the first time this exact content is
    /// seen anywhere in the current compile. Real Lua's `%p` reports
    /// pointer identity for strings past its short-string interning cutoff,
    /// so without this, two textually-identical literals compiled into
    /// different functions (nested closures each get their own `FuncState`
    /// constant pool) would wrongly appear to be different objects.
    pub(super) fn intern_string_literal(&mut self, bytes: &[u8]) -> Rc<Vec<u8>> {
        if let Some(existing) = self.string_literals.get(bytes) {
            return existing.clone();
        }
        let interned = Rc::new(bytes.to_vec());
        self.string_literals
            .insert(bytes.to_vec(), interned.clone());
        interned
    }

    /// Scans for the `global`-declaration state governing `name`, starting
    /// at the current function and, if not immediately resolved there,
    /// continuing into each enclosing function's own active scopes in
    /// turn - mirroring real Lua's upvalue-like walk through `fs->prev` so
    /// a declaration in an enclosing, still-open block stays visible to a
    /// nested function compiled within it (`goto.lua`'s `global none;
    /// local function foo () XXX = 1 end` case).
    fn resolve_global_decl(&self, name: &str) -> GlobalScanState {
        let mut state = GlobalScanState::Preambular;
        for lvl in (0..self.stack.len()).rev() {
            if self.stack[lvl].scan_global_decls(name, &mut state) {
                break;
            }
        }
        state
    }

    /// Whether `_ENV` itself has been declared as a named `global` anywhere
    /// in the active enclosing-function chain. Real Lua resolves every
    /// global access by first looking up the special `_ENV` variable
    /// (`buildglobal`'s `singlevaraux(fs, ls->envn, ...)`) - if `_ENV` was
    /// declared as an ordinary named global rather than being the real
    /// implicit environment upvalue, *that* lookup itself now resolves as
    /// "global" too, which is nonsensical, so every ordinary global access
    /// (including of `_ENV` itself) becomes a compile error, regardless of
    /// whether the specific name being accessed was otherwise validly
    /// declared - matching `buildglobal`'s `"%s is global when accessing
    /// variable '%s'"` error.
    fn env_declared_as_global(&self) -> bool {
        self.stack.iter().any(|fs| fs.has_named_global("_ENV"))
    }

    /// Checks whether an ordinary (non-declaring) global access to `name`
    /// is allowed under the current `global`-declaration state, per Lua
    /// 5.5's stricter checking. Declaring statements themselves (the
    /// `global` statement's own re-read/write of the names it declares)
    /// must not go through this - only ordinary reads/writes of a name.
    fn check_global_access(&self, name: &str, line: u32) -> Result<(), String> {
        if self.env_declared_as_global() {
            return Err(format!(
                "line {line}: _ENV is global when accessing variable '{name}'"
            ));
        }
        // `_ENV` itself is Lua's always-available implicit environment
        // upvalue, not an ordinary global - real Lua resolves a bare `_ENV`
        // identifier via the upvalue chain (pre-seeded on every function),
        // never falling into the declared-name strict check at all (see
        // `singlevaraux`/`buildvar`: that check only fires once a name
        // resolves as `VGLOBAL`, which a structurally-present `_ENV`
        // upvalue never does unless it was itself redeclared via `global
        // _ENV` - already handled above).
        if name != "_ENV" && matches!(self.resolve_global_decl(name), GlobalScanState::Invalidated)
        {
            return Err(format!("line {line}: variable '{name}' not declared"));
        }
        Ok(())
    }

    fn compile_function(&mut self, function: &Function) -> Result<Rc<Proto>, String> {
        let mut state = FuncState::new(function.name.clone());
        state.num_params = function.params.len();
        state.is_vararg = function.vararg;
        state.push_scope();
        for (name, _) in &function.params {
            state.declare_local(name, false);
        }
        if let Some(vararg_name) = &function.vararg_name {
            state.vararg_name = Some(state.declare_local(vararg_name, true));
        }
        self.stack.push(state);
        if let Err(error) = self.compile_block(&function.body) {
            self.stack.pop();
            return Err(error);
        }
        // Implicit `return` at the end of a function body - attributed to
        // the closing `end`'s line, matching real Lua (`luaK_ret` emitted
        // from `close_func`/`lparser.c` after the body, at the `end` token's
        // line) - not the function's own declaration line, so this doesn't
        // fold into `activelines[linedefined]`.
        let level = self.level();
        self.stack[level].emit(Instr::Return(0, ValueCount::ZERO), function.end_line);
        if let Err(error) = self.stack[level].pop_scope(function.end_line) {
            self.stack.pop();
            return Err(error);
        }
        let mut state = self.stack.pop().expect("pushed above");
        if !state.scopes.is_empty() {
            return Err(format!(
                "line {}: unbalanced scopes compiling '{}'",
                function.line, function.name
            ));
        }
        let num_registers = state.max_reg as usize;
        let mut captured_registers = vec![false; num_registers];
        for &reg in &state.captured {
            captured_registers[reg as usize] = true;
        }
        let captured_cell_count = captured_registers.iter().filter(|&&c| c).count();
        Ok(Rc::new(Proto {
            metadata: PrototypeMetadata::new(
                state.name,
                state.num_params,
                state.is_vararg,
                num_registers,
            )
            .expect("dynamic bytecode metadata fits the compiler's narrower register limits"),
            instrs: std::mem::take(&mut state.instrs),
            source_map: SourceMap::new(
                std::mem::take(&mut state.lines)
                    .into_iter()
                    .map(|line| SourceLocation::new(line, 0))
                    .collect(),
            ),
            consts: std::mem::take(&mut state.consts),
            upvals: std::mem::take(&mut state.upvals),
            upval_names: std::mem::take(&mut state.upval_names),
            param_names: function
                .params
                .iter()
                .map(|(name, _)| name.clone())
                .collect(),
            vararg_name: state.vararg_name,
            captured_registers,
            captured_cell_count,
            nested: std::mem::take(&mut state.nested),
            line_defined: function.line,
            last_line_defined: function.end_line,
        }))
    }

    fn level(&self) -> usize {
        self.stack.len() - 1
    }

    fn resolve(&mut self, level: usize, name: &str) -> Resolved {
        match self.stack[level].find_local_or_global(name) {
            Some(LocalOrGlobal::Local(reg, constant)) => return Resolved::Local(reg, constant),
            Some(LocalOrGlobal::GlobalDecl) => return Resolved::Global,
            None => {}
        }
        if let Some((idx, constant)) = self.stack[level].find_upval(name) {
            return Resolved::Upval(idx, constant);
        }
        if level == 0 {
            return Resolved::Global;
        }
        match self.resolve(level - 1, name) {
            Resolved::Local(reg, constant) => {
                self.stack[level - 1].mark_captured(reg);
                let idx =
                    self.stack[level].add_upval(name, UpvalSource::ParentLocal(reg), constant);
                Resolved::Upval(idx, constant)
            }
            Resolved::Upval(parent_idx, constant) => {
                let idx = self.stack[level].add_upval(
                    name,
                    UpvalSource::ParentUpval(parent_idx),
                    constant,
                );
                Resolved::Upval(idx, constant)
            }
            Resolved::Global => Resolved::Global,
        }
    }

    /// Compiles `expr` so its single result value ends up exactly in
    /// register `dst`, reserving `dst` first so the expression's own
    /// internal temporaries (which may need registers above `dst`) can
    /// never collide with it.
    fn compile_into(&mut self, expr: &Expr, dst: Reg) -> Result<(), String> {
        let level = self.level();
        self.stack[level].reserve_through(dst);
        let r = self.compile_expr(expr)?;
        if r != dst {
            self.stack
                .last_mut()
                .unwrap()
                .emit(Instr::Move(dst, r), expr.line);
        }
        Ok(())
    }
}

enum Resolved {
    Local(Reg, bool),
    Upval(u16, bool),
    Global,
}
