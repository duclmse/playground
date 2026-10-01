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

pub use instr::{
    BoundedCache, CallCacheEntry, Const, FieldCacheEntry, Instr, LocalDebug, NativeStatus, Proto,
    UpvalSource, IC_SLOTS,
};

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
            state.declare_local_checked(name, false, function.line)?;
        }
        if let Some(vararg_name) = &function.vararg_name {
            state.vararg_name =
                Some(state.declare_local_checked(vararg_name, true, function.line)?);
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
        // Real Lua caps a function's register-stack window at `MAX_FSTACK`
        // (255, `lparser.c`/`lcode.c`'s `luaK_checkstack`), checked as
        // registers are reserved. Sol's `Reg` is a `u16` with far more
        // headroom, so without this a function using more than 255 live
        // registers (e.g. a call with hundreds of arguments) would compile
        // and run instead of failing like real Lua does.
        const MAX_REGISTERS: usize = 255;
        if num_registers > MAX_REGISTERS {
            return Err(format!(
                "line {}: too many registers (limit is {MAX_REGISTERS}) in function '{}'",
                function.line, function.name
            ));
        }
        // Real Lua caps a function's upvalue count at `MAXUPVAL` (255,
        // `lparser.c`'s `newupvalue`/`luaY_checklimit`), checked as each new
        // upvalue is created while resolving a name mid-parse. Sol's
        // equivalent (`FuncState::add_upval`, called from `resolve` above) is
        // infallible, since `resolve` is also reached - via
        // `resolve(level, "_ENV")` - from `emit_environment_get`/
        // `emit_environment_set`, which are `()`-returning and used by every
        // plain global variable read/write; threading a `Result` through
        // those just to cover this one narrow case isn't worth it. Instead,
        // check the finished count here, exactly like `MAX_REGISTERS` above:
        // `add_upval` never fails, but a function that ends up with more than
        // 255 entries in `state.upvals` is rejected post-hoc, the same way an
        // over-budget register file is. Real Lua's own wording separately
        // reports the current parse position and the function's own
        // definition line ("in function at line N"); Sol only tracks the
        // latter here, so both roles are filled by `function.line` - this
        // still satisfies `errors.lua`'s own compound assertion
        // (`string.find(b, "too many upvalues") and string.find(b, "line
        // 5")`), which checks for that literal "line N" substring alongside
        // "too many upvalues", not real Lua's exact phrasing.
        const MAX_UPVALUES: usize = 255;
        if state.upvals.len() > MAX_UPVALUES {
            return Err(format!(
                "line {}: too many upvalues (limit is {MAX_UPVALUES}) in function at line {} '{}'",
                function.line, function.line, function.name
            ));
        }
        let mut captured_registers = vec![false; num_registers];
        for &reg in &state.captured {
            captured_registers[reg as usize] = true;
        }
        let captured_cell_count = captured_registers.iter().filter(|&&c| c).count();
        // Named-vararg lazy-view optimization (vararg.lua's `notab`/`foo`
        // zero-allocation contract): only eligible when the register is
        // never captured as an upvalue (a nested closure could stash the
        // raw cell somewhere a flat scan of this function's own `instrs`
        // can't see) - everything else is decided by the exhaustive
        // operand scan in `try_lower_vararg_to_lazy_view`.
        let vararg_lazy = match state.vararg_name {
            Some(vararg_reg) if !captured_registers[vararg_reg as usize] => {
                try_lower_vararg_to_lazy_view(&mut state.instrs, vararg_reg)
            }
            _ => false,
        };
        let instr_count = state.instrs.len();
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
            locals: std::mem::take(&mut state.locals),
            vararg_name: state.vararg_name,
            vararg_lazy,
            captured_registers,
            captured_cell_count,
            nested: std::mem::take(&mut state.nested),
            call_cache: (0..instr_count).map(|_| BoundedCache::default()).collect(),
            field_cache: (0..instr_count).map(|_| BoundedCache::default()).collect(),
            global_cache: (0..instr_count).map(|_| BoundedCache::default()).collect(),
            // `debug.getinfo` reports both definition lines as zero for a
            // top-level chunk, even though its source map still uses the
            // source's real executable line numbers. An ordinary function
            // named `main` must retain its own declaration/end lines, hence
            // the explicit AST distinction rather than a name check.
            line_defined: if function.is_chunk { 0 } else { function.line },
            last_line_defined: if function.is_chunk {
                0
            } else {
                function.end_line
            },
            call_count: std::cell::Cell::new(0),
            native_status: std::cell::Cell::new(NativeStatus::Interpreted),
            optimize_count: std::cell::Cell::new(0),
            osr_counts: std::cell::RefCell::new(std::collections::HashMap::new()),
            osr_entries: std::cell::RefCell::new(std::collections::HashMap::new()),
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

/// Attempts to lower a named vararg parameter's register (`vararg_reg`) to
/// the zero-allocation lazy view described on `Instr::VarargIndexGet` and
/// friends, rewriting `instrs` in place. Returns `true` (having rewritten
/// every direct index/field access against `vararg_reg` into the matching
/// `Vararg*` opcode) iff a whole-function scan proves `vararg_reg` is never
/// used any other way - e.g. moved into another register, passed as a call
/// argument or return value, used as a key/value against some *other*
/// table, or as the subject of `...`/numeric/generic `for`'s open register
/// range. Any other shape falls back to `false`, leaving `instrs`
/// unchanged, so the caller keeps today's already-correct eager-table
/// behavior.
///
/// Callers must separately confirm `vararg_reg` isn't in
/// `captured_registers` first - a register captured as an upvalue by some
/// nested closure is never eligible regardless of what this scan finds,
/// since a nested closure can only reach it through the capture (opaque to
/// a scan of this function's own flat `instrs`), never as a direct operand
/// here.
fn try_lower_vararg_to_lazy_view(instrs: &mut [Instr], vararg_reg: Reg) -> bool {
    // Every function body ends with an unconditional implicit
    // `Return(0, ValueCount::ZERO)` followed by that outer scope's
    // exit cleanup (`DetachCell`/`LoadNil` for each of its locals,
    // including the vararg parameter's own register) - dead code whenever
    // the body already returned earlier, but still present in `instrs` and,
    // scanned unconditionally, would disqualify nearly every real function.
    // Skip disqualification (but still lower, harmlessly, since it never
    // runs) for any instruction that a straight-line/jump-target reachability
    // pass can prove unreachable from the function's entry.
    let reachable = compute_reachable(instrs);
    if instrs
        .iter()
        .enumerate()
        .any(|(idx, instr)| reachable[idx] && vararg_register_disqualifies(instr, vararg_reg))
    {
        return false;
    }
    for instr in instrs.iter_mut() {
        let replacement = match instr {
            Instr::GetIndex(dst, base, key) if *base == vararg_reg => {
                Some(Instr::VarargIndexGet(*dst, *key))
            }
            Instr::SetIndex(base, key, value) if *base == vararg_reg => {
                Some(Instr::VarargIndexSet(*key, *value))
            }
            Instr::GetField(dst, base, name) if *base == vararg_reg => {
                Some(Instr::VarargFieldGet(*dst, *name))
            }
            Instr::SetField(base, name, value) if *base == vararg_reg => {
                Some(Instr::VarargFieldSet(*name, *value))
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            *instr = replacement;
        }
    }
    true
}

/// A conservative reachability pass over one function's flat `instrs`: which
/// indices can actually execute, starting from entry (index 0) and following
/// fallthrough plus every `Jump`/`JumpIfFalse`/`JumpIfTrue`/`ForPrep`/
/// `ForLoop`/`TForLoop` target (computed the same way `dispatch_step` resolves
/// them: `target = pc as i32 + delta`). `Return`/`TailCall`/unconditional
/// `Jump` end fallthrough into the next index; every other instruction
/// (including the conditional jumps, which may or may not branch) preserves
/// it. An index reachable only via a jump from an instruction this same pass
/// has already deemed unreachable is still marked reachable - jump targets
/// are collected from every instruction up front, not gated on that
/// instruction's own liveness - so this never *under*-approximates
/// reachability, only ever over-approximates it (the only direction that's
/// safe for `try_lower_vararg_to_lazy_view`'s disqualification scan to err
/// in).
fn compute_reachable(instrs: &[Instr]) -> Vec<bool> {
    let len = instrs.len();
    let mut jump_targets = vec![false; len];
    for (i, instr) in instrs.iter().enumerate() {
        let delta = match instr {
            Instr::Jump(delta)
            | Instr::JumpIfFalse(_, delta)
            | Instr::JumpIfTrue(_, delta)
            | Instr::ForPrep(_, delta)
            | Instr::ForLoop(_, delta)
            | Instr::TForLoop(_, delta) => Some(*delta),
            _ => None,
        };
        if let Some(delta) = delta {
            let target = i as i64 + delta as i64;
            if target >= 0 && (target as usize) < len {
                jump_targets[target as usize] = true;
            }
        }
    }
    let mut reachable = vec![false; len];
    let mut alive = true;
    for i in 0..len {
        if jump_targets[i] {
            alive = true;
        }
        reachable[i] = alive;
        alive &= !matches!(instrs[i], Instr::Jump(_) | Instr::Return(_, _) | Instr::TailCall(_, _));
    }
    reachable
}

/// The per-instruction half of `try_lower_vararg_to_lazy_view`'s safety
/// scan. Exhaustive over `Instr` by construction (no wildcard arm), so
/// adding a new opcode forces this match to be revisited rather than
/// silently defaulting to "safe". `GetIndex`/`SetIndex`/`GetField`/
/// `SetField` are the only variants where `reg` appearing as the table
/// operand (`base`) alone doesn't disqualify; every other operand position
/// on those four, and every operand position on every other variant,
/// disqualifies unconditionally. `ForPrep`/`ForLoop`/`TForCall`/`TForLoop`
/// address an open-ended register range starting at their `base` operand;
/// this compiler's stack discipline guarantees a persistent local's register
/// never coincides with a temporary allocated after it for the local's
/// lifetime, so `reg >= base` conservatively (and soundly) disqualifies
/// without needing each instruction's precise upper bound.
///
/// `Call`/`TailCall`/`Return`/`Vararg` instead carry an explicit
/// `ValueCount`, so their touched range is checked precisely via
/// `value_count_range_hits` rather than the open-ended `reg >= base`: a
/// `Fixed(0)` count (e.g. the synthetic `Return(0, ValueCount::ZERO)` every
/// function body ends with as a fallback "return nothing", regardless of how
/// many registers the function actually uses) touches no registers at all,
/// and treating its `base` as a real lower bound would wrongly disqualify
/// any function whose vararg register is anything other than 0.
fn vararg_register_disqualifies(instr: &Instr, reg: Reg) -> bool {
    match instr {
        Instr::GetIndex(dst, base, key) if *base == reg => *dst == reg || *key == reg,
        Instr::SetIndex(base, key, value) if *base == reg => *key == reg || *value == reg,
        Instr::GetField(dst, base, _) if *base == reg => *dst == reg,
        Instr::SetField(base, _, value) if *base == reg => *value == reg,

        // `base` (the callee) is always read regardless of `arguments`.
        Instr::Call(base, arguments, results) => {
            reg == *base
                || value_count_range_hits(reg, *base + 1, *arguments)
                || value_count_range_hits(reg, *base, *results)
        }
        Instr::TailCall(base, arguments) => {
            reg == *base || value_count_range_hits(reg, *base + 1, *arguments)
        }
        Instr::Return(base, count) => value_count_range_hits(reg, *base, *count),
        Instr::Vararg(base, count) => value_count_range_hits(reg, *base, *count),
        Instr::ForPrep(base, _) => reg >= *base,
        Instr::ForLoop(base, _) => reg >= *base,
        Instr::TForCall(base, _) => reg >= *base,
        Instr::TForLoop(base, _) => reg >= *base,

        Instr::LoadConst(dst, _) => *dst == reg,
        Instr::LoadNil(dst) => *dst == reg,
        Instr::LoadBool(dst, _) => *dst == reg,
        Instr::Move(dst, src) => *dst == reg || *src == reg,
        Instr::NewLocal(dst, src, _) => *dst == reg || *src == reg,
        Instr::DetachCell(dst) => *dst == reg,
        Instr::GetUpval(dst, _) => *dst == reg,
        Instr::SetUpval(_, src) => *src == reg,
        Instr::GetEnvironment(dst) => *dst == reg,
        Instr::SetEnvironment(src) => *src == reg,
        Instr::GetGlobal(dst, _) => *dst == reg,
        Instr::SetGlobal(_, src, _, _) => *src == reg,
        Instr::ErrorIfGlobalDefined(src, _) => *src == reg,
        Instr::NewTable(dst) => *dst == reg,
        Instr::NewClosure(dst, _) => *dst == reg,
        // Reached only when the guarded arms above didn't match (i.e.
        // `base != reg`), so any occurrence here is `reg` used in an
        // operand position this optimization never rewrites on some
        // *other* table's access (e.g. `reg` is the key, or the dst of an
        // unrelated lookup) - disqualifying.
        Instr::GetField(dst, base, _) => *dst == reg || *base == reg,
        Instr::SetField(base, _, value) => *base == reg || *value == reg,
        Instr::GetIndex(dst, base, key) => *dst == reg || *base == reg || *key == reg,
        Instr::SetIndex(base, key, value) => *base == reg || *key == reg || *value == reg,
        Instr::SetArrayItem(base, _, src) => *base == reg || *src == reg,
        Instr::SetArrayMulti(base, _, src) => *base == reg || *src == reg,
        Instr::Len(dst, src) => *dst == reg || *src == reg,
        Instr::Not(dst, src) => *dst == reg || *src == reg,
        Instr::Neg(dst, src) => *dst == reg || *src == reg,
        Instr::BitNot(dst, src) => *dst == reg || *src == reg,
        Instr::Binary(_, dst, lhs, rhs) => *dst == reg || *lhs == reg || *rhs == reg,
        Instr::IntegerBinary(_, dst, lhs, rhs) => *dst == reg || *lhs == reg || *rhs == reg,
        Instr::Jump(_) => false,
        Instr::JumpIfFalse(src, _) => *src == reg,
        Instr::JumpIfTrue(src, _) => *src == reg,
        Instr::MarkClose(src, _) => *src == reg,
        Instr::CloseSlots(_) => false,
        // These 4 are only ever produced BY this same pass - never present
        // in `state.instrs` when the scan runs - but matched exhaustively
        // rather than wildcarded away, in case a future caller ever runs
        // this scan a second time over already-lowered instructions.
        Instr::VarargIndexGet(dst, key) => *dst == reg || *key == reg,
        Instr::VarargIndexSet(key, value) => *key == reg || *value == reg,
        Instr::VarargFieldGet(dst, _) => *dst == reg,
        Instr::VarargFieldSet(_, value) => *value == reg,
    }
}

/// Whether `reg` falls inside the register range a `ValueCount`-carrying
/// operand touches, starting at `start` (already offset past any leading
/// fixed operand, e.g. `Call`'s callee slot). `Open` means "up to the
/// frame's current top", which isn't known statically, so it conservatively
/// matches every register at or above `start`; `Fixed(n)` matches exactly
/// the `n` registers `[start, start + n)`, including the empty range when
/// `n == 0`.
fn value_count_range_hits(reg: Reg, start: Reg, count: ValueCount) -> bool {
    let reg = reg as u32;
    let start = start as u32;
    match count {
        ValueCount::Fixed(n) => reg >= start && reg < start + n,
        ValueCount::Open => reg >= start,
    }
}
