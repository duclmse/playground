//! Per-function compiler state: register allocation, block-scope stack, and
//! goto/label scope resolution. Almost entirely self-contained, except that
//! `Compiler` methods elsewhere reach directly into a `FuncState`'s fields
//! too (e.g. numeric-/generic-for loop compilation pushes directly onto
//! `scopes.last_mut().unwrap().locals`), so the fields below are
//! `pub(super)` rather than private.

use std::collections::HashMap;
use std::rc::Rc;

use super::{Const, Instr, Proto, Reg, UpvalSource};

/// A label's recorded offset plus the function-wide count of active locals
/// (`nactive`) at the point it's considered declared, and its source line
/// (for duplicate-label error messages). `nactive` normally counts every
/// local active anywhere in the function at the label's textual position,
/// but a label that is the last statement of its immediate block (the
/// common `goto continue`/`::continue::`-at-end-of-loop idiom) instead uses
/// the active-local count as of that block's *start*, excluding locals the
/// block itself declared - matching real Lua's exception that lets a
/// `goto` legally skip over a block's own locals when jumping to its very
/// end.
#[derive(Clone, Copy)]
struct LabelInfo {
    offset: usize,
    nactive: usize,
    line: u32,
}

/// A not-yet-resolved `goto`'s patch site plus the function-wide count of
/// active locals (`nactive`) at the goto statement itself, and its source
/// line - compared against a matching label's `nactive` to detect a `goto`
/// that illegally jumps into the scope of a local.
struct PendingGoto {
    name: String,
    patch_site: usize,
    nactive: usize,
    line: u32,
}

/// A `global` declaration statement's effect on name resolution, tracked as
/// one entry per declared binding directly in the declaring `BlockScope` -
/// mirroring real Lua's `Vardesc` entries in `lparser.c`, which live in the
/// very same per-function active-variable stack as ordinary locals and are
/// popped when their block closes, rather than a separate whole-compile
/// structure. This is what lets a `global X` inside a nested `do...end`
/// correctly revert once that block's `end` is reached.
#[derive(Clone)]
pub(super) enum GlobalDeclEntry {
    /// `global NAME` (or `global NAME <const>`): declares one bareword name.
    /// Shadows an outer local of the same name for the rest of this scope
    /// (see `find_local_or_global`), and satisfies the strict "must be
    /// declared" check for `NAME` specifically. `<const>`-ness of a *named*
    /// declaration is tracked separately via `environment_constants`, which
    /// is unaffected by this entry.
    Named(String),
    /// `global [<const>] *`: does not name anything, so it never shadows a
    /// local, but it satisfies the strict check for *every* name, and
    /// (unless a more specific `Named` declaration for that name exists)
    /// determines whether assigning that name is a const violation.
    Collective { constant: bool },
    /// `global none`: declares zero names, but - like any other `global`
    /// statement - turns on strict declared-only checking for the rest of
    /// this scope.
    NoGlobals,
}

impl GlobalDeclEntry {
    /// Display label for goto-scope-violation error messages (`local_name_at`).
    fn label(&self) -> &str {
        match self {
            GlobalDeclEntry::Named(name) => name,
            GlobalDeclEntry::Collective { .. } => "*",
            GlobalDeclEntry::NoGlobals => "none",
        }
    }
}

/// Outcome of scanning active `global`-declaration entries (innermost scope
/// and innermost function first) for one name access, mirroring real Lua's
/// tri-state `searchvar` result: `preambular` (-1, unrestricted legacy
/// access - no relevant declaration seen at all), `invalidated` (-2, some
/// *other* name was declared with no collective override active, so this
/// access must be rejected), or `Found` (a declaration governs this specific
/// name, carrying its allowed const-ness).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum GlobalScanState {
    Preambular,
    Invalidated,
    Found(bool),
}

/// Result of `find_local_or_global`: either an ordinary local (register +
/// const-ness) or a `global NAME` declaration shadowing that name.
pub(super) enum LocalOrGlobal {
    Local(Reg, bool),
    GlobalDecl,
}

pub(super) struct BlockScope {
    pub(super) locals: Vec<(String, Reg, bool)>,
    /// `global` declarations made directly in this scope, in declaration
    /// order. Each still occupies a slot in the live active-variable count
    /// (see `active_local_count`) and is discarded when this scope closes,
    /// exactly like a real `local` - see `check_goto_scope`'s `"scope of
    /// '*'"` case - but unlike `locals`, these bind no register and (for
    /// `Named`) are consulted specially by `find_local_or_global` rather
    /// than by plain `find_local`.
    pub(super) global_decls: Vec<GlobalDeclEntry>,
    known_labels: HashMap<String, LabelInfo>,
    unresolved_gotos: Vec<PendingGoto>,
    pub(super) saved_next_reg: Reg,
    /// Number of `<close>` locals (or, for a generic-for's synthetic outer
    /// loop scope, its implicit 4th iterator value) declared directly in
    /// this scope. `pop_scope` emits `Instr::CloseSlots(close_count)` when
    /// this is nonzero.
    pub(super) close_count: u16,
    /// `active_local_count()` at the moment this scope was pushed, i.e. the
    /// live active-local count as of this block's start, before any of the
    /// block's own locals. Used for a label that is the last
    /// statement of this block: real Lua lets a `goto` skip over that
    /// block's own local declarations to reach such a label (the common
    /// `goto continue`/`::continue::`-at-end-of-loop idiom), so that
    /// label's effective `nactive` excludes this block's own locals.
    nactive_at_start: usize,
}

pub(super) struct LoopCtx {
    pub(super) break_patches: Vec<usize>,
    /// `scopes.len()` immediately before this loop's own scope(s) were
    /// pushed, so `break` can sum `close_count` over `scopes[scope_depth..]`
    /// to know how many pending to-be-closed values it must close itself
    /// before jumping past the loop (a `break` skips the scopes' own normal
    /// `CloseSlots`, so it has to redo that closing inline).
    pub(super) scope_depth: usize,
    /// One past the highest register `alloc_reg` has handed out anywhere in
    /// this loop's body so far. A loop's body is compiled once but executed
    /// repeatedly, so code textually *before* a `local` in the body (e.g. a
    /// guard `if` at the top of a `while`) re-runs on every later iteration,
    /// including ones that skip past that `local`'s own (re-)initialization
    /// via an early `return`/`break`/`goto`. If a later iteration's guard
    /// then reused a temporary register that an earlier iteration's `local`
    /// had captured into a cell (ordinary stack-discipline recycling would
    /// happily hand that freed slot back out), it would clobber the live
    /// closure's captured value with unrelated scratch data. Clamping
    /// `next_reg` to never drop below this floor while still inside the
    /// loop - mirroring `retired_floor`'s same "a few extra registers,
    /// never correctness" tradeoff - keeps every register number the loop
    /// has ever used off-limits for reuse within that same loop, so a
    /// register only becomes a captured local's slot if nothing earlier in
    /// the loop body ever touched it first.
    pub(super) reg_floor: Reg,
}

pub(super) struct FuncState {
    pub(super) instrs: Vec<Instr>,
    pub(super) lines: Vec<u32>,
    pub(super) consts: Vec<Const>,
    pub(super) upvals: Vec<UpvalSource>,
    pub(super) upval_names: Vec<String>,
    upval_constants: Vec<bool>,
    pub(super) nested: Vec<Rc<Proto>>,
    pub(super) scopes: Vec<BlockScope>,
    pub(super) loops: Vec<LoopCtx>,
    pub(super) next_reg: Reg,
    pub(super) max_reg: Reg,
    pub(super) num_params: usize,
    pub(super) is_vararg: bool,
    pub(super) vararg_name: Option<Reg>,
    pub(super) name: String,
    pub(super) environment_constants: std::collections::HashSet<String>,
    /// Registers ever captured as a `ParentLocal` upvalue by some nested
    /// closure, discovered as those closures are compiled (see `resolve`).
    pub(super) captured: std::collections::HashSet<Reg>,
    /// One past the highest register index ever captured so far. Ordinary
    /// register recycling (`end_statement`, `pop_scope`, `reset_to`) must
    /// never drop `next_reg` below this: a captured register's cell is
    /// aliased by a live closure for the rest of the *function's* run, not
    /// just the declaring block, so reusing its slot for an unrelated local
    /// or temporary later would let that unrelated write corrupt the
    /// closure's captured value through the shared cell. Retiring the slot
    /// entirely (rather than tracking per-declaration identity) costs a few
    /// extra registers in functions with captures, never correctness.
    pub(super) retired_floor: Reg,
}

impl FuncState {
    pub(super) fn new(name: String) -> Self {
        Self {
            instrs: Vec::new(),
            lines: Vec::new(),
            consts: Vec::new(),
            upvals: Vec::new(),
            upval_names: Vec::new(),
            upval_constants: Vec::new(),
            nested: Vec::new(),
            scopes: Vec::new(),
            loops: Vec::new(),
            next_reg: 0,
            max_reg: 0,
            num_params: 0,
            is_vararg: false,
            vararg_name: None,
            name,
            environment_constants: std::collections::HashSet::new(),
            captured: std::collections::HashSet::new(),
            retired_floor: 0,
        }
    }

    pub(super) fn alloc_reg(&mut self) -> Reg {
        let reg = self.next_reg;
        self.next_reg += 1;
        self.max_reg = self.max_reg.max(self.next_reg);
        if let Some(innermost) = self.loops.last_mut() {
            innermost.reg_floor = innermost.reg_floor.max(self.next_reg);
        }
        reg
    }

    /// The current innermost loop's `reg_floor` (see its doc comment), or 0
    /// outside any loop. Combined with `retired_floor` wherever `next_reg`
    /// is lowered back down after a statement/scope, so ordinary temp
    /// recycling never hands out a register the enclosing loop has already
    /// used earlier in its own body.
    pub(super) fn loop_reg_floor(&self) -> Reg {
        self.loops.last().map(|l| l.reg_floor).unwrap_or(0)
    }

    /// Marks `reg` as captured by a nested closure and permanently retires
    /// it (and everything below it that's currently live) from the
    /// register-recycling pool. See the `retired_floor` field doc.
    pub(super) fn mark_captured(&mut self, reg: Reg) {
        self.captured.insert(reg);
        self.retired_floor = self.retired_floor.max(reg + 1);
    }

    /// Ensures `next_reg > dst`, i.e. `dst` (and everything below it) is
    /// considered allocated, without touching any register's contents.
    pub(super) fn reserve_through(&mut self, dst: Reg) {
        while self.next_reg <= dst {
            self.alloc_reg();
        }
    }

    /// Sets `next_reg` to exactly `dst`, so the *next* `alloc_reg()` call
    /// returns `dst` itself. Used right before compiling an open
    /// multi-value producer (`compile_expr_multi_n(_, -1)`) whose first
    /// internally-allocated register must land exactly at a predetermined
    /// contiguous position — unlike `reserve_through`, which is for
    /// fixed-position single values reached via a `Move` and would leave
    /// `next_reg` one past `dst`, this must not overshoot. Only ever
    /// called when `dst` is provably already `<= next_reg` (every
    /// preceding fixed element's `compile_into` only ever raises
    /// `next_reg`), so this never resurrects a register that's still live.
    ///
    /// Deliberately does *not* clamp against `loop_reg_floor()` the way
    /// `end_statement`/`pop_scope` do: that floor exists to stop a *later
    /// sibling statement* in a loop body from recycling a register a
    /// still-open closure captured earlier in the same loop body, but every
    /// `reset_to` call reclaims registers that were only ever live within
    /// the *same statement* currently being compiled (fixed call/table-ctor
    /// arguments compiled immediately before it) - nothing mid-statement can
    /// have captured them as an upvalue, since closures are only created via
    /// explicit function literals, never as a side effect of argument
    /// marshaling. Clamping here as well would let `loop_reg_floor()`
    /// (bumped by this very statement's own earlier scratch registers)
    /// overshoot `dst`, breaking the "lands exactly at `dst`" contract this
    /// function documents and desyncing every register position downstream
    /// of it - a real, observed panic on nested trailing multi-value calls
    /// inside a loop body (`f(g(h(...)))`, e.g. `db.lua`'s
    /// `string.format("%s", string.rep(...))` under a numeric `for`).
    pub(super) fn reset_to(&mut self, dst: Reg) {
        self.next_reg = dst.max(self.retired_floor);
    }

    pub(super) fn emit(&mut self, instr: Instr, line: u32) -> usize {
        self.instrs.push(instr);
        self.lines.push(line);
        self.instrs.len() - 1
    }

    /// The line of the most recently emitted instruction, if any. Real
    /// Lua's line-info generation (`lcode.c`'s `savelineinfo`) tags every
    /// instruction with `ls->lastline` - the line of the most recent token
    /// actually consumed by the parser - which for a compiler-synthesized
    /// structural instruction with no source token of its own (e.g. the
    /// `OP_JMP` a `test_then_block` emits to skip an `else` branch) ends up
    /// being the line of whatever real statement was parsed immediately
    /// before it, not the enclosing compound statement's own opening line.
    /// This is the equivalent lookup for Sol's already-fully-parsed-AST
    /// compiler: at the point a structural instruction (a control-flow
    /// jump, a `CloseSlots`, a loop back-edge) is emitted, the line of the
    /// last instruction already in `self.instrs` is exactly "the line of
    /// the last real statement compiled so far" - the same value.
    pub(super) fn last_line(&self) -> Option<u32> {
        self.lines.last().copied()
    }

    pub(super) fn push_const(&mut self, value: Const) -> u32 {
        self.consts.push(value);
        (self.consts.len() - 1) as u32
    }

    /// Interns a field/method/global name as a `Const::Str` and returns its
    /// pool index, for `Instr::GetField`/`SetField`'s name operand. Lets the
    /// VM reuse the pooled `Rc<Vec<u8>>` as a table key by cloning the `Rc`
    /// instead of allocating and copying a fresh byte vector on every
    /// execution of the instruction.
    pub(super) fn push_name_const(&mut self, name: &str) -> u32 {
        self.push_const(Const::Str(Rc::new(name.as_bytes().to_vec())))
    }

    /// Live count of locals currently in scope, summed over every
    /// currently-open block (outermost to innermost) - mirrors real Lua's
    /// `fs->nactvar` register-count, which *shrinks* when a block closes
    /// (unlike a simple "declared so far" tally). Used for goto/label scope
    /// checks; see `pop_scope`'s bubble-out clamp for why liveness (not a
    /// monotonic count) is required for correctness.
    pub(super) fn active_local_count(&self) -> usize {
        self.scopes
            .iter()
            .map(|scope| scope.locals.len() + scope.global_decls.len())
            .sum()
    }

    /// Real Lua caps a function's live local-variable count at `MAXVARS`
    /// (200, `lparser.c`), checked once per variable as it enters scope
    /// (`adjustlocalvars`'s `luaY_checklimit`) - so a single `local
    /// a,a,a,...` statement past that count fails to compile at all rather
    /// than silently succeeding. Call this immediately after each new local
    /// is pushed onto the innermost scope's `locals` (or `global_decls`),
    /// matching that same per-variable timing; real Lua's exact wording also
    /// names whether this is the main chunk or a nested function, which
    /// isn't reproduced here since callers only need the `"too many"`
    /// substring `errors.lua`'s `checkerr`-style helpers match on.
    pub(super) fn check_local_variable_limit(&self, line: u32) -> Result<(), String> {
        const MAXVARS: usize = 200;
        if self.active_local_count() > MAXVARS {
            Err(format!(
                "line {line}: too many local variables (limit is {MAXVARS})"
            ))
        } else {
            Ok(())
        }
    }

    /// Finds the name of the local at live index `index` (0-based, counting
    /// from the outermost currently-open scope inward) - the flattened
    /// equivalent of real Lua's `luaF_getlocalname`, evaluated against the
    /// scope stack *as it stands right now*. Must only be called with an
    /// `index` produced by `active_local_count`/`nactive_at_start` taken at
    /// a moment when the scope stack looked the same as it does now (i.e.
    /// from within `check_goto_scope`, at the point the check fires).
    fn local_name_at(&self, index: usize) -> Option<&str> {
        let mut remaining = index;
        for scope in &self.scopes {
            let scope_count = scope.locals.len() + scope.global_decls.len();
            if remaining < scope_count {
                return if remaining < scope.locals.len() {
                    Some(scope.locals[remaining].0.as_str())
                } else {
                    Some(scope.global_decls[remaining - scope.locals.len()].label())
                };
            }
            remaining -= scope_count;
        }
        None
    }

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(BlockScope {
            locals: Vec::new(),
            global_decls: Vec::new(),
            known_labels: HashMap::new(),
            unresolved_gotos: Vec::new(),
            saved_next_reg: self.next_reg,
            close_count: 0,
            nactive_at_start: self.active_local_count(),
        });
    }

    /// Pops the current block scope. If it declared any `<close>` locals,
    /// first emits `Instr::CloseSlots` to close them (this only covers
    /// normal fallthrough exit - a `goto` out of the scope bypasses this and
    /// is a documented limitation; see the `Stmt::Goto` compilation site).
    /// Any goto in it that never found a matching label bubbles up to the
    /// parent scope (or, if this is the outermost scope, is returned as an
    /// error by the caller). A bubbled-out goto that referenced a local
    /// declared *within* the scope now closing has its `nactive` clamped
    /// down to that scope's own starting count - matching real Lua's
    /// `movegotosout` - since that local's scope is now gone entirely and
    /// must not be mistaken for a still-relevant "skipped-over" local by an
    /// enclosing label's scope check.
    pub(super) fn pop_scope(&mut self, line: u32) -> Result<(), String> {
        let scope = self.scopes.pop().expect("scope stack underflow");
        self.next_reg = scope
            .saved_next_reg
            .max(self.retired_floor)
            .max(self.loop_reg_floor());
        if scope.close_count > 0 {
            self.emit(Instr::CloseSlots(scope.close_count), line);
        }
        if self.scopes.last().is_some() {
            for mut pending in scope.unresolved_gotos {
                if pending.nactive > scope.nactive_at_start {
                    pending.nactive = scope.nactive_at_start;
                }
                let parent = self.scopes.last().unwrap();
                if let Some(label) = parent.known_labels.get(&pending.name).copied() {
                    self.check_goto_scope(&pending, &label)?;
                    self.patch_jump(pending.patch_site, label.offset as i32);
                } else {
                    self.scopes
                        .last_mut()
                        .unwrap()
                        .unresolved_gotos
                        .push(pending);
                }
            }
        } else if let Some(pending) = scope.unresolved_gotos.first() {
            return Err(format!(
                "line {}: no visible label '{}' for goto at line {}",
                pending.line, pending.name, pending.line
            ));
        }
        Ok(())
    }

    /// Checks a resolved goto/label pair for a scope violation: a `goto`
    /// may never jump to a point where a local it hadn't seen yet is
    /// already in scope. Matches real Lua's `closegoto` check and error
    /// text (`"<goto NAME> at line L jumps into the scope of 'V'"`).
    /// Must be called while `self.scopes` reflects the label's own live
    /// context (true whenever this fires: same-scope resolution in
    /// `record_label`/`goto_stmt`, or a `pop_scope` bubble-out match, both
    /// happen before anything else changes the scope stack).
    fn check_goto_scope(&self, pending: &PendingGoto, label: &LabelInfo) -> Result<(), String> {
        if pending.nactive < label.nactive {
            let vname = self.local_name_at(pending.nactive).unwrap_or("?");
            return Err(format!(
                "<goto {}> at line {} jumps into the scope of '{}'",
                pending.name, pending.line, vname
            ));
        }
        Ok(())
    }

    fn push_global_decl(&mut self, entry: GlobalDeclEntry) {
        self.scopes
            .last_mut()
            .expect("global declaration outside any scope")
            .global_decls
            .push(entry);
    }

    /// Registers `global NAME` in the current scope: it occupies one slot in
    /// the live active-variable count (same goto-scope rationale as the
    /// collective/`none` forms below) and, for the rest of this scope's
    /// lifetime, shadows an outer local of the same name so an ordinary
    /// bareword write/read of `NAME` resolves as a global (see
    /// `find_local_or_global`) rather than to that outer local.
    pub(super) fn declare_global_named(&mut self, name: &str) {
        self.push_global_decl(GlobalDeclEntry::Named(name.to_string()));
    }

    /// Registers a `global [<const>] *` collective declaration as a
    /// goto-scope pseudo-local, matching real Lua's `errmsg(..., "scope of
    /// '*'")` case.
    pub(super) fn declare_global_collective(&mut self, constant: bool) {
        self.push_global_decl(GlobalDeclEntry::Collective { constant });
    }

    /// Registers a `global none` declaration as a goto-scope pseudo-local
    /// (`errmsg(..., "scope of 'none'")`).
    pub(super) fn declare_global_none(&mut self) {
        self.push_global_decl(GlobalDeclEntry::NoGlobals);
    }

    /// Scans this function's own active `global`-declaration entries,
    /// innermost scope first and innermost-within-scope first (matching
    /// real Lua's `searchvar`, which scans `fs->nactvar - 1` down to `0`),
    /// updating `state` and returning `true` the moment an exact `Named`
    /// match for `name` is found (which short-circuits the whole
    /// cross-function scan in the caller). Otherwise `state` accumulates
    /// `Invalidated`/`Found` per the same rules as `searchvar` and the scan
    /// continues into the enclosing function via the caller's loop.
    pub(super) fn scan_global_decls(&self, name: &str, state: &mut GlobalScanState) -> bool {
        for scope in self.scopes.iter().rev() {
            for entry in scope.global_decls.iter().rev() {
                match entry {
                    GlobalDeclEntry::Named(n) if n == name => {
                        *state = GlobalScanState::Found(false);
                        return true;
                    }
                    GlobalDeclEntry::Named(_) | GlobalDeclEntry::NoGlobals => {
                        if *state == GlobalScanState::Preambular {
                            *state = GlobalScanState::Invalidated;
                        }
                    }
                    GlobalDeclEntry::Collective { constant } => {
                        if !matches!(state, GlobalScanState::Found(_)) {
                            *state = GlobalScanState::Found(*constant);
                        }
                    }
                }
            }
        }
        false
    }

    /// Whether this function has an active `global NAME` declaration
    /// literally named `NAME` (used only for the `_ENV` special case: see
    /// `Compiler::env_declared_as_global`). Unlike `scan_global_decls`, this
    /// is a plain existence check with no invalidation/collective state,
    /// since real Lua's `_ENV`-poisoning only ever triggers on an exact
    /// named match, never on a collective `*` declaration.
    pub(super) fn has_named_global(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope| {
            scope
                .global_decls
                .iter()
                .any(|entry| matches!(entry, GlobalDeclEntry::Named(n) if n == name))
        })
    }

    /// Combined local-or-named-global-declaration lookup, innermost scope
    /// first: within each scope, an active `global NAME` declaration
    /// shadows an outer local also named `NAME` (matching real Lua, where
    /// both live in the same per-function active-variable stack and the
    /// more recently pushed one wins), so this - not plain `find_local` -
    /// is what name resolution (`Compiler::resolve`) must consult.
    pub(super) fn find_local_or_global(&self, name: &str) -> Option<LocalOrGlobal> {
        for scope in self.scopes.iter().rev() {
            for (local_name, reg, constant) in scope.locals.iter().rev() {
                if local_name == name {
                    return Some(LocalOrGlobal::Local(*reg, *constant));
                }
            }
            for entry in scope.global_decls.iter().rev() {
                if let GlobalDeclEntry::Named(n) = entry {
                    if n == name {
                        return Some(LocalOrGlobal::GlobalDecl);
                    }
                }
            }
        }
        None
    }

    pub(super) fn declare_local(&mut self, name: &str, constant: bool) -> Reg {
        let reg = self.alloc_reg();
        self.scopes
            .last_mut()
            .expect("declare_local outside any scope")
            .locals
            .push((name.to_string(), reg, constant));
        reg
    }

    /// `declare_local`, but callable from a `Result<_, String>` context that
    /// wants the `MAXVARS` check applied - see `check_local_variable_limit`.
    /// Only used for parameter/vararg binding (`mod.rs::compile_function`),
    /// which has no natural "innermost scope's own line" the way an ordinary
    /// statement does, so the defining line is passed in explicitly.
    pub(super) fn declare_local_checked(
        &mut self,
        name: &str,
        constant: bool,
        line: u32,
    ) -> Result<Reg, String> {
        let reg = self.declare_local(name, constant);
        self.check_local_variable_limit(line)?;
        Ok(reg)
    }

    pub(super) fn find_upval(&self, name: &str) -> Option<(u16, bool)> {
        self.upval_names
            .iter()
            .position(|n| n == name)
            .map(|i| (i as u16, self.upval_constants[i]))
    }

    pub(super) fn add_upval(&mut self, name: &str, source: UpvalSource, constant: bool) -> u16 {
        self.upvals.push(source);
        self.upval_names.push(name.to_string());
        self.upval_constants.push(constant);
        (self.upvals.len() - 1) as u16
    }

    /// A label must not share a name with any other currently-open (not yet
    /// popped) scope's label, even across nested blocks - sequential
    /// sibling blocks may freely reuse a name, since a scope's labels are
    /// discarded once it pops. Returns the line of the earlier label, for
    /// the "already defined" error.
    fn find_open_label_line(&self, name: &str) -> Option<u32> {
        self.scopes
            .iter()
            .find_map(|scope| scope.known_labels.get(name))
            .map(|label| label.line)
    }

    /// Records a label at the current instruction offset and resolves any
    /// still-pending same-block gotos that named it (a forward reference).
    /// `is_last` marks a label that is the last statement of its immediate
    /// block (before that block's own closing keyword), which gets the
    /// "goto continue" exception - see `BlockScope::nactive_at_start`.
    pub(super) fn record_label(
        &mut self,
        name: &str,
        offset: usize,
        is_last: bool,
        line: u32,
    ) -> Result<(), String> {
        if let Some(earlier_line) = self.find_open_label_line(name) {
            // Real Lua's `checkrepeated` (`lparser.c`) raises this through
            // `luaK_semerror`/`luaX_syntaxerror`, which prefixes it with the
            // position of the *new* (duplicate) label - `line` here, not
            // `earlier_line` - matching every other "line {line}: ..."
            // compile error in this module (`format_chunk_diagnostic`
            // recognizes that prefix and turns it into the usual
            // `chunk:line:` form). Real Lua's own `earlier_line` equivalent
            // is itself an off-by-however-many-lines quirk (a label's stored
            // line is `ls->linenumber` read only after the parser's
            // one-token lookahead has already skipped ahead to whatever
            // comes next, so blank lines/comments between the two labels
            // shift it forward) that isn't reproduced here - only the
            // outer position prefix is.
            return Err(format!(
                "line {line}: label '{name}' already defined on line {earlier_line}"
            ));
        }
        let scope = self.scopes.last().expect("label outside any scope");
        let nactive = if is_last {
            scope.nactive_at_start
        } else {
            self.active_local_count()
        };
        let label = LabelInfo {
            offset,
            nactive,
            line,
        };
        let pending = std::mem::take(&mut self.scopes.last_mut().unwrap().unresolved_gotos);
        let mut still_pending = Vec::with_capacity(pending.len());
        for goto in pending {
            if goto.name == name {
                self.check_goto_scope(&goto, &label)?;
                self.patch_jump(goto.patch_site, offset as i32);
            } else {
                still_pending.push(goto);
            }
        }
        let scope = self.scopes.last_mut().unwrap();
        scope.unresolved_gotos = still_pending;
        scope.known_labels.insert(name.to_string(), label);
        Ok(())
    }

    /// Resolves a `goto` against the current scope's already-known labels
    /// (a backward reference); otherwise defers it, to be resolved by a
    /// later `record_label` in this scope or bubbled up to an enclosing one
    /// by `pop_scope`.
    pub(super) fn goto_stmt(
        &mut self,
        name: &str,
        patch_site: usize,
        line: u32,
    ) -> Result<(), String> {
        let nactive = self.active_local_count();
        if let Some(label) = self.scopes.last().unwrap().known_labels.get(name).copied() {
            let pending = PendingGoto {
                name: name.to_string(),
                patch_site,
                nactive,
                line,
            };
            self.check_goto_scope(&pending, &label)?;
            self.patch_jump(patch_site, label.offset as i32);
        } else {
            self.scopes
                .last_mut()
                .unwrap()
                .unresolved_gotos
                .push(PendingGoto {
                    name: name.to_string(),
                    patch_site,
                    nactive,
                    line,
                });
        }
        Ok(())
    }

    pub(super) fn patch_jump(&mut self, patch_site: usize, target: i32) {
        let delta = target - patch_site as i32;
        match &mut self.instrs[patch_site] {
            Instr::Jump(t)
            | Instr::JumpIfFalse(_, t)
            | Instr::JumpIfTrue(_, t)
            | Instr::ForPrep(_, t) => *t = delta,
            other => panic!("patch_jump on non-branching instruction: {other:?}"),
        }
    }

    pub(super) fn here(&self) -> usize {
        self.instrs.len()
    }
}
