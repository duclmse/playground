//! U9: baseline JIT for the dynamic `.lua` bytecode tier. Compiles hot
//! `crate::lua_bytecode::Proto` bodies to native code via a second,
//! independent `cranelift_jit::JITModule` instance (disjoint symbol-import
//! namespace from the typed tier's own `crate::jit::Jit`, see that module's
//! own doc comment) - see `docs/features/unified-sol-runtime-plan.md`'s U9
//! ledger and the plan this milestone was built from
//! (`/Users/duclm/.claude/plans/enumerated-chasing-flute.md` at the time of
//! writing) for the full design rationale.
//!
//! Unlike the typed tier's `Jit::new(program: TProgram)`, this JIT has no
//! whole-program upfront pass: dynamic `Proto`s are discovered one at a time,
//! as chunks are compiled/loaded, so functions are declared and compiled
//! lazily, one `promote()` call per hot `Proto`, instead of all at
//! construction time.
//!
//! Construction is lazy and fallible-but-recoverable: `LuaRuntime` does not
//! stand up a `DynJit` (or map any executable memory) until the first time a
//! `Proto`'s hot counter crosses `SOL_LUA_PROMOTE_THRESHOLD` - most runtimes
//! (in particular the huge majority of this crate's own unit/integration
//! tests, which run short scripts) never reach that point and so never pay
//! for a `JITModule`. If construction fails (e.g. no executable-memory
//! permission in a sandboxed environment), promotion is disabled for the
//! rest of that `LuaRuntime`'s lifetime and every `Proto` simply stays
//! interpreted forever - the same graceful-fallback shape as an individual
//! `Proto` failing to compile (see `NativeStatus::Interpreted`'s doc below).

#[cfg(feature = "jit")]
mod abi;
#[cfg(feature = "jit")]
mod lower;
#[cfg(feature = "jit")]
mod opt_lower;
#[cfg(feature = "jit")]
mod stubs;

use std::rc::Rc;

#[cfg(feature = "jit")]
use cranelift_frontend::FunctionBuilderContext;
#[cfg(feature = "jit")]
use cranelift_jit::{JITBuilder, JITModule};

use crate::lua_bytecode::Proto;

use super::LuaRuntime;

// `NativeStatus` is defined on the `lua_bytecode` side (it lives directly as
// a field on `Proto`, and `lua_bytecode` has no dependency on
// `lua_runtime`); re-exported here so the `jit`-gated `impl LuaRuntime`
// block below can write bare `NativeStatus::...` instead of reaching across
// module boundaries. `dispatch.rs` imports the same type straight from
// `crate::lua_bytecode` itself, so this re-export has no non-`jit` reader -
// without the `jit` feature, `Proto::native_status` simply never leaves
// `NativeStatus::Interpreted` (see the non-`jit` `DynJitState`/
// `try_promote`/`try_optimize`/`try_osr` below), and nothing in this module
// pattern-matches on it.
#[cfg(feature = "jit")]
pub use crate::lua_bytecode::NativeStatus;

// `abi::NativeFn` is `LuaRuntime::run_native`'s (`lua_runtime/dispatch.rs`)
// only reader outside this module; re-exported here for the same reason as
// `NativeStatus` above. Only meaningful when `run_native` can actually be
// reached, i.e. when native code exists to run - see `run_native`'s own
// `#[cfg(feature = "jit")]` twin in dispatch.rs.
#[cfg(feature = "jit")]
pub(super) use abi::NativeFn;

/// Reads `SOL_LUA_PROMOTE_THRESHOLD` (distinct from the typed tier's own
/// `SOL_PROMOTE_THRESHOLD` - see the plan's "Hot-function counter design"
/// section for why these are deliberately separate env vars), falling back
/// to a default chosen at parity with the typed tier's own threshold
/// (`tier.rs`) pending this tier's own benchmark-driven re-tuning in item 3.
///
/// Read once per process and cached: this is called from `new_lua_frame`,
/// the single choke point for *every* Lua-function activation, so an
/// `env::var` syscall per call would be pure overhead on top of the
/// `call_count` bump it gates.
pub fn promote_threshold() -> u32 {
    static THRESHOLD: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("SOL_LUA_PROMOTE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200)
    })
}

#[cfg_attr(not(feature = "jit"), allow(dead_code))]
fn jit_log_enabled() -> bool {
    std::env::var_os("SOL_LUA_JIT_LOG").is_some()
}

/// Reads `SOL_LUA_OPTIMIZE_THRESHOLD`, gating U10 work item 3's proof-
/// specialized recompile - mirrors `promote_threshold()`'s own cached-env-var
/// pattern exactly. This threshold only starts counting once a `Proto` is
/// already `NativeStatus::Native` (see `LuaRuntime::try_optimize`'s own
/// `optimize_count` increment site in `dispatch.rs`), so it is deliberately
/// a *second* activation count on top of `promote_threshold()`'s own, not a
/// replacement for it - a default an order of magnitude above
/// `promote_threshold()`'s default of 200 means only `Proto`s that stay hot
/// well past their initial baseline promotion pay for a second compile.
pub fn optimize_threshold() -> u32 {
    static THRESHOLD: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("SOL_LUA_OPTIMIZE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2000)
    })
}

/// U10 work item 5 (recompilation-storm guard): whether an activation/
/// backedge counter that has just reached `threshold` should actually
/// trigger a (re-)compile attempt. Separated out from the three call sites
/// that each bump their own counter (`new_lua_frame`'s `call_count`/
/// `optimize_count` in `dispatch.rs`, `try_osr_backedge`'s per-header
/// `osr_counts` in `dispatch/bytecode.rs`) so the exact same bounding rule
/// applies everywhere: a `u32` counter driven by `wrapping_add(1)` forever
/// (every further activation/backedge, for the rest of the process) will
/// equal `threshold` again after a full wraparound cycle even though
/// nothing about the underlying `Proto`'s eligibility can ever change
/// (`Proto::instrs` is immutable) - `already_failed` closes that gap by
/// making a prior failure permanent, rather than relying on a wraparound
/// period of ~4 billion activations never actually being reached in
/// practice. See this module's own `tests` below for the exact
/// pathological scenario this bounds (a counter poised just before
/// wraparound).
pub(super) fn should_attempt(count: u32, threshold: u32, already_failed: bool) -> bool {
    !already_failed && count == threshold
}

/// Reads `SOL_LUA_OSR_THRESHOLD`, gating U10 work item 4's mid-loop OSR
/// entry - mirrors `promote_threshold()`/`optimize_threshold()`'s own
/// cached-env-var pattern. Unlike those two, this counts backward branches
/// taken (`Proto::osr_counts`, bumped once per loop iteration a given loop
/// header is reached - see `try_osr_backedge`), not function activations, so
/// a single long-running call can cross it on its own; the typed tier's own
/// OSR precedent (`tier.rs`'s `DEFAULT_OSR_THRESHOLD`) is likewise lower
/// than its promote threshold for the same reason - a loop iterating inside
/// one activation needs its own, faster-firing signal, independent of how
/// many times the *function itself* has been called.
pub fn osr_threshold() -> u32 {
    static THRESHOLD: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("SOL_LUA_OSR_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    })
}

#[cfg(feature = "jit")]
pub struct DynJit {
    /// U9 item 7 (code-cache lifecycle, "confirm don't newly build"): a
    /// `Proto`'s native code, once `promote`d into this `module`, is never
    /// reclaimed even after the `Proto` itself becomes unreachable -
    /// `cranelift-jit`'s `JITModule` has no per-function `free_function` in
    /// the pinned version this crate uses, so this module only grows for the
    /// lifetime of the owning `LuaRuntime`. There is no content-invalidation
    /// case to handle alongside it: `Proto::instrs` is immutable post-load,
    /// so a promoted function's native code never goes stale out from under
    /// it. This is a known limitation against the exit gate's "code memory
    /// stays within published budgets" criterion - see
    /// `/Users/duclm/.claude/plans/enumerated-chasing-flute.md`'s "Code cache
    /// lifecycle" note for the full rationale - and is deliberately left
    /// unfixed pending real benchmark evidence that it matters, rather than
    /// building eviction machinery speculatively.
    module: JITModule,
    builder_ctx: FunctionBuilderContext,
    /// Every stub's `FuncId`, declared once here and reused via
    /// `declare_func_in_func` by every `lower_proto` call - see
    /// `stubs::StubFuncs`'s own doc.
    stub_funcs: stubs::StubFuncs,
    /// Disambiguates declared function names across distinct `Proto`s -
    /// `cranelift_module::Module::declare_function` requires a unique name
    /// per function, and two `Proto`s can share a `metadata.name` (e.g. two
    /// anonymous functions, or recursive reloads of the same chunk).
    next_id: u64,
    /// Traces promotion attempts/failures to stderr as they happen, mirroring
    /// the typed tier's `SOL_JIT_LOG`.
    jit_log: bool,
}

#[cfg(feature = "jit")]
impl DynJit {
    /// Builds the module and registers every stub symbol it can ever call
    /// (`stubs::register`) up front, mirroring `crate::jit::Jit::new`'s own
    /// shape.
    pub fn new() -> Result<Self, String> {
        let mut builder = JITBuilder::with_flags(
            &[("opt_level", "speed")],
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| e.to_string())?;
        stubs::register(&mut builder);
        let mut module = JITModule::new(builder);
        let stub_funcs = stubs::declare(&mut module)?;
        Ok(DynJit {
            module,
            builder_ctx: FunctionBuilderContext::new(),
            stub_funcs,
            next_id: 0,
            jit_log: jit_log_enabled(),
        })
    }

    /// Compiles `proto`'s body to native code and returns its entry point.
    ///
    /// Restricted to item 3's eligible instruction set (`lower::is_eligible`)
    /// - anything outside it (calls, table/global/upvalue access, captured
    /// registers) fails here, and callers (`LuaRuntime::try_promote` below)
    /// must treat that as "stay interpreted," not a hard error. Extended
    /// through item 5 to lift that restriction entirely.
    pub fn promote(&mut self, proto: &Rc<Proto>) -> Result<*const u8, String> {
        if self.jit_log {
            eprintln!(
                "[dynjit] promotion requested for '{}' (line {})",
                proto.metadata.name, proto.line_defined
            );
        }
        if !lower::is_eligible(proto) {
            if self.jit_log {
                eprintln!(
                    "[dynjit] '{}' is not eligible for item-3 lowering (calls, captured \
                     registers, table/global access, or an out-of-range Return)",
                    proto.metadata.name
                );
            }
            return Err("not eligible for leaf-instruction lowering".to_string());
        }
        let name = self.fresh_name(proto);
        let func_id = lower::lower_proto(
            &mut self.module,
            &mut self.builder_ctx,
            &self.stub_funcs,
            proto,
            &name,
        )?;
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        let ptr = self.module.get_finalized_function(func_id);
        if self.jit_log {
            eprintln!(
                "[dynjit] '{}' promoted to native code as '{name}'",
                proto.metadata.name
            );
        }
        Ok(ptr)
    }

    /// Compiles `proto`'s body a second time, through the proof-specialized
    /// `opt_lower` path, and returns its entry point - the optimizing-tier
    /// counterpart to `promote` above. Shares this same `module`/
    /// `builder_ctx`/`stub_funcs` rather than standing up a second
    /// `JITModule`: both tiers declare functions into one symbol namespace,
    /// and `opt_lower::lower_proto` only ever reuses stubs `stub_funcs`
    /// already declared for `promote`'s own lowering.
    ///
    /// Restricted to `opt_lower::is_eligible`'s subset (pure arithmetic/
    /// branch/return `Proto`s `sol_ir::lift_proto` fully models - see that
    /// module's own doc) - a strict *subset* of `lower::is_eligible`'s own
    /// whitelist, not a superset, so a `Proto` ineligible here can still be
    /// (and in practice almost always already is, by the time this runs -
    /// see `optimize_threshold`'s doc) eligible for the baseline tier.
    pub fn optimize(&mut self, proto: &Rc<Proto>) -> Result<*const u8, String> {
        if self.jit_log {
            eprintln!(
                "[dynjit] optimization requested for '{}' (line {})",
                proto.metadata.name, proto.line_defined
            );
        }
        if !opt_lower::is_eligible(proto) {
            if self.jit_log {
                eprintln!(
                    "[dynjit] '{}' is not eligible for item-3 optimizing lowering (anything \
                     beyond arithmetic/branch/return - calls, table/global/upvalue access, \
                     closures, or `for` loops)",
                    proto.metadata.name
                );
            }
            return Err("not eligible for optimizing lowering".to_string());
        }
        let name = self.fresh_name(proto);
        let func_id = opt_lower::lower_proto(
            &mut self.module,
            &mut self.builder_ctx,
            &self.stub_funcs,
            proto,
            &name,
        )?;
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        let ptr = self.module.get_finalized_function(func_id);
        if self.jit_log {
            eprintln!(
                "[dynjit] '{}' optimized to native code as '{name}'",
                proto.metadata.name
            );
        }
        Ok(ptr)
    }

    /// Compiles a U10 work item 4 OSR entry for the loop headed at bytecode
    /// `header_pc` inside `proto`, and returns its entry point. Shares this
    /// same `module`/`builder_ctx`/`stub_funcs` with `promote`/`optimize`,
    /// same rationale as `optimize`'s own doc.
    ///
    /// Gated on the *same* eligibility as `optimize` (`opt_lower::is_eligible`
    /// - pure arithmetic/branch/return/loop `Proto`s `sol_ir::lift_proto`
    /// fully models), since the OSR entry is compiled by that same lowering,
    /// just starting at a different block. Callers (`LuaRuntime::try_osr`)
    /// must treat an `Err` here as "this loop stays interpreted," exactly
    /// like a failed `promote`/`optimize` call.
    pub fn osr_compile(&mut self, proto: &Rc<Proto>, header_pc: usize) -> Result<*const u8, String> {
        if self.jit_log {
            eprintln!(
                "[dynjit] OSR entry requested for '{}' at loop header pc {header_pc}",
                proto.metadata.name
            );
        }
        if !opt_lower::is_eligible(proto) {
            if self.jit_log {
                eprintln!(
                    "[dynjit] '{}' is not eligible for OSR (same restriction as item 3's \
                     optimizing tier - anything beyond arithmetic/branch/return/loop)",
                    proto.metadata.name
                );
            }
            return Err("not eligible for OSR lowering".to_string());
        }
        let name = self.fresh_name(proto);
        let func_id = opt_lower::lower_osr_entry(
            &mut self.module,
            &mut self.builder_ctx,
            &self.stub_funcs,
            proto,
            &name,
            header_pc,
        )?;
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        let ptr = self.module.get_finalized_function(func_id);
        if self.jit_log {
            eprintln!(
                "[dynjit] '{}' gained an OSR entry at loop header pc {header_pc} ('{name}')",
                proto.metadata.name
            );
        }
        Ok(ptr)
    }

    /// Reserves and returns a name guaranteed unique across every function
    /// this `DynJit` instance ever declares - see `next_id`'s doc comment.
    fn fresh_name(&mut self, proto: &Proto) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("dynjit_{}_{id}", proto.metadata.name)
    }
}

/// `LuaRuntime::dynjit`'s field type. A `DynJit` owns executable memory, so
/// construction is deferred until the first `Proto` actually earns it (see
/// this module's own doc comment) rather than built eagerly in the
/// infallible `LuaRuntime::with_budgets` constructor.
#[cfg(feature = "jit")]
pub(super) enum DynJitState {
    /// No promotion attempted yet this runtime's lifetime.
    Uninit,
    /// `DynJit::new()` succeeded; available for `promote` calls.
    Ready(DynJit),
    /// `DynJit::new()` failed once (e.g. no executable-memory permission) -
    /// promotion is permanently off for the rest of this runtime's
    /// lifetime; every `Proto` just stays interpreted. No retry: a
    /// per-runtime capability like executable-memory access is not expected
    /// to flip from unavailable to available mid-run.
    Unavailable,
}

/// Without the `jit` feature there is no `DynJit` to ever become `Ready` -
/// this build target (e.g. wasm32-unknown-unknown, see
/// docs/features/milestones/u12-wasm-playground.md) cannot JIT native code
/// at all, so every `Proto` stays interpreted permanently, the same
/// graceful-fallback end state `DynJitState::Unavailable` already models
/// for a runtime-detected failure (e.g. no executable-memory permission)
/// above - this is that same state, just known at compile time instead of
/// after a failed `DynJit::new()` call.
#[cfg(not(feature = "jit"))]
pub(super) enum DynJitState {
    Unavailable,
}

/// `LuaRuntime::with_budgets`'s initial `dynjit` field value - a free
/// function (rather than each `DynJitState` variant construction inline at
/// the call site in `init.rs`) so that site stays identical across both
/// `cfg`s despite the two configs' enums not sharing a variant name for
/// their starting state.
#[cfg(feature = "jit")]
pub(super) fn initial_state() -> DynJitState {
    DynJitState::Uninit
}

#[cfg(not(feature = "jit"))]
pub(super) fn initial_state() -> DynJitState {
    DynJitState::Unavailable
}

#[cfg(feature = "jit")]
impl LuaRuntime {
    /// Called from `new_lua_frame` once `proto.call_count` has just crossed
    /// `promote_threshold()`. Lazily builds this runtime's `DynJit` on first
    /// use (see `DynJitState`'s doc), then attempts to promote `proto`.
    ///
    /// `function` is `proto`'s already-registered `FunctionId` (`new_lua_frame`
    /// always calls `prototype_id` before this, unconditionally, for every
    /// activation - see that method's own doc) - item 7's `ExecutionTier`
    /// bookkeeping: on a successful promotion, `function_registry`'s
    /// descriptor for `proto` flips from the `ExecutionTier::Generic` it was
    /// registered at to `ExecutionTier::Native`, so anything that later reads
    /// the registry (no such reader exists yet - see the U9 milestone doc's
    /// item 7 note on not adding a new introspection surface ahead of need)
    /// sees accurate tier bookkeeping rather than a permanently-stale
    /// `Generic`.
    ///
    /// Returns whether `proto` is now `NativeStatus::Native`.
    pub(super) fn try_promote(&mut self, proto: &Rc<Proto>, function: sol_core::FunctionId) -> bool {
        if proto.native_status.get() != NativeStatus::Interpreted || proto.promotion_failed.get() {
            return false;
        }
        let jit = match &mut self.dynjit {
            DynJitState::Ready(jit) => jit,
            DynJitState::Unavailable => return false,
            DynJitState::Uninit => match DynJit::new() {
                Ok(jit) => {
                    self.dynjit = DynJitState::Ready(jit);
                    match &mut self.dynjit {
                        DynJitState::Ready(jit) => jit,
                        DynJitState::Uninit | DynJitState::Unavailable => unreachable!(
                            "just assigned DynJitState::Ready above"
                        ),
                    }
                }
                Err(err) => {
                    if jit_log_enabled() {
                        eprintln!(
                            "[dynjit] could not initialize the dynamic JIT; disabling \
                             promotion for the rest of this runtime: {err}"
                        );
                    }
                    self.dynjit = DynJitState::Unavailable;
                    return false;
                }
            },
        };
        proto.native_status.set(NativeStatus::Promoting);
        match jit.promote(proto) {
            Ok(ptr) => {
                proto.native_status.set(NativeStatus::Native(ptr));
                self.function_registry
                    .set_tier(function, sol_core::ExecutionTier::Native)
                    .expect(
                        "function was already registered via prototype_id \
                         earlier in this same new_lua_frame call",
                    );
                true
            }
            Err(_) => {
                proto.native_status.set(NativeStatus::Interpreted);
                // U10 work item 5: a failed compile is permanent - see
                // `promotion_failed`'s own doc and `should_attempt` above.
                proto.promotion_failed.set(true);
                false
            }
        }
    }

    /// Called from `new_lua_frame` once `proto.optimize_count` has just
    /// crossed `optimize_threshold()`, mirroring `try_promote`'s own shape -
    /// see that method's doc for the shared `DynJitState` lazy-construction
    /// path, reused unchanged here since `optimize` lives on the same
    /// `DynJit`/`JITModule` `promote` does.
    ///
    /// Gated on `proto` already being `NativeStatus::Native` (not
    /// `Interpreted`): this tier only ever recompiles a `Proto` that has
    /// already earned, and kept, its baseline native code. Unlike
    /// `try_promote`'s failure path, a failed optimizing recompile leaves
    /// `native_status` at its still-valid `Native(ptr)` rather than
    /// regressing to `Interpreted` - there is a perfectly good, already-
    /// finalized baseline entry point sitting right there, and discarding it
    /// over a failed *second* compile would be a pure regression.
    ///
    /// Returns whether `proto` is now `NativeStatus::Optimized`.
    pub(super) fn try_optimize(&mut self, proto: &Rc<Proto>, _function: sol_core::FunctionId) -> bool {
        if proto.optimization_failed.get() {
            return false;
        }
        let Some(baseline_ptr) = (match proto.native_status.get() {
            NativeStatus::Native(ptr) => Some(ptr),
            _ => None,
        }) else {
            return false;
        };
        let jit = match &mut self.dynjit {
            DynJitState::Ready(jit) => jit,
            DynJitState::Unavailable => return false,
            DynJitState::Uninit => match DynJit::new() {
                Ok(jit) => {
                    self.dynjit = DynJitState::Ready(jit);
                    match &mut self.dynjit {
                        DynJitState::Ready(jit) => jit,
                        DynJitState::Uninit | DynJitState::Unavailable => {
                            unreachable!("just assigned DynJitState::Ready above")
                        }
                    }
                }
                Err(err) => {
                    if jit_log_enabled() {
                        eprintln!(
                            "[dynjit] could not initialize the dynamic JIT; disabling \
                             promotion for the rest of this runtime: {err}"
                        );
                    }
                    self.dynjit = DynJitState::Unavailable;
                    return false;
                }
            },
        };
        proto.native_status.set(NativeStatus::Optimizing);
        match jit.optimize(proto) {
            Ok(ptr) => {
                proto.native_status.set(NativeStatus::Optimized(ptr));
                true
            }
            Err(_) => {
                proto.native_status.set(NativeStatus::Native(baseline_ptr));
                // U10 work item 5: permanent, same reasoning as
                // `try_promote`'s own `promotion_failed` above.
                proto.optimization_failed.set(true);
                false
            }
        }
    }

    /// Called from `try_osr_backedge` (`lua_runtime/dispatch/bytecode.rs`)
    /// once a loop header's own backward-branch counter has just crossed
    /// `osr_threshold()` (gated by `should_attempt`, U10 work item 5 - see
    /// that function's doc). Unlike `try_promote`/`try_optimize`, this has no
    /// `NativeStatus` state machine of its own to drive - `Proto::osr_entries`
    /// is a plain success cache keyed by `header_pc`, and `Proto::osr_failed`
    /// is the parallel permanent-failure cache, both checked by the caller
    /// before this is invoked (see that method's doc).
    ///
    /// Returns `None` (and leaves the loop interpreted forever after,
    /// mirroring `try_promote`/`try_optimize`'s own graceful-fallback shape)
    /// on any failure - ineligible `Proto`, or `DynJit` unavailable. The
    /// caller is responsible for recording that failure into
    /// `proto.osr_failed`.
    pub(super) fn try_osr(&mut self, proto: &Rc<Proto>, header_pc: usize) -> Option<*const u8> {
        let jit = match &mut self.dynjit {
            DynJitState::Ready(jit) => jit,
            DynJitState::Unavailable => return None,
            DynJitState::Uninit => match DynJit::new() {
                Ok(jit) => {
                    self.dynjit = DynJitState::Ready(jit);
                    match &mut self.dynjit {
                        DynJitState::Ready(jit) => jit,
                        DynJitState::Uninit | DynJitState::Unavailable => {
                            unreachable!("just assigned DynJitState::Ready above")
                        }
                    }
                }
                Err(err) => {
                    if jit_log_enabled() {
                        eprintln!(
                            "[dynjit] could not initialize the dynamic JIT; disabling \
                             promotion for the rest of this runtime: {err}"
                        );
                    }
                    self.dynjit = DynJitState::Unavailable;
                    return None;
                }
            },
        };
        match jit.osr_compile(proto, header_pc) {
            Ok(ptr) => {
                proto.osr_entries.borrow_mut().insert(header_pc, ptr);
                Some(ptr)
            }
            Err(_) => None,
        }
    }
}

/// Without the `jit` feature, promotion/optimization/OSR can never succeed
/// (there is no `DynJit` to compile anything) - every `Proto` simply stays
/// `NativeStatus::Interpreted` forever, mirroring the `DynJitState::Unavailable`
/// runtime-failure path above, just decided at compile time.
#[cfg(not(feature = "jit"))]
impl LuaRuntime {
    pub(super) fn try_promote(&mut self, _proto: &Rc<Proto>, _function: sol_core::FunctionId) -> bool {
        false
    }

    pub(super) fn try_optimize(&mut self, _proto: &Rc<Proto>, _function: sol_core::FunctionId) -> bool {
        false
    }

    pub(super) fn try_osr(&mut self, _proto: &Rc<Proto>, _header_pc: usize) -> Option<*const u8> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::should_attempt;

    /// U10 work item 5's own stated verification goal: a synthetic
    /// pathological case (a counter that keeps incrementing past threshold
    /// forever) must converge to "never attempt again" rather than retrying
    /// in an unbounded loop. `call_count`/`optimize_count`/`osr_counts` are
    /// all real `u32`s driven by `wrapping_add(1)` with no reset, so the
    /// pathological case this project can actually hit is wraparound: a
    /// counter equals `threshold` once, the attempt fails, and ~4 billion
    /// activations later the counter (having wrapped through 0) equals
    /// `threshold` again. Modeled here directly on the counter arithmetic
    /// (not by actually driving a real `Proto` through 4 billion calls,
    /// which no test budget affords) - this is exactly `should_attempt`'s
    /// own contract, so testing it directly is a faithful, not a weakened,
    /// check of the real behavior.
    #[test]
    fn a_counter_that_wraps_back_to_threshold_after_a_failure_never_attempts_again() {
        let threshold: u32 = 200;

        // First activation sequence: counter climbs from 0 up to `threshold`
        // one activation at a time, exactly like `new_lua_frame`'s own
        // `wrapping_add(1)` loop. Nothing should fire before `threshold`,
        // and it must fire exactly once on reaching it.
        let mut already_failed = false;
        let mut attempts = 0u32;
        let mut count: u32 = 0;
        for _ in 0..threshold {
            count = count.wrapping_add(1);
            if should_attempt(count, threshold, already_failed) {
                attempts += 1;
                already_failed = true; // the one real attempt fails
            }
        }
        assert_eq!(count, threshold);
        assert_eq!(attempts, 1, "must attempt exactly once on reaching threshold");

        // Second sequence: the counter keeps incrementing for the rest of
        // the process's life (every further activation), wraps through
        // `u32::MAX`, and reaches `threshold` again. Simulated by jumping
        // straight to a value a few steps before the wrap (real wraparound
        // takes ~4 billion steps - this reproduces the identical arithmetic
        // `wrapping_add` would reach, just without spending that long doing
        // it) and then stepping the remaining distance back to `threshold`.
        count = u32::MAX - 2;
        let remaining_until_rewrap = u32::MAX.wrapping_sub(count).wrapping_add(threshold).wrapping_add(1);
        for _ in 0..remaining_until_rewrap {
            count = count.wrapping_add(1);
            if should_attempt(count, threshold, already_failed) {
                attempts += 1;
            }
        }
        assert_eq!(count, threshold, "counter must have wrapped back to threshold");
        assert_eq!(
            attempts, 1,
            "a counter re-equaling threshold after a wraparound must not retry an already-failed compile"
        );
    }

    #[test]
    fn a_fresh_counter_reaching_threshold_without_a_prior_failure_does_attempt() {
        assert!(should_attempt(200, 200, false));
    }

    #[test]
    fn a_counter_below_threshold_never_attempts_regardless_of_failure_state() {
        assert!(!should_attempt(199, 200, false));
        assert!(!should_attempt(199, 200, true));
    }
}
