//! The instruction/constant value types: `Const`, `UpvalSource`, `Instr`,
//! and the compiled-function output type `Proto`.

use std::cell::RefCell;
use std::rc::Rc;

use sol_core::{ExecutablePrototype, ObjectId, PrototypeMetadata, SourceMap, ValueCount};

use crate::ast::BinaryOp;

use super::Reg;

/// How many distinct identities a per-call-site inline cache (see
/// `BoundedCache`) remembers before it starts evicting the oldest entry to
/// make room for a new one. This is the sole thing bounding mono -> poly ->
/// megamorphic growth: a call site that cycles through more than `IC_SLOTS`
/// distinct identities just keeps round-robin-evicting, degrading to "always
/// miss, same as an uncached interpreter" rather than growing without bound.
pub const IC_SLOTS: usize = 4;

/// A per-call-site inline cache: up to `IC_SLOTS` entries, oldest evicted
/// first once full. `T` is intentionally not required to be `Copy` (cache
/// payloads hold `Rc`-shared data, e.g. `CallCacheEntry`'s `Rc<Proto>`), so
/// this uses a `RefCell<Vec<T>>` rather than a `Cell`-based slot array.
/// Every lookup re-verifies the cached entry against live state (see each
/// cache's own call site) rather than trusting it indefinitely, so this
/// structure itself never needs invalidation logic - only bounded growth.
#[derive(Debug, Clone)]
pub struct BoundedCache<T>(RefCell<Vec<T>>);

impl<T> Default for BoundedCache<T> {
    fn default() -> Self {
        Self(RefCell::new(Vec::new()))
    }
}

impl<T: Clone> BoundedCache<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a clone of the first entry matching `predicate`, if any.
    pub fn find<F: Fn(&T) -> bool>(&self, predicate: F) -> Option<T> {
        self.0.borrow().iter().find(|entry| predicate(entry)).cloned()
    }

    /// Inserts `entry`, evicting the oldest entry first if already at
    /// `IC_SLOTS` capacity. Returns whether an eviction happened, so callers
    /// can feed U8's `debug.icstats()` eviction counters.
    pub fn insert(&self, entry: T) -> bool {
        let mut entries = self.0.borrow_mut();
        let evicted = entries.len() >= IC_SLOTS;
        if evicted {
            entries.remove(0);
        }
        entries.push(entry);
        evicted
    }

    /// Current entry count - test-only, to assert bounded growth.
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A call-site's cached resolution of a `LuaValue::Closure` callee's
/// `(prototype, upvalue cells)`. Deliberately omits the callee's `Globals`:
/// `Globals` is a `lua_runtime`-layer type, and `Proto` (this module,
/// `lua_bytecode`) has no dependency on `lua_runtime` - embedding it here
/// would be a layering violation, not just a style preference, since
/// `lua_runtime` is the one that depends on `lua_bytecode`, not the reverse.
/// `globals_for_closure` stays an uncached lookup on every call, cache hit or
/// not; only the heap-object borrow + prototype-registry resolve this entry
/// replaces are actually expensive per U7's own measurements.
#[derive(Debug, Clone)]
pub struct CallCacheEntry {
    /// The closure `ObjectId` (generation-checked) this entry was resolved
    /// for. A stale entry whose closure was freed and its slot reused simply
    /// fails this equality check - see `sol_core::ObjectId`'s doc comment.
    pub guard: ObjectId,
    pub prototype: Rc<Proto>,
    pub upvalues: Rc<[std::cell::Cell<ObjectId>]>,
}

/// A `GetField`/`SetField` call site's cached resolution of a table's own
/// raw hash-part slot for that instruction's compile-time-constant field
/// name. Scoped to a plain `LuaValue::Table` base whose raw lookup already
/// found the field on the table's own storage (never via a `__index`/
/// `__newindex` metatable fallback - that path stays uncached, same as the
/// call-target cache leaves `step_result_for_call`'s metamethod chain
/// uncached) - see `sol_core::Heap::table_hash_index_of`/
/// `table_get_at_hash_index`/`table_set_at_hash_index`.
#[derive(Debug, Clone, Copy)]
pub struct FieldCacheEntry {
    /// The table `ObjectId` (generation-checked) this entry was resolved
    /// for - same staleness guarantee as `CallCacheEntry::guard`.
    pub guard: ObjectId,
    /// Index into that table's `TableObject::hash` `IndexMap`.
    pub index: usize,
}

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

/// One source-level local binding and the bytecode range during which it is
/// visible.  This is deliberately separate from register allocation: a
/// register may later be recycled for a temporary or a different lexical
/// binding, while Lua's debug API must expose only the binding live at the
/// suspended instruction.
#[derive(Debug, Clone)]
pub struct LocalDebug {
    pub name: String,
    pub register: Reg,
    /// Inclusive bytecode program counter where the binding becomes live.
    pub start_pc: u32,
    /// Exclusive bytecode program counter where the binding leaves scope.
    pub end_pc: u32,
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
    /// Emitted before the instructions that compute a new local's
    /// initializer, directly into that local's own eventual register (see
    /// `Stmt::Local`/`Stmt::MultiLocal`): clears this register's current
    /// cell association so that write can't alias - and silently corrupt -
    /// whatever cell an earlier loop iteration or sibling scope's `NewLocal`
    /// left captured here. The following `NewLocal` gives it a real fresh
    /// cell again if the register turns out to be captured; a no-op on an
    /// uncaptured register.
    DetachCell(Reg),
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
    /// Lazy named-vararg-view fast path (see `Proto::vararg_lazy`): an
    /// index/field access whose `base` was statically proven (by
    /// `try_lower_vararg_to_lazy_view`) to always be this function's named
    /// vararg parameter register and nothing else, so `base` itself is
    /// implicit rather than an operand. As long as that register hasn't
    /// been materialized into a real `Table` yet (still `Nil`), the VM
    /// answers directly from the frame's `varargs` - a `table.pack`-shaped
    /// read/write with no allocation. `GetIndex`/`SetIndex`'s dynamic `key`
    /// register counterparts:
    VarargIndexGet(Reg, Reg),
    VarargIndexSet(Reg, Reg),
    /// `GetField`/`SetField`'s compile-time-constant-name counterparts (the
    /// `u32` is a `Proto::consts` index, always a `Const::Str`).
    VarargFieldGet(Reg, u32),
    VarargFieldSet(u32, Reg),
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

#[derive(Debug, Clone)]
pub struct Proto {
    pub metadata: PrototypeMetadata,
    pub instrs: Vec<Instr>,
    pub source_map: SourceMap,
    pub consts: Vec<Const>,
    pub upvals: Vec<UpvalSource>,
    /// Source names for `upvals`, kept alongside the capture descriptors so
    /// runtime diagnostics can preserve Lua's `upvalue 'name'` wording.
    pub upval_names: Vec<String>,
    /// Declared parameter names, in register order (`param_names[i]` is
    /// register `i`'s name). Unlike an ordinary `local x = ...`, a
    /// parameter's register is populated by the calling convention itself
    /// with no `Instr::NewLocal` to carry its name, so `describe_register`
    /// (`lua_runtime/dispatch.rs`) cannot recover it by scanning for a
    /// write - this is the minimal, `upval_names`-style debug metadata that
    /// lets it do so anyway, mirroring real Lua's `locvars` (scaled down to
    /// just the one case Sol's bytecode can't otherwise reconstruct).
    pub param_names: Vec<String>,
    /// Lexical local-variable lifetime information for `debug.getlocal` and
    /// `debug.setlocal`.  Parameters are entries beginning at PC zero;
    /// ordinary locals begin at their `NewLocal` instruction and end when
    /// their enclosing compiler scope closes.
    pub locals: Vec<LocalDebug>,
    pub vararg_name: Option<Reg>,
    /// Whether `vararg_name`'s register was statically proven safe to leave
    /// as a lazy view over `LuaFrame::varargs` instead of eagerly building a
    /// real `Table` at call time - see `try_lower_vararg_to_lazy_view` and
    /// the `Instr::Vararg{Index,Field}{Get,Set}` opcodes it emits. Always
    /// `false` when `vararg_name.is_none()`.
    pub vararg_lazy: bool,
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
    /// Per-`pc` inline cache for `Call`/`TailCall`/`TForCall`'s closure-
    /// callee resolution (U8). Same length as `instrs`, indexed by `pc`,
    /// following `source_map`'s existing "parallel side table on `Proto`"
    /// precedent; entries at non-call `pc`s simply stay empty (no
    /// allocation - `BoundedCache::default()` is a zero-capacity `Vec`).
    pub call_cache: Vec<BoundedCache<CallCacheEntry>>,
    /// Per-`pc` inline cache for `GetField`/`SetField`'s raw-hash-slot
    /// resolution (U8). Same length/indexing/empty-by-default convention as
    /// `call_cache`; entries at non-field-access `pc`s simply stay empty.
    pub field_cache: Vec<BoundedCache<FieldCacheEntry>>,
    /// Per-`pc` inline cache for `GetGlobal`/`SetGlobal`'s plain-`_ENV`
    /// raw-hash-slot resolution (U8). Reuses `FieldCacheEntry` and the same
    /// table-field-cache mechanism against the `_ENV` table's own hash part;
    /// a bare `_ENV = newtable` reassignment is caught for free, since the
    /// guard is the `_ENV` table's own `ObjectId` at time of use (read fresh
    /// from `Globals::as_value()` on every access, never cached itself) - a
    /// reassigned `_ENV` simply means a different `ObjectId` is checked
    /// against the cache entry, which then misses. Same length/indexing/
    /// empty-by-default convention as `call_cache`; entries at non-global-
    /// access `pc`s (including every `pc` in a `has_base()` scope, which
    /// always takes the uncached `base`-chain path instead) simply stay
    /// empty.
    pub global_cache: Vec<BoundedCache<FieldCacheEntry>>,
    pub line_defined: u32,
    /// The line of this function's closing `end` - real Lua's
    /// `debug.getinfo`'s `lastlinedefined` (`lua_Debug::lastlinedefined`,
    /// set once in `close_func`/`lparser.c` from the `end`/EOZ token's own
    /// line). Not derived from where the last instruction happens to land -
    /// no code executes *on* the `end` line itself, so scanning instruction
    /// line info can never recover it. From `ast::Function::end_line`.
    pub last_line_defined: u32,
    /// Activation counter for the dynamic baseline JIT (U9): incremented
    /// once per call at `LuaRuntime::new_lua_frame`'s single choke point,
    /// exactly parallel to how `call_cache`/`field_cache`/`global_cache`
    /// already live here as per-`Proto` side tables rather than in some
    /// external `Proto`-identity-keyed map. Read by
    /// `lua_runtime::dynjit::promote_threshold` consumers to decide when to
    /// attempt promotion.
    pub call_count: std::cell::Cell<u32>,
    /// This `Proto`'s current native-code promotion state (U9). Lives here,
    /// rather than in an external cache keyed by `Proto` identity, for the
    /// same reason `call_count` does. A raw pointer inside `Native` is
    /// `Copy`, so a plain `Cell` suffices.
    pub native_status: std::cell::Cell<NativeStatus>,
    /// Second-tier hot-activation counter for the optimizing JIT (U10 work
    /// item 3): incremented once per call *after* `native_status` has
    /// already reached `Native`, mirroring `call_count`'s own single-
    /// choke-point design. Read by `lua_runtime::dynjit::optimize_threshold`
    /// consumers to decide when to attempt a proof-specialized recompile on
    /// top of the already-promoted baseline tier.
    pub optimize_count: std::cell::Cell<u32>,
}

/// A `Proto`'s current dynamic-JIT (U9/U10) promotion state. Defined in
/// `lua_bytecode` (not `lua_runtime`) because it lives directly on `Proto`,
/// and `lua_bytecode` has no dependency on `lua_runtime` -
/// `lua_runtime::dynjit` re-exports this type rather than defining its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStatus {
    /// Running through `dispatch_step` as ordinary bytecode. Either never
    /// promoted, or permanently fell back here after a compile failure (no
    /// retry).
    Interpreted,
    /// A promotion attempt for this `Proto` is in flight.
    Promoting,
    /// Compiled by the baseline tier (U9); the pointer is this `Proto`'s
    /// native entry point, whose calling convention is defined by
    /// `lua_runtime::dynjit::abi`. A `Native` `Proto` is itself eligible for
    /// a further optimizing recompile once it stays hot (see `Optimizing`/
    /// `Optimized` below) - `Native` does not mean "final tier".
    Native(*const u8),
    /// An optimizing recompile (U10 work item 3) on top of an already-
    /// `Native` `Proto` is in flight.
    Optimizing,
    /// Compiled by the optimizing tier (U10 work item 3): proof-specialized
    /// native code sharing the exact same calling convention as `Native`
    /// (`lua_runtime::dynjit::abi::NativeFn`), so every `Native` consumer
    /// (`run_native`) handles `Optimized` identically. Kept as a distinct
    /// variant (rather than overwriting the `Native` pointer in place) so a
    /// failed optimizing recompile can fall back to the still-valid baseline
    /// pointer - see `lua_runtime::dynjit::try_optimize`.
    Optimized(*const u8),
}

impl Default for NativeStatus {
    fn default() -> Self {
        NativeStatus::Interpreted
    }
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

impl Proto {
    /// U8 item 7's "serialize... for inspection" deliverable: a
    /// human-readable text dump of this prototype's (and its nested
    /// closures', recursively) per-call-site inline cache occupancy,
    /// classified mono/poly/megamorphic by how many of `IC_SLOTS` distinct
    /// identities are currently cached. Exposed via `debug.icprofile()`
    /// (`natives_debug.rs`).
    ///
    /// Deliberately dumps occupancy only, not per-call-site hit/miss counts:
    /// no per-pc counter infrastructure exists (only the cross-call-site
    /// aggregate counters `debug.icstats()` exposes do), and nothing in the
    /// dynamic `.lua` path consumes a profile for PGO yet - building one
    /// speculatively is scoped out here the same way U7/U8 deferred other
    /// unmeasured work elsewhere in this plan.
    pub fn ic_profile_dump(&self) -> String {
        let mut out = String::new();
        self.write_ic_profile(&mut out, 0);
        out
    }

    fn write_ic_profile(&self, out: &mut String, depth: usize) {
        use std::fmt::Write;
        let indent = "  ".repeat(depth);
        let _ = writeln!(
            out,
            "{indent}proto '{}' line={} ({} instrs)",
            self.metadata.name,
            self.line_defined,
            self.instrs.len()
        );
        for (pc, cache) in self.call_cache.iter().enumerate() {
            Self::write_cache_line(out, &indent, &self.source_map, pc as u32, "call", cache.len());
        }
        for (pc, cache) in self.field_cache.iter().enumerate() {
            Self::write_cache_line(out, &indent, &self.source_map, pc as u32, "field", cache.len());
        }
        for (pc, cache) in self.global_cache.iter().enumerate() {
            Self::write_cache_line(out, &indent, &self.source_map, pc as u32, "global", cache.len());
        }
        for nested in &self.nested {
            nested.write_ic_profile(out, depth + 1);
        }
    }

    fn write_cache_line(
        out: &mut String,
        indent: &str,
        source_map: &SourceMap,
        pc: u32,
        kind: &str,
        len: usize,
    ) {
        if len == 0 {
            return;
        }
        use std::fmt::Write;
        let state = match len {
            1 => "mono",
            n if n >= IC_SLOTS => "megamorphic",
            _ => "poly",
        };
        let line = source_map.location(pc).map(|loc| loc.line).unwrap_or(0);
        let _ = writeln!(
            out,
            "{indent}  pc={pc} line={line} kind={kind} state={state}({len}/{IC_SLOTS})"
        );
    }
}
