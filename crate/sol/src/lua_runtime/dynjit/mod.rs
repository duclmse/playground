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

mod abi;
mod lower;
mod stubs;

use std::rc::Rc;

use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};

use crate::lua_bytecode::Proto;

use super::LuaRuntime;

// `NativeStatus` is defined on the `lua_bytecode` side (it lives directly as
// a field on `Proto`, and `lua_bytecode` has no dependency on
// `lua_runtime`); re-exported here so callers can write
// `dynjit::NativeStatus` without reaching across module boundaries.
pub use crate::lua_bytecode::NativeStatus;

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

fn jit_log_enabled() -> bool {
    std::env::var_os("SOL_LUA_JIT_LOG").is_some()
}

pub struct DynJit {
    // `module`/`builder_ctx`/`next_id` aren't read anywhere yet - real
    // lowering (`promote`'s actual body) lands in item 3 and is the first
    // real reader of all three.
    #[allow(dead_code)]
    module: JITModule,
    #[allow(dead_code)]
    builder_ctx: FunctionBuilderContext,
    /// Disambiguates declared function names across distinct `Proto`s -
    /// `cranelift_module::Module::declare_function` requires a unique name
    /// per function, and two `Proto`s can share a `metadata.name` (e.g. two
    /// anonymous functions, or recursive reloads of the same chunk).
    #[allow(dead_code)]
    next_id: u64,
    /// Traces promotion attempts/failures to stderr as they happen, mirroring
    /// the typed tier's `SOL_JIT_LOG`.
    jit_log: bool,
}

impl DynJit {
    /// Builds the module and registers every stub symbol it can ever call
    /// (`stubs::register`) up front, mirroring `crate::jit::Jit::new`'s own
    /// shape - even though, at this point in the milestone, `stubs::register`
    /// has nothing to register yet.
    pub fn new() -> Result<Self, String> {
        let mut builder = JITBuilder::with_flags(
            &[("opt_level", "speed")],
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| e.to_string())?;
        stubs::register(&mut builder);
        let module = JITModule::new(builder);
        Ok(DynJit {
            module,
            builder_ctx: FunctionBuilderContext::new(),
            next_id: 0,
            jit_log: jit_log_enabled(),
        })
    }

    /// Compiles `proto`'s body to native code and returns its entry point.
    ///
    /// Not yet implemented: `lower.rs`'s per-`Instr` lowering pass lands in
    /// item 3 of the plan (leaf-instruction lowering), extended through item
    /// 5 (calls). Until then this always fails, and callers
    /// (`LuaRuntime::try_promote` below) must treat that as "stay
    /// interpreted," not a hard error - a `Proto` this JIT can't yet (or can
    /// never, for a compile failure) handle natively is always safe to keep
    /// running in the existing interpreter.
    pub fn promote(&mut self, proto: &Rc<Proto>) -> Result<*const u8, String> {
        if self.jit_log {
            eprintln!(
                "[dynjit] promotion requested for '{}' (line {}) - lowering not yet implemented",
                proto.metadata.name, proto.line_defined
            );
        }
        Err("dynamic JIT lowering not yet implemented".to_string())
    }

    /// Reserves and returns a name guaranteed unique across every function
    /// this `DynJit` instance ever declares - see `next_id`'s doc comment.
    /// First real caller lands in item 3, once `promote` actually declares
    /// functions on `self.module`.
    #[allow(dead_code)]
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

impl LuaRuntime {
    /// Called from `new_lua_frame` once `proto.call_count` has just crossed
    /// `promote_threshold()`. Lazily builds this runtime's `DynJit` on first
    /// use (see `DynJitState`'s doc), then attempts to promote `proto`.
    ///
    /// Returns whether `proto` is now `NativeStatus::Native` - always
    /// `false` for now, since `DynJit::promote` itself always fails until
    /// item 3 lands real lowering. This hook exists ahead of that purely to
    /// exercise the lazy-init/threshold-crossing wiring in isolation first.
    pub(super) fn try_promote(&mut self, proto: &Rc<Proto>) -> bool {
        if proto.native_status.get() != NativeStatus::Interpreted {
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
                true
            }
            Err(_) => {
                proto.native_status.set(NativeStatus::Interpreted);
                false
            }
        }
    }
}
