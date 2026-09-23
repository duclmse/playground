//! The bytecode trampoline's dispatch core: `call`/`drive`/`dispatch_step`
//! drive execution one step at a time through the explicit `Frame` stack
//! (see `frame`) instead of native Rust recursion, so execution can be
//! paused/resumed at any point (coroutines, the step debugger). Also holds
//! index/arithmetic/metamethod resolution (`index_resolve`,
//! `set_index_resolve`, `binary_resolve`, `metamethod`) and the
//! frame/register-buffer recycling pools.

use std::cell::RefCell;
use std::rc::Rc;

use sol_core::{FrameHeader, FrameState, FunctionId, ValueCount};

use crate::ast::{BinaryOp, UnaryOp};
use crate::lua_bytecode::{Instr, Proto, Reg, UpvalSource};

use super::frame::*;
use super::util::*;
use super::*;

/// Bound on table-to-table `__index`/`__newindex` fallback chains, matching
/// real Lua's `MAXTAGLOOP` (`lvm.c`) - see `LuaRuntime::index`/`set_index`.
const MAX_METATABLE_CHAIN: usize = 2000;

/// Bound on consecutive `__call` metamethod hops resolved for one call site,
/// matching real Lua 5.5's `MAX_CCMT`/`tryfuncTM` (`ldo.c`): a dedicated
/// bit-packed counter on the `CallInfo`, distinct from and much smaller than
/// `MAX_METATABLE_CHAIN`. Confirmed against the pinned `lua5.5` oracle
/// (`/opt/homebrew/bin/lua5.5`): a 15-hop `__call` chain resolves normally
/// (bottoming out with an ordinary "attempt to call a ... value" once the
/// final link has no `__call` of its own), while a 16-hop chain raises
/// exactly `"'__call' chain too long"` instead - see
/// `step_result_for_call`, and `lua-5.5.1-tests/calls.lua`'s "testing chains
/// too long" case (line ~223), which exercises exactly that boundary.
const MAX_CALL_CHAIN: usize = 15;

mod bytecode;

/// Whether `instr` writes `reg` as one of its destination registers -
/// conservative for multi-result/range-writing instructions (treats the
/// whole plausible range as written) so `describe_register`'s backward scan
/// never walks past a write it can't fully account for.
fn instr_writes(instr: &Instr, reg: Reg) -> bool {
    use Instr::*;
    match instr {
        LoadConst(dst, _)
        | LoadNil(dst)
        | LoadBool(dst, _)
        | Move(dst, _)
        | NewLocal(dst, _, _)
        | GetUpval(dst, _)
        | GetEnvironment(dst)
        | GetGlobal(dst, _)
        | NewTable(dst)
        | NewClosure(dst, _)
        | GetField(dst, _, _)
        | GetIndex(dst, _, _)
        | Len(dst, _)
        | Not(dst, _)
        | Neg(dst, _)
        | BitNot(dst, _)
        | Binary(_, dst, _, _)
        | IntegerBinary(_, dst, _, _) => *dst == reg,
        Call(base, _, _) | TailCall(base, _) | Vararg(base, _) => reg >= *base,
        TForCall(base, nvars) => reg >= base + 3 && reg < base + 3 + nvars,
        ForPrep(base, _) | ForLoop(base, _) => (*base..=base + 3).contains(&reg),
        _ => false,
    }
}

/// Real Lua's `getobjname` (`ldebug.c`), scaled down to the handful of
/// sources worth annotating an "attempt to index/call a nil value" error
/// with: scans backward from just before `pc` for the most recent write to
/// `reg`, chasing through `Move` the way real Lua's bytecode scan does, and
/// gives up (returning `None`, so the error stays unannotated) at the first
/// write it can't name - matching real Lua's own conservative fallback
/// rather than guessing.
fn describe_register(proto: &Proto, pc: usize, reg: Reg) -> Option<String> {
    let mut target = reg;
    // When a dotted name is lowered through a self-overwriting register,
    // retain its final field while chasing the root.  A local root retains
    // Lua's `field 'x'` wording; a global root takes precedence as
    // `global 'x'` (see the branch below).
    let mut dotted_field = None;
    for (index, instr) in proto.instrs[..pc].iter().enumerate().rev() {
        match instr {
            // `compile_expr` emits this self-move only after an `and`/`or`
            // join. The value may have come from either branch, so Lua has
            // no stable source name to expose in a later diagnostic.
            Instr::Move(dst, src) if *dst == target && *src == target => return None,
            Instr::Move(dst, src) if *dst == target => target = *src,
            Instr::NewLocal(dst, _, name_idx) if *dst == target => {
                if let Some(field) = dotted_field {
                    return Some(format!("field '{field}'"));
                }
                let name = String::from_utf8_lossy(&name_const(proto, *name_idx)).into_owned();
                return Some(format!("local '{name}'"));
            }
            Instr::GetUpval(dst, index) if *dst == target => {
                if let Some(field) = dotted_field {
                    return Some(format!("field '{field}'"));
                }
                let name = proto.upval_names.get(*index as usize)?;
                return Some(format!("upvalue '{name}'"));
            }
            Instr::GetGlobal(dst, name) if *dst == target => {
                return Some(format!("global '{name}'"));
            }
            Instr::GetField(dst, receiver, name_idx) if *dst == target => {
                let name = String::from_utf8_lossy(&name_const(proto, *name_idx)).into_owned();
                // A dotted name (`aaa.bbb.cc`) is compiled by
                // `compile_name_into` as a succession of self-overwriting
                // field loads.  It is still the original global/local that
                // Lua identifies when a later index fails (for example,
                // `aaa.bbb:ddd()` reports `global 'aaa'` when `aaa` is
                // nil).  Keep walking through that special shape instead
                // of stopping at its last field.  Ordinary field
                // expressions use a separate destination register and keep
                // their useful `field 'name'` description below.
                if *receiver == target {
                    dotted_field = Some(name);
                    continue;
                }
                // Ordinary global access is `_ENV.name`.  The default
                // environment has its own `GetGlobal` instruction, but a
                // lexically rebound `_ENV` lowers to a field load.  Keep
                // Lua's observable global wording for that special
                // receiver rather than calling it a field of `_ENV`.
                if matches!(
                    describe_register(proto, index, *receiver).as_deref(),
                    Some("local '_ENV'") | Some("upvalue '_ENV'")
                ) {
                    return Some(format!("global '{name}'"));
                }
                // `compile_method_base` emits `GetField(base, receiver,
                // name)` immediately followed by `Move(base + 1, receiver)`
                // to materialize the implicit `self`. That bytecode shape
                // is exactly the call-site distinction Lua exposes as a
                // `method` rather than a plain `field` in diagnostics.
                let is_method = matches!(
                    proto.instrs.get(index + 1),
                    Some(Instr::Move(self_reg, source))
                        if *self_reg == target + 1 && *source == *receiver
                );
                let kind = if is_method { "method" } else { "field" };
                return Some(format!("{kind} '{name}'"));
            }
            other if instr_writes(other, target) => return None,
            _ => {}
        }
    }
    None
}

/// Appends real Lua's "(global 'x')"/"(field 'x')" suffix to a fresh
/// "attempt to index a ... value" error, when the indexed base's source is
/// nameable - never touches an already-annotated or unrelated error (e.g.
/// the metatable-chain-too-long error also raised from this path).
fn annotate_index_error(err: &mut LuaError, proto: &Proto, pc: usize, base: Reg) {
    if err.message.starts_with("attempt to index a ") && err.message.ends_with(" value") {
        if let Some(description) = describe_register(proto, pc, base) {
            err.message = format!("{} ({description})", err.message);
        }
    }
}

/// The call counterpart of `annotate_index_error`. Lua's debug-name lookup
/// uses the instruction that loaded the callee, so a direct global or field
/// call retains its useful source name even when the callee is a non-function.
fn annotate_call_error(err: &mut LuaError, proto: &Proto, pc: usize, base: Reg) {
    if err.message.starts_with("attempt to call a ") && err.message.ends_with(" value") {
        if let Some(description) = describe_register(proto, pc, base) {
            err.message = format!("{} ({description})", err.message);
        }
    }
}

impl LuaRuntime {
    /// Semantic-ABI entry point used by specialized-tier adapters. The
    /// callable remains a normal value in the dynamic global environment;
    /// only scalar boxing policy belongs in the adapter.
    pub fn call_global_outcome(
        &mut self,
        name: &str,
        args: Vec<LuaValue>,
    ) -> sol_core::CallOutcome<LuaValue, LuaError> {
        match self.call(self.globals.get(name), args) {
            Ok(values) => sol_core::CallOutcome::Returned(values),
            Err(error) => sol_core::CallOutcome::Raised(error),
        }
    }

    /// Checked scalar adapter for specialized bytecode/native call slots.
    /// Guards execute once at the tier boundary; the dynamic function body
    /// then uses normal Lua values while the specialized caller keeps raw
    /// register bits.
    pub fn call_global_scalar_outcome(
        &mut self,
        name: &str,
        parameters: &[BridgeScalar],
        result: BridgeScalar,
        arguments: &[u64],
    ) -> sol_core::CallOutcome<u64, String> {
        if parameters.len() != arguments.len() {
            return sol_core::CallOutcome::Raised(format!(
                "'{name}' expects {} argument(s), got {}",
                parameters.len(),
                arguments.len()
            ));
        }
        let arguments = parameters
            .iter()
            .zip(arguments)
            .map(|(kind, bits)| match kind {
                BridgeScalar::I64 => LuaValue::Integer(*bits as i64),
                BridgeScalar::F64 => LuaValue::Float(f64::from_bits(*bits)),
                BridgeScalar::Bool => LuaValue::Bool(*bits != 0),
            })
            .collect();
        match self.call_global_outcome(name, arguments) {
            sol_core::CallOutcome::Returned(values) => {
                let value = values.into_iter().next().unwrap_or(LuaValue::Nil);
                let bits = match (result, &value) {
                    (BridgeScalar::I64, LuaValue::Integer(value)) => Some(*value as u64),
                    (BridgeScalar::F64, LuaValue::Float(value)) => Some(value.to_bits()),
                    (BridgeScalar::F64, LuaValue::Integer(value)) => {
                        Some((*value as f64).to_bits())
                    }
                    (BridgeScalar::Bool, LuaValue::Bool(value)) => Some(*value as u64),
                    _ => None,
                };
                match bits {
                    Some(bits) => sol_core::CallOutcome::Returned(vec![bits]),
                    None => sol_core::CallOutcome::Raised(format!(
                        "dynamic function '{name}' returned {}, expected {result:?}",
                        value.type_name()
                    )),
                }
            }
            sol_core::CallOutcome::Yielded(values) => {
                let mut packed = Vec::with_capacity(values.len());
                for value in values {
                    let bits = match (result, value) {
                        (BridgeScalar::I64, LuaValue::Integer(value)) => Some(value as u64),
                        (BridgeScalar::F64, LuaValue::Float(value)) => Some(value.to_bits()),
                        (BridgeScalar::F64, LuaValue::Integer(value)) => {
                            Some((value as f64).to_bits())
                        }
                        (BridgeScalar::Bool, LuaValue::Bool(value)) => Some(value as u64),
                        _ => None,
                    };
                    let Some(bits) = bits else {
                        return sol_core::CallOutcome::Raised(format!(
                            "dynamic function '{name}' yielded an incompatible value"
                        ));
                    };
                    packed.push(bits);
                }
                sol_core::CallOutcome::Yielded(packed)
            }
            sol_core::CallOutcome::Raised(error) => {
                sol_core::CallOutcome::Raised(error.to_string())
            }
            sol_core::CallOutcome::TailCall(_) => {
                unreachable!("the dynamic trampoline consumes tail calls")
            }
        }
    }

    pub(super) fn call(
        &mut self,
        value: LuaValue,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        if self.call_depth >= self.max_call_depth {
            // Real Lua's own message for this condition is the plain
            // string "stack overflow" (`ldo.c`'s `luaD_growstack`/
            // `luaE_incCstack` via `LUAI_MAXCCALLS`), and a fair number of
            // corpus files (`errors.lua`, `coroutine.lua`, `locals.lua`)
            // assert on `string.find(msg, "stack overflow")` after a deep
            // recursion. Keep the budget wording (which env-var docs and
            // other diagnostics reference) but include the substring real
            // scripts actually search for.
            return Err(LuaError::new(
                "stack overflow (Lua call-depth budget exhausted)",
            ));
        }
        self.call_depth += 1;
        // Hooks fire once per actual callable dispatched here, not for the
        // `__call` metamethod's receiver (the `other` arm below) - that's not
        // really "a function being called" in real Lua's model, and it
        // recurses into `self.call(method, args)`, which fires its own
        // "call"/"return" pair for the resolved `method` itself. Computed via
        // `&value` (match ergonomics, no move) since `value` is still needed
        // by the match below.
        let fires_hook = matches!(
            &value,
            LuaValue::NativeFunction(_)
                | LuaValue::Native(_)
                | LuaValue::RegisteredNative(_)
                | LuaValue::CFunction(_)
                | LuaValue::GMatchIterator(_)
                | LuaValue::CoroutineWrapper(_)
                | LuaValue::Closure(_)
        );
        let result = (|| -> LuaResult<Vec<LuaValue>> {
            if fires_hook {
                self.fire_hook("call", None)?;
            }
            let result = match value {
                LuaValue::NativeFunction(function) => self.call_native(function, args),
                LuaValue::Native(bridge) => {
                    let callable = self.register_native_bridge(bridge)?;
                    self.call_registered_native(callable, args)
                }
                LuaValue::RegisteredNative(callable) => self.call_registered_native(callable, args),
                LuaValue::CFunction(callable) => self.call_c_function(callable, args),
                LuaValue::GMatchIterator(state) => self.call_gmatch_iterator(state),
                LuaValue::CoroutineWrapper(co) => self.call_coroutine_wrapper(co, args),
                LuaValue::Closure(closure) => {
                    let name = closure.proto.metadata.name.clone();
                    let upvals = closure.upvals.borrow().clone();
                    self.call_closure(closure.proto.clone(), upvals, closure.globals.clone(), args)
                        .map_err(|error| error.at(&name))
                }
                other => {
                    if let Some(method) = self.metamethod(&other, b"__call")? {
                        let mut args = args;
                        args.insert(0, other);
                        self.call(method, args)
                    } else {
                        Err(LuaError::new(format!(
                            "attempt to call a {} value",
                            other.type_name()
                        )))
                    }
                }
            };
            if fires_hook && result.is_ok() {
                self.fire_hook("return", None)?;
            }
            result
        })();
        self.call_depth -= 1;
        result
    }

    /// Fires a `debug.sethook` event for whichever hook is active on the
    /// currently-running coroutine (`LuaRuntime::active_hook`), if any is
    /// installed, its mask actually subscribes to `event`, and no hook is
    /// already running - real Lua disables every hook for a hook callback's
    /// own dynamic extent (including anything that callback itself calls),
    /// both to match its documented semantics and to avoid a hook that runs
    /// Lua code recursing into itself forever. `line` is `Some` only for
    /// `"line"` events (and is ignored for every other event) - real Lua's
    /// hook callback receives `(event)` for call/return/count and
    /// `(event, line)` for line events.
    pub(super) fn fire_hook(&mut self, event: &'static str, line: Option<i64>) -> LuaResult<()> {
        c_api::fire_c_hook(self, event, line)?;
        let Some(hook) = self.active_hook.clone() else {
            return Ok(());
        };
        if self.running_hook {
            return Ok(());
        }
        let interested = match event {
            "call" | "tail call" => hook.mask.call,
            "return" => hook.mask.ret,
            "line" => hook.mask.line,
            "count" => hook.mask.count,
            _ => false,
        };
        if !interested {
            return Ok(());
        }
        let mut args = vec![LuaValue::String(Rc::new(event.as_bytes().to_vec()))];
        if let Some(line) = line {
            args.push(LuaValue::Integer(line));
        }
        self.running_hook = true;
        let result = self.call(hook.callback.clone(), args);
        self.running_hook = false;
        result.map(|_| ())
    }

    /// Per-instruction `line`/`count` hook check, called from `dispatch_step`'s
    /// `'exec: loop` right after `frame.header.pc` is set to the instruction
    /// about to execute. Cheap no-op when no hook is active (checked before it
    /// is even called) or when neither mask bit is set. Count-hook decrementing
    /// happens even while a "line" event doesn't fire and vice versa - real
    /// Lua's `hookcount`/line-change checks are independent of each other.
    fn fire_line_and_count_hooks(&mut self, frame: &mut LuaFrame, pc: usize) -> LuaResult<()> {
        c_api::fire_c_instruction_hooks(self, frame, pc)?;
        let Some(hook) = self.active_hook.clone() else {
            return Ok(());
        };
        if hook.mask.count {
            let remaining = hook.count_remaining.get() - 1;
            if remaining <= 0 {
                hook.count_remaining.set(hook.count);
                self.fire_hook("count", None)?;
            } else {
                hook.count_remaining.set(remaining);
            }
        }
        if hook.mask.line {
            if let Some(location) = frame.proto.source_map.location(pc as u32) {
                let line = location.line as i64;
                let pc = pc as i64;
                // Fires again whenever the source line actually changes, or
                // whenever `pc` jumps back to (or before) where the last
                // line-hook fired - a loop's back-edge re-executing an earlier,
                // possibly identical, line. Tracked per-frame
                // (`frame.hook_last_pc`/`hook_last_line`), since each call
                // frame has its own independent notion of "have I already
                // hooked this line".
                if line != frame.hook_last_line || pc <= frame.hook_last_pc {
                    frame.hook_last_line = line;
                    frame.hook_last_pc = pc;
                    self.fire_hook("line", Some(line))?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn register_native_bridge(
        &mut self,
        bridge: Rc<NativeBridge>,
    ) -> LuaResult<sol_core::NativeCallableId> {
        let identity = Rc::as_ptr(&bridge) as usize;
        if let Some(callable) = self.native_bridge_ids.get(&identity) {
            return Ok(*callable);
        }
        let function = self.next_native_function;
        self.next_native_function = function
            .checked_add(1)
            .ok_or_else(|| LuaError::new("native callable ID space exhausted"))?;
        // Provider zero is reserved for the dynamic runtime's standard
        // library. Provider one is this runtime's typed-code host registry.
        let callable = sol_core::NativeCallableId::new(1, function);
        self.native_bridges.insert(callable, bridge);
        self.native_bridge_ids.insert(identity, callable);
        Ok(callable)
    }

    fn call_registered_native(
        &mut self,
        callable: sol_core::NativeCallableId,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let bridge = self.native_bridges.get(&callable).cloned().ok_or_else(|| {
            LuaError::new(format!(
                "native callable provider {} function {} is not registered",
                callable.provider, callable.function
            ))
        })?;
        call_native_bridge(&bridge, args)
    }

    pub(super) fn register_c_function(
        &mut self,
        function: c_api::LuaCFunction,
    ) -> LuaResult<sol_core::NativeCallableId> {
        let identity = function as usize;
        if let Some(callable) = self.c_function_ids.get(&identity) {
            return Ok(*callable);
        }
        let function_id = self.next_native_function;
        self.next_native_function = function_id
            .checked_add(1)
            .ok_or_else(|| LuaError::new("native callable ID space exhausted"))?;
        let callable = sol_core::NativeCallableId::new(2, function_id);
        self.c_functions.insert(callable, function);
        self.c_function_ids.insert(identity, callable);
        Ok(callable)
    }

    fn call_c_function(
        &mut self,
        callable: CanonicalCFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        match self.call_c_function_outcome(callable, args)? {
            c_api::CApiOutcome::Returned(values) => Ok(values),
            c_api::CApiOutcome::Yielded(_) => {
                Err(LuaError::new("attempt to yield across a C-call boundary"))
            }
        }
    }

    fn call_c_function_outcome(
        &mut self,
        callable: CanonicalCFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<c_api::CApiOutcome> {
        let callable_id = callable.callable_id();
        let function = self.c_functions.get(&callable_id).copied().ok_or_else(|| {
            LuaError::new(format!(
                "C callable provider {} function {} is not registered",
                callable_id.provider, callable_id.function
            ))
        })?;
        c_api::invoke(self, function, args, callable.object_id())
    }

    /// Executes a compiled `Proto` against a fresh register file. Most
    /// registers are plain, unboxed `LuaValue`s in a flat `Vec` - only
    /// registers the compiler determined are ever captured as a
    /// `ParentLocal` upvalue (`proto.captured_registers`) get an actual
    /// `RcRef<LuaValue>` cell, since only those can be aliased by a
    /// closure that outlives this call. `NewLocal`/loop-variable
    /// materialization give a captured register a *fresh* cell identity
    /// (see `reg_set_fresh`) so a closure created in one iteration/scope
    /// keeps its own cell even after a later iteration or an unrelated
    /// later use reuses the same register number; non-captured registers
    /// skip this entirely; there's nothing to alias. All operator, table,
    /// and metatable semantics are delegated to the existing `LuaRuntime`
    /// methods (`binary`/`unary`/`index`/`set_index`/`call`/etc.) — this
    /// method only interprets control flow and register traffic.
    /// Pops a previously-recycled register buffer (see `regs_pool`'s field
    /// doc) or allocates a fresh one if the pool is empty, then resizes it
    /// to exactly `num_registers` `Nil`s - reusing existing capacity from a
    /// prior, possibly differently-shaped call instead of allocating.
    fn take_regs_buffer(&mut self, num_registers: usize) -> Vec<LuaValue> {
        let mut regs = self.regs_pool.pop().unwrap_or_default();
        regs.clear();
        regs.resize(num_registers, LuaValue::Nil);
        regs
    }

    /// Same idea as `take_regs_buffer`, but each slot is independently
    /// `Some`/`None` per `captured_registers`, so it's rebuilt slot-by-slot
    /// rather than resized. This still needs a fresh `Rc::new(RefCell::new(..))`
    /// per actually-captured register (that allocation is inherent to
    /// closure semantics: every call must give a captured local its own
    /// cell identity) - only the enclosing `Vec`'s allocation is recycled.
    fn take_cells_buffer(&mut self, captured_registers: &[bool]) -> Cells {
        let mut cells = self.cells_pool.pop().unwrap_or_default();
        cells.clear();
        cells.extend(captured_registers.iter().map(|&captured| {
            if captured {
                Some(Rc::new(RefCell::new(LuaValue::Nil)))
            } else {
                None
            }
        }));
        cells
    }

    /// Returns both frame buffers to their pools for a later call to reuse.
    /// Cleared here (not left for the next `take_*_buffer` call): a value
    /// left sitting in an idle pooled buffer would keep its `Rc` alive for
    /// however long the buffer sits unused, invisible to the trial-deletion
    /// cycle collector (`collect_cycles`, which only walks `gc_tables`/
    /// `gc_closures`, not these pools) - dropping eagerly here (even for
    /// `cells` holding `Rc`s a live closure still shares) just decrements our
    /// reference; it doesn't invalidate the closure's own clone.
    fn recycle_frame_buffers(&mut self, mut regs: Vec<LuaValue>, mut cells: Cells) {
        regs.clear();
        cells.clear();
        self.regs_pool.push(regs);
        self.cells_pool.push(cells);
    }

    /// Pops a previously-recycled argument/return-value buffer (see
    /// `values_pool`'s field doc) or allocates a fresh one if the pool is
    /// empty. Cleared but not resized - callers `extend`/`push` into it.
    fn take_values_buffer(&mut self) -> Vec<LuaValue> {
        let mut values = self.values_pool.pop().unwrap_or_default();
        values.clear();
        values
    }

    /// Returns a call's argument or return-value buffer to the pool for a
    /// later call to reuse, once the caller is done reading it. Cleared
    /// immediately (not just on the next `take_values_buffer`): otherwise a
    /// value sitting untouched in an idle pooled buffer would hold its `Rc`
    /// alive for however long the buffer sits unused, which the trial-
    /// deletion cycle collector (`collect_cycles`) has no visibility into
    /// (it only walks `gc_tables`/`gc_closures`, not this pool) - so a stale
    /// reference here could delay or block reclaiming an otherwise-garbage
    /// cycle.
    fn recycle_values_buffer(&mut self, mut values: Vec<LuaValue>) {
        values.clear();
        self.values_pool.push(values);
    }

    /// Entry point for a `LuaValue::Closure` call: builds the initial
    /// `LuaFrame` and drives `LuaRuntime::frames` iteratively until it (and
    /// everything it transitively pushes via ordinary `Instr::Call` against
    /// further closures) completes. Replaces the old `run_proto`, which
    /// recursed through the native Rust call stack once per nested Lua call;
    /// here nested closure calls are pushed onto `frames` instead, so a deep
    /// Lua call chain costs O(1) native stack regardless of depth (still
    /// bounded by `max_call_depth`, just not by the OS stack size).
    ///
    /// This is the blocking bridge - the only caller that can reach it is
    /// `LuaRuntime::call`, used outside the trampoline (metamethod calls not
    /// yet trampolined, a leaf native calling back into `self.call()`, ...).
    /// Real Lua has no way to yield across such a boundary either ("attempt
    /// to yield across a C-call boundary"), so a `DriveOutcome::Yielded`
    /// reaching here - `coroutine.yield` called from inside this blocking
    /// call's dynamic extent - is a genuine error, not a suspension: unwind
    /// exactly like the `Err` path already does and report it. The
    /// coroutine's own driving (`LuaRuntime::resume_coroutine`) calls
    /// `drive` directly instead, so it can treat `Yielded` as legitimate.
    fn call_closure(
        &mut self,
        proto: Rc<Proto>,
        upvals: Vec<RcRef<LuaValue>>,
        globals: Globals,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let base_depth = self.frames.len();
        // Called directly against an already-resolved `LuaValue::Closure`
        // (see `call`'s `LuaValue::Closure` arm) - no `__call` chain was
        // walked to get here, so `call_chain_hops` is 0. A `__call` chain
        // resolved through this same blocking bridge's own inline retry
        // (`call`'s `other` arm) recurses through `call` again instead of
        // reaching here directly, so it is not reflected in
        // `debug.getinfo`'s `extraargs` either - a known gap in that
        // untrampolined path, distinct from the trampolined
        // `step_result_for_call` path this crate's `debug.getinfo` support
        // was verified against.
        let frame = self.new_lua_frame(proto, upvals, globals, args, 0)?;
        self.frames.push(Frame::Lua(frame));
        let mut depth_charged: usize = 0;
        match self.drive(base_depth, &mut depth_charged, None) {
            DriveOutcome::Returned(values) => Ok(values),
            DriveOutcome::Yielded(_) => {
                let error = LuaError::new("attempt to yield across a C-call boundary");
                let error = self.close_frames_above(base_depth, error);
                self.call_depth -= depth_charged;
                Err(error)
            }
            DriveOutcome::Raised(error) => {
                let error = self.close_frames_above(base_depth, error);
                self.call_depth -= depth_charged;
                Err(error)
            }
            DriveOutcome::TailCall(_) => {
                unreachable!("tail calls are consumed inside the dynamic trampoline")
            }
        }
    }

    /// Builds a fresh `LuaFrame` for `proto` - the exact preamble `run_proto`
    /// used to run inline (allocation charge, register/cell buffers, param
    /// binding, vararg collection) before entering its dispatch loop.
    fn new_lua_frame(
        &mut self,
        proto: Rc<Proto>,
        upvals: Vec<RcRef<LuaValue>>,
        globals: Globals,
        args: Vec<LuaValue>,
        call_chain_hops: usize,
    ) -> LuaResult<LuaFrame> {
        let function = self.prototype_id(&proto)?;
        let register_count = proto.metadata.registers as usize;
        let parameter_count = proto.metadata.arity.parameters as usize;
        self.charge_allocation(
            (register_count + proto.captured_cell_count) * std::mem::size_of::<LuaValue>(),
        )?;
        let mut regs = self.take_regs_buffer(register_count);
        let cells = self.take_cells_buffer(&proto.captured_registers);
        for i in 0..parameter_count.min(register_count) {
            let value = args.get(i).cloned().unwrap_or(LuaValue::Nil);
            reg_set(&mut regs, &cells, i, value);
        }
        let varargs: Vec<LuaValue> = if proto.metadata.arity.variadic {
            // Reuses `args`' own allocation as `varargs` (draining the
            // consumed leading params in place) instead of collecting into a
            // brand-new `Vec`.
            let mut varargs = args;
            let skip = parameter_count.min(varargs.len());
            varargs.drain(0..skip);
            varargs
        } else {
            self.recycle_values_buffer(args);
            Vec::new()
        };
        if let Some(vararg_reg) = proto.vararg_name {
            let table = self.values_table(&varargs);
            let LuaValue::Table(named_varargs) = &table else {
                unreachable!("values_table always returns a table")
            };
            named_varargs.borrow_mut().set(
                LuaValue::String(Rc::new(b"n".to_vec())),
                LuaValue::Integer(varargs.len() as i64),
            )?;
            reg_set(&mut regs, &cells, vararg_reg as usize, table);
        }
        Ok(LuaFrame {
            header: FrameHeader::new(function, 0, 0),
            proto,
            upvals,
            globals,
            regs,
            cells,
            varargs,
            call_chain_hops,
            pending: Pending::None,
            to_close: Vec::new(),
            entry_label: None,
            hook_last_pc: -1,
            hook_last_line: -1,
            c_hook_last_pc: -1,
            c_hook_last_line: -1,
        })
    }

    fn prototype_id(&mut self, prototype: &Rc<Proto>) -> LuaResult<FunctionId> {
        let identity = Rc::as_ptr(prototype) as usize;
        if let Some(function) = self.prototype_ids.get(&identity) {
            return Ok(*function);
        }
        let function = self
            .function_registry
            .register(prototype.metadata.clone(), sol_core::ExecutionTier::Generic)
            .map_err(|error| LuaError::new(error.to_string()))?;
        self.prototype_ids.insert(identity, function);
        Ok(function)
    }

    /// Drives `LuaRuntime::frames` from its current top down to (but not
    /// including) `base_depth`, returning either the result values of the
    /// frame that was sitting at `base_depth` when it finishes
    /// (`DriveOutcome::Done`), or the values passed to `coroutine.yield` if
    /// execution hit that anywhere above `base_depth`
    /// (`DriveOutcome::Yielded`) - in the latter case every frame above
    /// `base_depth` is left exactly as it was (still charged, not finished),
    /// ready for a later `drive` call to resume it. Any error leaves
    /// `frames` untouched here - `call_closure`/`resume_coroutine` truncate
    /// back to `base_depth` on the error path, since a partially-unwound
    /// frame stack must never be left for the next call to find.
    ///
    /// `initial_incoming`, when `Some`, seeds the loop's `incoming` instead
    /// of starting from `None` - needed only when resuming a coroutine whose
    /// topmost frame is already paused waiting on a `coroutine.yield` call's
    /// eventual result (see `resume_coroutine`); every other caller passes
    /// `None`, exactly matching the old no-parameter behavior.
    ///
    /// Every `Instr::Call` against a `Closure` callee that this drives (i.e.
    /// every `PushClosure` step beyond the first, base-depth frame `call_closure`
    /// already pushed and charged for via `call`'s own `call_depth`
    /// bookkeeping) charges one unit of `call_depth` budget here instead, so
    /// a chain of nested Lua calls is bounded the same way regardless of
    /// whether a given hop went through this trampoline or the blocking
    /// `call` bridge. `depth_charged` tracks how many such units this
    /// specific `drive` call has charged but not yet released, so an error
    /// exit can hand that count back to the caller to undo - `call_depth`
    /// must return to exactly what it was before this call started, the same
    /// way a native `call()` invocation always undoes its own `+= 1`.
    pub(super) fn drive(
        &mut self,
        base_depth: usize,
        depth_charged: &mut usize,
        initial_incoming: Option<Vec<LuaValue>>,
    ) -> DriveOutcome {
        match self.drive_result(base_depth, depth_charged, initial_incoming) {
            Ok(outcome) => outcome,
            Err(error) => DriveOutcome::Raised(error),
        }
    }

    /// Fallible implementation behind the semantic outcome boundary. Keeping
    /// `Result` internally lets instruction helpers use `?`; callers observe
    /// returned, yielded, and raised states through the common call ABI.
    fn drive_result(
        &mut self,
        base_depth: usize,
        depth_charged: &mut usize,
        initial_incoming: Option<Vec<LuaValue>>,
    ) -> LuaResult<DriveOutcome> {
        let mut incoming: Option<Vec<LuaValue>> = initial_incoming;
        loop {
            match self.frames.pop() {
                Some(Frame::Lua(mut frame)) => {
                    let step = self.dispatch_step(&mut frame, incoming.take());
                    // Only ever `Some` immediately after a dispatch step that
                    // just decided to call a `__index`/`__newindex` metamethod
                    // - see `pending_frame_label`'s field doc. Always taken
                    // here so it never leaks onto some later, unrelated call.
                    let new_frame_label = self.pending_frame_label.take();
                    match step {
                        Ok(StepResult::Done(values)) => {
                            // The base frame's own "return" is fired by `call`/
                            // `call_closure`'s caller instead (this
                            // `drive_result` call is itself running inside that
                            //  pending `self.call()`'s dynamic extent), so only
                            // fire here for a nested Lua frame - one this
                            // trampoline itself pushed via `PushClosure`/
                            // `resolve_call`, never through `self.call()`.
                            let is_base_frame = self.frames.len() == base_depth;
                            if !is_base_frame {
                                self.fire_hook("return", None)?;
                            }
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(values));
                            }
                            incoming = Some(values);
                        }
                        Ok(StepResult::PushClosure {
                            proto,
                            upvals,
                            globals,
                            args,
                            call_chain_hops,
                        }) => {
                            if self.call_depth >= self.max_call_depth {
                                let call_site = frame.proto.source_map.location(frame.header.pc);
                                let entry_label = frame.entry_label;
                                self.frames.push(Frame::Lua(frame));
                                let error = LuaError::new(
                                    "stack overflow (Lua call-depth budget exhausted)",
                                );
                                let error = match call_site {
                                    Some(location) => error.at(&format!("line {}", location.line)),
                                    None => error,
                                };
                                let error = match entry_label {
                                    Some(label) => error.at(&format!("in metamethod '{label}'")),
                                    None => error,
                                };
                                match self.unwind_error_to_marker(
                                    error,
                                    base_depth,
                                    depth_charged,
                                )? {
                                    CallStep::Pending => {}
                                    CallStep::Done(values) => incoming = Some(values),
                                    CallStep::Yielded(values) => {
                                        return Ok(DriveOutcome::Yielded(values));
                                    }
                                }
                            } else {
                                self.call_depth += 1;
                                *depth_charged += 1;
                                self.frames.push(Frame::Lua(frame));
                                let mut child = self.new_lua_frame(
                                    proto,
                                    upvals,
                                    globals,
                                    args,
                                    call_chain_hops,
                                )?;
                                child.entry_label = new_frame_label;
                                self.frames.push(Frame::Lua(child));
                                self.fire_hook("call", None)?;
                            }
                        }
                        Ok(StepResult::TailClosure {
                            proto,
                            upvals,
                            globals,
                            args,
                            call_chain_hops,
                        }) => {
                            frame.header.state = FrameState::Returned;
                            self.recycle_frame_buffers(
                                std::mem::take(&mut frame.regs),
                                std::mem::take(&mut frame.cells),
                            );
                            self.recycle_values_buffer(std::mem::take(&mut frame.varargs));
                            let replacement =
                                self.new_lua_frame(proto, upvals, globals, args, call_chain_hops)?;
                            self.frames.push(Frame::Lua(replacement));
                        }
                        Ok(StepResult::CallLeaf { callee, args }) => {
                            let call_site = frame.proto.source_map.location(frame.header.pc);
                            let call_proto = frame.proto.clone();
                            let call_pc = frame.header.pc as usize;
                            let call_base = match frame.pending {
                                Pending::Call { base, .. } => Some(base as Reg),
                                Pending::TailCall => match call_proto.instrs.get(call_pc) {
                                    Some(Instr::TailCall(base, _)) => Some(*base),
                                    _ => None,
                                },
                                // Native continuations can issue a leaf call
                                // while the Lua frame is not suspended at an
                                // `Instr::Call`; such a call has no bytecode
                                // register to name.
                                _ => None,
                            };
                            // A leaf (non-Lua) metamethod fails before a
                            // child frame exists, so consume the pending
                            // label directly rather than waiting for the
                            // PushClosure arm to attach it to a frame.
                            let entry_label = new_frame_label.or(frame.entry_label);
                            self.frames.push(Frame::Lua(frame));
                            match self.call(callee, args) {
                                Ok(values) => incoming = Some(values),
                                Err(mut error) => {
                                    if let Some(call_base) = call_base {
                                        annotate_call_error(
                                            &mut error,
                                            &call_proto,
                                            call_pc,
                                            call_base,
                                        );
                                    }
                                    // Protected calls receive only an error
                                    // value, not its traceback, so the
                                    // metamethod context must also be part of
                                    // the primary message (Lua's observable
                                    // `pcall` behavior), not merely a stack
                                    // annotation.
                                    if let Some(label) = new_frame_label {
                                        if error.message.starts_with("attempt to call a ") {
                                            error.message =
                                                format!("{} (metamethod '{label}')", error.message);
                                        }
                                    }
                                    match self.unwind_error_to_marker(
                                        {
                                            let error = match call_site {
                                                Some(location) => {
                                                    error.at(&format!("line {}", location.line))
                                                }
                                                None => error,
                                            };
                                            match entry_label {
                                                Some(label) => {
                                                    error.at(&format!("in metamethod '{label}'"))
                                                }
                                                None => error,
                                            }
                                        },
                                        base_depth,
                                        depth_charged,
                                    )? {
                                        CallStep::Pending => {}
                                        CallStep::Done(values) => incoming = Some(values),
                                        CallStep::Yielded(values) => {
                                            return Ok(DriveOutcome::Yielded(values));
                                        }
                                    }
                                }
                            }
                        }
                        Ok(StepResult::CallNative { cont, callee, args }) => {
                            self.frames.push(Frame::Lua(frame));
                            match self.push_native_call(
                                cont,
                                callee,
                                args,
                                base_depth,
                                depth_charged,
                            )? {
                                CallStep::Pending => {}
                                CallStep::Done(values) => incoming = Some(values),
                                CallStep::Yielded(values) => {
                                    return Ok(DriveOutcome::Yielded(values));
                                }
                            }
                        }
                        Ok(StepResult::Resolved(values)) => {
                            self.frames.push(Frame::Lua(frame));
                            incoming = Some(values);
                        }
                        Ok(StepResult::Yield(values)) => {
                            self.frames.push(Frame::Lua(frame));
                            return Ok(DriveOutcome::Yielded(values));
                        }
                        Err(error) => {
                            let error = match frame.proto.source_map.location(frame.header.pc) {
                                Some(location) => error.at(&format!("line {}", location.line)),
                                None => error,
                            };
                            let error = match frame.entry_label {
                                Some(label) => error.at(&format!("in metamethod '{label}'")),
                                None => error,
                            };
                            // `frame` was popped above and never pushed back
                            // for this arm, so it's being discarded here -
                            // close its own pending `<close>` values before
                            // searching outer frames for a pcall/xpcall
                            // marker.
                            let error = self.close_frame_tbc_on_error(&mut frame, error);
                            match self.unwind_error_to_marker(error, base_depth, depth_charged)? {
                                CallStep::Pending => {}
                                CallStep::Done(values) => incoming = Some(values),
                                CallStep::Yielded(values) => {
                                    return Ok(DriveOutcome::Yielded(values));
                                }
                            }
                        }
                    }
                }
                Some(Frame::Native(cont)) => {
                    let values = incoming.take().expect(
                        "drive: a Frame::Native marker is only ever resumed with a pending result",
                    );
                    match cont {
                        NativeCont::Pcall | NativeCont::Xpcall(XCallStage::Function { .. }) => {
                            self.fire_hook("return", None)?;
                            let mut wrapped = vec![LuaValue::Bool(true)];
                            wrapped.extend(values);
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(wrapped));
                            }
                            incoming = Some(wrapped);
                        }
                        NativeCont::Xpcall(XCallStage::Handler) => {
                            let wrapped = vec![
                                LuaValue::Bool(false),
                                values.into_iter().next().unwrap_or(LuaValue::Nil),
                            ];
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(wrapped));
                            }
                            incoming = Some(wrapped);
                        }
                        NativeCont::Once => {
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(values));
                            }
                            incoming = Some(values);
                        }
                        NativeCont::Sort(mut state) => {
                            let less = values.into_iter().next().unwrap_or(LuaValue::Nil).truthy();
                            match self.sort_step(&mut state, Some(less)) {
                                Ok(SortOutcome::Done(result_values)) => {
                                    self.fire_hook("return", None)?;
                                    if self.finish_frame(base_depth, depth_charged) {
                                        return Ok(DriveOutcome::Returned(result_values));
                                    }
                                    incoming = Some(result_values);
                                }
                                Ok(SortOutcome::NeedsCall { callee, args }) => {
                                    self.frames.push(Frame::Native(NativeCont::Sort(state)));
                                    match self.resolve_call(callee, args, base_depth, depth_charged)
                                    {
                                        Ok(CallStep::Done(values)) => incoming = Some(values),
                                        Ok(CallStep::Pending) => {}
                                        Ok(CallStep::Yielded(values)) => {
                                            return Ok(DriveOutcome::Yielded(values));
                                        }
                                        Err(error) => match self.unwind_error_to_marker(
                                            error,
                                            base_depth,
                                            depth_charged,
                                        )? {
                                            CallStep::Pending => {}
                                            CallStep::Done(values) => incoming = Some(values),
                                            CallStep::Yielded(values) => {
                                                return Ok(DriveOutcome::Yielded(values));
                                            }
                                        },
                                    }
                                }
                                Err(error) => match self.unwind_error_to_marker(
                                    error,
                                    base_depth,
                                    depth_charged,
                                )? {
                                    CallStep::Pending => {}
                                    CallStep::Done(values) => incoming = Some(values),
                                    CallStep::Yielded(values) => {
                                        return Ok(DriveOutcome::Yielded(values));
                                    }
                                },
                            }
                        }
                        NativeCont::Gsub(mut state) => {
                            let value = values.into_iter().next().unwrap_or(LuaValue::Nil);
                            match self.gsub_step(&mut state, Some(value)) {
                                Ok(GsubOutcome::Done(output, count)) => {
                                    self.fire_hook("return", None)?;
                                    let result_values =
                                        vec![output, LuaValue::Integer(count as i64)];
                                    if self.finish_frame(base_depth, depth_charged) {
                                        return Ok(DriveOutcome::Returned(result_values));
                                    }
                                    incoming = Some(result_values);
                                }
                                Ok(GsubOutcome::NeedsCall { callee, args }) => {
                                    self.frames.push(Frame::Native(NativeCont::Gsub(state)));
                                    match self.resolve_call(callee, args, base_depth, depth_charged)
                                    {
                                        Ok(CallStep::Done(values)) => incoming = Some(values),
                                        Ok(CallStep::Pending) => {}
                                        Ok(CallStep::Yielded(values)) => {
                                            return Ok(DriveOutcome::Yielded(values));
                                        }
                                        Err(error) => match self.unwind_error_to_marker(
                                            error,
                                            base_depth,
                                            depth_charged,
                                        )? {
                                            CallStep::Pending => {}
                                            CallStep::Done(values) => incoming = Some(values),
                                            CallStep::Yielded(values) => {
                                                return Ok(DriveOutcome::Yielded(values));
                                            }
                                        },
                                    }
                                }
                                Err(error) => match self.unwind_error_to_marker(
                                    error,
                                    base_depth,
                                    depth_charged,
                                )? {
                                    CallStep::Pending => {}
                                    CallStep::Done(values) => incoming = Some(values),
                                    CallStep::Yielded(values) => {
                                        return Ok(DriveOutcome::Yielded(values));
                                    }
                                },
                            }
                        }
                    }
                }
                None => unreachable!("drive: frame stack must hold a frame above base_depth"),
            }
        }
    }

    /// Shared by every place a frame (`Frame::Lua` via `StepResult::Done`, or
    /// `Frame::Native` once its wrapped result is ready) completes: charges
    /// back the `call_depth` unit this `drive` call charged for it (every
    /// frame above `base_depth` was charged exactly once, by whichever of
    /// `PushClosure`/`push_native_call` pushed it), then reports whether this
    /// was the base frame itself - the signal for `drive` to return its
    /// result rather than deliver it as `incoming` to whatever is now on top.
    fn finish_frame(&mut self, base_depth: usize, depth_charged: &mut usize) -> bool {
        if self.frames.len() > base_depth {
            self.call_depth -= 1;
            *depth_charged -= 1;
        }
        self.frames.len() == base_depth
    }

    /// Pushes `cont` as a `Frame::Native` marker, charging one `call_depth`
    /// unit for it, then resolves `callee` via `resolve_call`. Used for a
    /// *fresh* native-builtin call (the marker has not been charged for
    /// yet) - a `Sort`/`Gsub` continuation that re-issues another call after
    /// already having a marker on the stack instead pushes its own
    /// `Frame::Native` directly and calls `resolve_call`, since that
    /// continuation is not a new call and must not be charged again.
    fn push_native_call(
        &mut self,
        cont: NativeCont,
        callee: LuaValue,
        args: Vec<LuaValue>,
        base_depth: usize,
        depth_charged: &mut usize,
    ) -> LuaResult<CallStep> {
        if self.call_depth >= self.max_call_depth {
            return self.unwind_error_to_marker(
                LuaError::new("stack overflow (Lua call-depth budget exhausted)"),
                base_depth,
                depth_charged,
            );
        }
        self.call_depth += 1;
        *depth_charged += 1;
        // `Once` isn't a real Lua-visible C function boundary (see its
        // `NativeCont` doc) - no call/return hook pair for it, only for the
        // actual pcall/xpcall/sort/gsub C functions this marker represents.
        let fires_hook = !matches!(cont, NativeCont::Once);
        self.frames.push(Frame::Native(cont));
        if fires_hook {
            self.fire_hook("call", None)?;
        }
        self.resolve_call(callee, args, base_depth, depth_charged)
    }

    /// Resolves callee (exactly as `step_result_for_call` would for an ordinary
    /// call) and either pushes the resulting `Closure` frame above whatever
    /// `Frame::Native` marker the caller just pushed (returning
    /// `Ok(CallStep::Pending)` - nothing is ready yet), issues the resulting
    /// leaf call synchronously (returning `Ok(CallStep::Done(values))` once it
    /// completes), hands back a fully-synchronous `Resolved` result the same
    /// way, escapes as `Ok(CallStep::Yielded(values))` if `callee` was actually
    /// `coroutine.yield`, or recurses through `push_native_call` for a nested
    /// reentrant native call. A leaf call - or resolving `callee` itself - can
    /// error; that error gets the same `unwind_error_to_marker` treatment as an
    /// error from deeper inside a pushed `Closure` frame, so it can still be
    /// caught by the very marker the caller just pushed (or a still-earlier
    /// one), matching real Lua's protected-call semantics. Shared by
    /// `push_native_call` (which pushes a freshly-charged marker before calling
    /// this) and by `drive`'s `NativeCont::Sort`/`NativeCont::Gsub` resume arms
    /// (which push an *uncharged* continuation marker - the same logical call
    /// as before, not a new one - before calling this).
    pub(super) fn resolve_call(
        &mut self,
        callee: LuaValue,
        args: Vec<LuaValue>,
        base_depth: usize,
        depth_charged: &mut usize,
    ) -> LuaResult<CallStep> {
        match self.step_result_for_call(callee, args) {
            Ok(StepResult::PushClosure {
                proto,
                upvals,
                globals,
                args,
                call_chain_hops,
            }) => {
                if self.call_depth >= self.max_call_depth {
                    return self.unwind_error_to_marker(
                        LuaError::new("stack overflow (Lua call-depth budget exhausted)"),
                        base_depth,
                        depth_charged,
                    );
                }
                self.call_depth += 1;
                *depth_charged += 1;
                let child = self.new_lua_frame(proto, upvals, globals, args, call_chain_hops)?;
                self.frames.push(Frame::Lua(child));
                self.fire_hook("call", None)?;
                Ok(CallStep::Pending)
            }
            Ok(StepResult::TailClosure { .. }) => {
                unreachable!("only TailCall instruction dispatch creates TailClosure")
            }
            Ok(StepResult::CallLeaf { callee, args }) => match self.call(callee, args) {
                Ok(values) => Ok(CallStep::Done(values)),
                Err(error) => self.unwind_error_to_marker(error, base_depth, depth_charged),
            },
            Ok(StepResult::CallNative { cont, callee, args }) => {
                self.push_native_call(cont, callee, args, base_depth, depth_charged)
            }
            Ok(StepResult::Resolved(values)) => Ok(CallStep::Done(values)),
            Ok(StepResult::Yield(values)) => Ok(CallStep::Yielded(values)),
            Ok(StepResult::Done(_)) => unreachable!("step_result_for_call never returns Done"),
            Err(error) => self.unwind_error_to_marker(error, base_depth, depth_charged),
        }
    }

    /// Searches `LuaRuntime::frames[base_depth..]`, innermost first, for the
    /// nearest `NativeCont::Pcall`/`NativeCont::Xpcall(XCallStage::Function)`
    /// marker - the frame-stack equivalent of real Lua's search up the C
    /// stack for the nearest enclosing protected-call boundary. If found,
    /// discards that marker and everything above it (undoing their
    /// `call_depth` charges, exactly as if each had returned normally - this
    /// is the only place a frame is discarded without ever completing) and
    /// converts `error` into that marker's resume value: `pcall` gets
    /// `(false, err)` directly; `xpcall` instead issues a call to its
    /// `handler` with `err` (itself going through this same push/unwind
    /// machinery, so a `handler` that also raises is not caught by its own
    /// marker); either can itself yield (e.g. `xpcall(f, coroutine.yield)`),
    /// which surfaces here as `Ok(CallStep::Yielded(_))` for the caller to
    /// propagate as a `DriveOutcome::Yielded` exactly like any other yield.
    /// Returns `Err(error)` unchanged if no marker was found below
    /// `base_depth`, meaning the error is this `drive` call's own to
    /// propagate, exactly as before this method existed.
    /// Closes up to `count` pending `<close>` values on `frame` (LIFO,
    /// innermost/most-recently-pushed first), threading `error` through as
    /// the propagating error passed to each `__close(value, err)` call
    /// (`None` on normal scope exit; `Some` when unwinding after an error).
    /// If a `__close` call itself raises, that new error replaces the
    /// propagating one and closing continues with it, matching real Lua's
    /// own to-be-closed-variable unwind semantics. Returns the final
    /// propagating error, if any. `__close` is invoked through the blocking
    /// `call` bridge (like `__gc`/`__tostring`), not the trampoline, since a
    /// value being closed during an unwind can't itself suspend.
    pub(super) fn close_pending(
        &mut self,
        frame: &mut LuaFrame,
        count: u16,
        mut error: Option<LuaError>,
    ) -> Option<LuaError> {
        for _ in 0..count {
            let Some(value) = frame.to_close.pop() else {
                break;
            };
            if matches!(value, LuaValue::Nil | LuaValue::Bool(false)) {
                continue;
            }
            let method = match self.metamethod(&value, b"__close") {
                Ok(method) => method,
                Err(e) => {
                    error = Some(e);
                    continue;
                }
            };
            let Some(method) = method else { continue };
            let err_arg = error
                .clone()
                .map(LuaError::into_lua_value)
                .unwrap_or(LuaValue::Nil);
            if let Err(e) = self.call(method, vec![value, err_arg]) {
                error = Some(e);
            }
        }
        error
    }

    /// Closes every remaining pending `<close>` value on `frame` - used when
    /// discarding a frame entirely (an error unwinding past it, or tearing down
    /// a blocking-call/coroutine-resume boundary), as opposed to
    /// `Instr::CloseSlots`, which only closes one scope's worth during
    /// normal control flow.
    fn close_frame_tbc_on_error(&mut self, frame: &mut LuaFrame, error: LuaError) -> LuaError {
        let count = frame.to_close.len() as u16;
        self.close_pending(frame, count, Some(error))
            .expect("close_pending never clears an input Some back to None")
    }

    /// Discards every frame above `base_depth`, closing each discarded
    /// `Frame::Lua`'s pending `<close>` values first (innermost frame - the end
    /// of `self.frames` - first). The error-unwind counterpart of
    /// `unwind_error_to_marker`, used at the trampoline's own outer boundaries
    /// (`call_closure`'s blocking bridge, `resume_coroutine`'s resume) where no
    /// `pcall`/`xpcall` marker search applies and the whole span above
    /// `base_depth` is simply discarded.
    pub(super) fn close_frames_above(&mut self, base_depth: usize, error: LuaError) -> LuaError {
        self.close_frames_above_optional(base_depth, Some(error))
            .expect("close_frames_above_optional never clears an input Some back to None")
    }

    /// The `Option`-carrying generalization `close_frames_above` is built on:
    /// `None` in means every discarded frame's `<close>` handler runs with no
    /// propagating error (`err` argument `nil`), matching a clean, non-error
    /// close - used by `coroutine.close()`'s self-close forced abort, which
    /// is not itself an error (see the `uncatchable` field doc on
    /// `LuaError`), only becoming one if some handler raises along the way.
    pub(super) fn close_frames_above_optional(
        &mut self,
        base_depth: usize,
        error: Option<LuaError>,
    ) -> Option<LuaError> {
        let mut error = error;
        while self.frames.len() > base_depth {
            if let Frame::Lua(mut frame) = self.frames.pop().expect("len > base_depth") {
                let count = frame.to_close.len() as u16;
                error = self.close_pending(&mut frame, count, error);
            }
        }
        error
    }

    /// Whether the currently executing coroutine/main is inside a native
    /// library call that real Lua does not let a yield cross - `string.gsub`
    /// and `table.sort`'s comparator specifically (verified against the
    /// pinned `lua5.5.1` oracle: both report `coroutine.isyieldable() ==
    /// false` inside their replacement/comparator callback, and a
    /// `coroutine.yield()` attempted there raises "attempt to yield across a
    /// C-call boundary", unlike `pcall`/`xpcall`, which are genuinely
    /// yieldable). `self.frames` holds only the currently-executing
    /// coroutine's own stack (swapped in by `resume_coroutine`), so a linear
    /// scan for either marker anywhere on it is equivalent to real Lua's
    /// running `nny` (non-yieldable-call-nesting) counter.
    pub(super) fn in_non_yieldable_call(&self) -> bool {
        self.frames.iter().any(|frame| {
            matches!(
                frame,
                Frame::Native(NativeCont::Gsub(_)) | Frame::Native(NativeCont::Sort(_))
            )
        })
    }

    fn unwind_error_to_marker(
        &mut self,
        error: LuaError,
        base_depth: usize,
        depth_charged: &mut usize,
    ) -> LuaResult<CallStep> {
        // An uncatchable error (currently only `coroutine.close()`'s self-close
        // forced abort) never stops at a `pcall`/`xpcall` marker - it always
        // propagates straight to the caller, exactly like an ordinary error
        // with no enclosing protected call at all, it reaches the coroutine's
        // own `resume_coroutine` boundary instead of being caught partway up.
        let marker_index = if error.uncatchable {
            None
        } else {
            self.frames[base_depth..].iter().rposition(|frame| {
                matches!(
                    frame,
                    Frame::Native(NativeCont::Pcall)
                        | Frame::Native(NativeCont::Xpcall(XCallStage::Function { .. }))
                        | Frame::Native(NativeCont::Xpcall(XCallStage::Handler))
                )
            })
        };
        let Some(marker_index) = marker_index else {
            return Err(error);
        };
        let marker_index = base_depth + marker_index;
        let removed = self.frames.len() - marker_index;
        // Close every discarded Lua frame's pending `<close>` values first,
        // innermost (highest index, popped first) to outermost, threading the
        // propagating error through each one.
        let mut error = error;
        while self.frames.len() > marker_index + 1 {
            if let Frame::Lua(mut frame) = self.frames.pop().expect("len > marker_index + 1") {
                error = self.close_frame_tbc_on_error(&mut frame, error);
            }
        }
        let cont = match self
            .frames
            .pop()
            .expect("unwind_error_to_marker: marker_index is in bounds")
        {
            Frame::Native(cont) => cont,
            Frame::Lua(_) => {
                unreachable!("unwind_error_to_marker: marker_index must point at a Frame::Native")
            }
        };
        self.call_depth -= removed;
        *depth_charged -= removed;
        match cont {
            NativeCont::Pcall => Ok(CallStep::Done(vec![
                LuaValue::Bool(false),
                error.into_lua_value(),
            ])),
            NativeCont::Xpcall(XCallStage::Function { handler }) => {
                // Stashed for `debug.traceback` to pick up if `handler` is (or
                // calls) it: real Lua's message handler runs with the erroring
                // stack still in place, but this trampoline has already unwound
                // and discarded those frames by this point (see the loop above),
                // so the frame-name/line trail collected on `error.stack` while
                // unwinding is the only surviving record of it.
                self.pending_error_stack = Some(error.stack.clone());
                self.push_native_call(
                    NativeCont::Xpcall(XCallStage::Handler),
                    handler,
                    vec![error.into_lua_value()],
                    base_depth,
                    depth_charged,
                )
            }
            NativeCont::Xpcall(XCallStage::Handler) => {
                // The message handler itself raised while running (real Lua's
                // "error in error handling" double fault, e.g. a  handler that
                // recurses past the call-depth budget the same way the original
                // error did) - real Lua does not attempt to invoke the handler
                // again for its own error, it synthesizes this fixed message.
                Ok(CallStep::Done(vec![
                    LuaValue::Bool(false),
                    LuaValue::String(Rc::new(b"error in error handling".to_vec())),
                ]))
            }
            NativeCont::Once => {
                unreachable!("unwind_error_to_marker: marker search never matches SingleCall")
            }
            NativeCont::Sort(_) => {
                unreachable!("unwind_error_to_marker: marker search never matches Sort")
            }
            NativeCont::Gsub(_) => {
                unreachable!("unwind_error_to_marker: marker search never matches Gsub")
            }
        }
    }

    /// Non-blocking half of unary-operator dispatch: resolves everything that
    /// doesn't need a metamethod call synchronously, and otherwise returns the
    /// method/args to call rather than calling it - see `Pending`'s doc comment
    fn unary_resolve(&mut self, op: UnaryOp, value: LuaValue) -> LuaResult<UnaryResolution> {
        match op {
            UnaryOp::Not => Ok(UnaryResolution::Value(LuaValue::Bool(!value.truthy()))),
            UnaryOp::Neg => match coerce_number(&value) {
                Ok(Number::Integer(n)) => {
                    Ok(UnaryResolution::Value(LuaValue::Integer(n.wrapping_neg())))
                }
                Ok(Number::Float(n)) => Ok(UnaryResolution::Value(LuaValue::Float(-n))),
                Err(error) => match self.metamethod(&value, b"__unm")? {
                    Some(method) => Ok(UnaryResolution::Call {
                        method,
                        // Real Lua's `luaT_trybiniTM` calls unary metamethods
                        // with the operand duplicated as both arguments (see
                        // `lvm.c`'s `luaT_callTMres(L, tm, p1, p1, res)`), not
                        // just once.
                        args: vec![value.clone(), value],
                    }),
                    None => Err(error),
                },
            },
            UnaryOp::BitNot => match self.integer(&value) {
                Ok(n) => Ok(UnaryResolution::Value(LuaValue::Integer(!n))),
                Err(error) => match self.metamethod(&value, b"__bnot")? {
                    Some(method) => Ok(UnaryResolution::Call {
                        method,
                        args: vec![value.clone(), value],
                    }),
                    // Real Lua's `luaV_execute`'s `OP_BNOT` falls back to
                    // `luaG_opinterror(L, p1, p1, "perform bitwise operation
                    // on")` when there's no `__bnot` metamethod, which
                    // reports the operand's type (including a `FILE*`
                    // userdata's `__name`) rather than the generic "number
                    // expected" `coerce_integer` raises for any non-number -
                    // matching the binary bitwise operators' `BitAnd | BitOr
                    // | ...` arm above. A float with no integer
                    // representation (e.g. `~-3.009`) still keeps its own,
                    // more specific message.
                    None => Err(if error.message.contains("no integer representation") {
                        error
                    } else {
                        LuaError::new(format!(
                            "attempt to perform bitwise operation on a {} value",
                            self.error_type_label(&value)
                        ))
                    }),
                },
            },
        }
    }

    /// See `unary`/`unary_resolve` - the binary-operator counterpart.
    pub(super) fn binary(
        &mut self,
        op: BinaryOp,
        left: LuaValue,
        right: LuaValue,
    ) -> LuaResult<LuaValue> {
        match self.binary_resolve(op, left, right)? {
            BinaryResolution::Value(value) => Ok(value),
            BinaryResolution::Call {
                method,
                args,
                continuation,
            } => {
                let raw = self
                    .call(method, args)?
                    .into_iter()
                    .next()
                    .unwrap_or(LuaValue::Nil);
                Ok(match continuation {
                    BinaryContinuation::Raw => raw,
                    BinaryContinuation::Bool => LuaValue::Bool(raw.truthy()),
                    BinaryContinuation::BoolNegated => LuaValue::Bool(!raw.truthy()),
                })
            }
        }
    }

    fn find_binary_metamethod(
        &self,
        left: &LuaValue,
        right: &LuaValue,
        name: &[u8],
    ) -> LuaResult<Option<LuaValue>> {
        Ok(self
            .metamethod(left, name)?
            .or(self.metamethod(right, name)?))
    }

    pub(super) fn binary_resolve(
        &mut self,
        op: BinaryOp,
        left: LuaValue,
        right: LuaValue,
    ) -> LuaResult<BinaryResolution> {
        use BinaryOp::*;
        match op {
            And | Or => Ok(BinaryResolution::Value(right)),
            Eq | NotEq => {
                if left == right {
                    return Ok(BinaryResolution::Value(LuaValue::Bool(op == Eq)));
                }
                match self.find_binary_metamethod(&left, &right, b"__eq")? {
                    Some(method) => Ok(BinaryResolution::Call {
                        method,
                        args: vec![left, right],
                        continuation: if op == Eq {
                            BinaryContinuation::Bool
                        } else {
                            BinaryContinuation::BoolNegated
                        },
                    }),
                    None => Ok(BinaryResolution::Value(LuaValue::Bool(op != Eq))),
                }
            }
            Lt | Le | Gt | Ge => {
                if let (Ok(left_number), Ok(right_number)) = (left.number(), right.number()) {
                    let result = compare_numbers(left_number, right_number)
                        .map(|ordering| match op {
                            Lt => ordering.is_lt(),
                            Le => ordering.is_le(),
                            Gt => ordering.is_gt(),
                            Ge => ordering.is_ge(),
                            _ => unreachable!(),
                        })
                        .unwrap_or(false);
                    return Ok(BinaryResolution::Value(LuaValue::Bool(result)));
                }
                let primitive = match (&left, &right) {
                    (LuaValue::String(a), LuaValue::String(b)) => Some(match op {
                        Lt => a < b,
                        Le => a <= b,
                        Gt => a > b,
                        Ge => a >= b,
                        _ => unreachable!(),
                    }),
                    _ => None,
                };
                if let Some(result) = primitive {
                    return Ok(BinaryResolution::Value(LuaValue::Bool(result)));
                }
                let left_type = self.error_type_label(&left);
                let right_type = self.error_type_label(&right);
                let (method_name, first, second) = match op {
                    Lt => (b"__lt".as_slice(), left, right),
                    Le => (b"__le".as_slice(), left, right),
                    Gt => (b"__lt".as_slice(), right, left),
                    Ge => (b"__le".as_slice(), right, left),
                    _ => unreachable!(),
                };
                if let Some(method) = self.find_binary_metamethod(&first, &second, method_name)? {
                    return Ok(BinaryResolution::Call {
                        method,
                        args: vec![first, second],
                        continuation: BinaryContinuation::Bool,
                    });
                }
                if matches!(op, Le | Ge) {
                    if let Some(method) = self.find_binary_metamethod(&second, &first, b"__lt")? {
                        return Ok(BinaryResolution::Call {
                            method,
                            args: vec![second, first],
                            continuation: BinaryContinuation::BoolNegated,
                        });
                    }
                }
                let message = if left_type == right_type {
                    format!("attempt to compare two {left_type} values")
                } else {
                    format!("attempt to compare {left_type} with {right_type}")
                };
                Err(LuaError::new(message))
            }
            Concat => {
                let primitive = |value: &LuaValue| match value {
                    LuaValue::String(_) | LuaValue::Integer(_) | LuaValue::Float(_) => {
                        Some(value.display_bytes())
                    }
                    _ => None,
                };
                if let (Some(mut a), Some(b)) = (primitive(&left), primitive(&right)) {
                    a.extend(b);
                    return Ok(BinaryResolution::Value(LuaValue::String(Rc::new(a))));
                }
                match self.find_binary_metamethod(&left, &right, b"__concat")? {
                    Some(method) => Ok(BinaryResolution::Call {
                        method,
                        args: vec![left, right],
                        continuation: BinaryContinuation::Raw,
                    }),
                    None => {
                        // Lua points at the operand which prevented primitive
                        // string/number concatenation. Prefer the right side
                        // (`left .. bad`), then fall back to the left.
                        let invalid = if primitive(&right).is_none() {
                            &right
                        } else {
                            &left
                        };
                        Err(LuaError::new(format!(
                            "attempt to concatenate a {} value",
                            invalid.type_name()
                        )))
                    }
                }
            }
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let (primitive_error, failing_side) = match self.integer(&left) {
                    Ok(a) => match self.integer(&right) {
                        Ok(b) => {
                            return Ok(BinaryResolution::Value(match op {
                                BitAnd => LuaValue::Integer(a & b),
                                BitOr => LuaValue::Integer(a | b),
                                BitXor => LuaValue::Integer(a ^ b),
                                Shl => LuaValue::Integer(shift(a, b, true)),
                                Shr => LuaValue::Integer(shift(a, b, false)),
                                _ => unreachable!(),
                            }));
                        }
                        Err(error) => (error, OperandSide::Right),
                    },
                    Err(error) => (error, OperandSide::Left),
                };
                let name = match op {
                    BitAnd => b"__band".as_slice(),
                    BitOr => b"__bor".as_slice(),
                    BitXor => b"__bxor".as_slice(),
                    Shl => b"__shl".as_slice(),
                    Shr => b"__shr".as_slice(),
                    _ => unreachable!(),
                };
                match self.find_binary_metamethod(&left, &right, name)? {
                    Some(method) => Ok(BinaryResolution::Call {
                        method,
                        args: vec![left, right],
                        continuation: BinaryContinuation::Raw,
                    }),
                    None => Err(
                        if primitive_error
                            .message
                            .contains("no integer representation")
                        {
                            primitive_error.with_operand_hint(failing_side)
                        } else {
                            let value = match failing_side {
                                OperandSide::Left => &left,
                                OperandSide::Right => &right,
                            };
                            LuaError::new(format!(
                                "attempt to perform bitwise operation on a {} value",
                                self.error_type_label(value)
                            ))
                        },
                    ),
                }
            }
            Add | Sub | Mul | Div | Mod | FloorDiv | Pow => {
                let failing_side = match coerce_number(&left) {
                    Ok(left) => match coerce_number(&right) {
                        Ok(right) => {
                            return Ok(BinaryResolution::Value(self.arithmetic(op, left, right)?));
                        }
                        Err(_) => OperandSide::Right,
                    },
                    Err(_) => OperandSide::Left,
                };
                let name = match op {
                    Add => b"__add".as_slice(),
                    Sub => b"__sub".as_slice(),
                    Mul => b"__mul".as_slice(),
                    Div => b"__div".as_slice(),
                    Mod => b"__mod".as_slice(),
                    FloorDiv => b"__idiv".as_slice(),
                    Pow => b"__pow".as_slice(),
                    _ => unreachable!(),
                };
                match self.find_binary_metamethod(&left, &right, name)? {
                    Some(method) => Ok(BinaryResolution::Call {
                        method,
                        args: vec![left, right],
                        continuation: BinaryContinuation::Raw,
                    }),
                    None => {
                        let failing_value = match failing_side {
                            OperandSide::Left => &left,
                            OperandSide::Right => &right,
                        };
                        let label = self.error_type_label(failing_value);
                        let message = if label != failing_value.type_name() {
                            format!("attempt to perform arithmetic on a {label} value")
                        } else {
                            "attempt to perform arithmetic on incompatible Lua values".to_string()
                        };
                        Err(LuaError::new(message).with_operand_hint(failing_side))
                    }
                }
            }
        }
    }

    fn arithmetic(&self, op: BinaryOp, left: Number, right: Number) -> LuaResult<LuaValue> {
        use BinaryOp::*;
        match (left, right) {
            (Number::Integer(a), Number::Integer(b)) if !matches!(op, Div | Pow) => match op {
                Add => Ok(LuaValue::Integer(a.wrapping_add(b))),
                Sub => Ok(LuaValue::Integer(a.wrapping_sub(b))),
                Mul => Ok(LuaValue::Integer(a.wrapping_mul(b))),
                Mod => floor_mod(a, b).map(LuaValue::Integer),
                FloorDiv => floor_div(a, b).map(LuaValue::Integer),
                _ => unreachable!(),
            },
            (left, right) => {
                let (a, b) = number_float(left, right);
                Ok(LuaValue::Float(match op {
                    Add => a + b,
                    Sub => a - b,
                    Mul => a * b,
                    Div => a / b,
                    // Real Lua's `luai_nummod` computes this via the
                    // hardware `fmod` plus a sign correction, not via
                    // `a - floor(a / b) * b` - that division/floor/multiply
                    // chain loses all precision for large `a` (e.g.
                    // `2.0^54 % 3`, math.lua's "precision of modulo for
                    // large numbers" check), since `a / b` and its floor
                    // are themselves already-rounded floats before ever
                    // being multiplied back out and subtracted.
                    Mod => {
                        let remainder = a % b;
                        if (remainder > 0.0 && b < 0.0) || (remainder < 0.0 && b > 0.0) {
                            remainder + b
                        } else {
                            remainder
                        }
                    }
                    FloorDiv => (a / b).floor(),
                    Pow => a.powf(b),
                    _ => unreachable!(),
                }))
            }
        }
    }

    pub(super) fn expect_table(&self, value: &LuaValue) -> LuaResult<RcRef<LuaTable>> {
        match value {
            LuaValue::Table(table) => Ok(table.clone()),
            value => Err(LuaError::new(format!(
                "table expected, got {}",
                value.type_name()
            ))),
        }
    }

    pub(super) fn raw_index(&self, value: LuaValue, key: LuaValue) -> LuaResult<LuaValue> {
        self.expect_table(&value)?.borrow().get(&key)
    }

    /// Blocking, metamethod-respecting `t[key]`/`t[key] = v`/`#t`, for native
    /// library functions (e.g `table.insert`/`remove`/`sort`/`concat`/`unpack`)
    /// that real Lua defines in terms of `lua_geti`/`lua_seti`/`lua_len` rather
    /// than raw field access - so a table argument with `__index`/`__newindex`/
    /// `__len` (a "proxy") behaves the same as a plain table. `index_resolve`/
    /// `set_index_resolve`/`len_resolve` are the coroutine-safe non-blocking
    /// halves used by the bytecode dispatch loop; native functions run outside
    /// that loop and can call back into `self.call` directly here.
    pub(super) fn index_get(&mut self, value: LuaValue, key: LuaValue) -> LuaResult<LuaValue> {
        match self.index_resolve(value, key)? {
            IndexResolution::Value(value) => Ok(value),
            IndexResolution::Call { method, args } => Ok(self
                .call(method, args)?
                .into_iter()
                .next()
                .unwrap_or(LuaValue::Nil)),
        }
    }

    pub(super) fn index_set(
        &mut self,
        value: LuaValue,
        key: LuaValue,
        new_value: LuaValue,
    ) -> LuaResult<()> {
        match self.set_index_resolve(value, key, new_value)? {
            SetIndexResolution::Done => Ok(()),
            SetIndexResolution::Call { method, args } => {
                self.call(method, args)?;
                Ok(())
            }
        }
    }

    pub(super) fn length_of(&mut self, value: LuaValue) -> LuaResult<i64> {
        let result = match self.len_resolve(value)? {
            LenResolution::Value(value) => value,
            LenResolution::Call { method, args } => self
                .call(method, args)?
                .into_iter()
                .next()
                .unwrap_or(LuaValue::Nil),
        };
        // Real Lua's `luaL_len` reports this specific message (not a generic
        // coercion error) when a `__len` metamethod's result isn't an
        // integer, e.g. `setmetatable({}, {__len = function() return 'abc' end})`.
        match result {
            LuaValue::Integer(n) => Ok(n),
            LuaValue::Float(f)
                if f.is_finite()
                    && f.fract() == 0.0
                    && f >= i64::MIN as f64
                    && f < -(i64::MIN as f64) =>
            {
                Ok(f as i64)
            }
            _ => Err(LuaError::new("object length is not an integer")),
        }
    }

    pub(super) fn raw_set_index(
        &mut self,
        value: LuaValue,
        key: LuaValue,
        new_value: LuaValue,
    ) -> LuaResult<()> {
        let table = self.expect_table(&value)?;
        if table.borrow().get(&key)? == LuaValue::Nil {
            self.charge_new_table_entry(&table)?;
        }
        let result = table.borrow_mut().set(key, new_value);
        result
    }

    pub(super) fn metamethod(&self, value: &LuaValue, name: &[u8]) -> LuaResult<Option<LuaValue>> {
        if let LuaValue::Userdata(value) = value {
            return c_api::canonical_metamethod(self, value.object_id(), name);
        }
        if let LuaValue::CanonicalTable(value) = value {
            return c_api::canonical_metamethod(self, value.object_id(), name);
        }
        let metatable = match value {
            LuaValue::Table(table) => table.borrow().metatable.clone(),
            LuaValue::String(_) => Some(self.string_metatable.clone()),
            LuaValue::Integer(_) | LuaValue::Float(_) => self.number_metatable.clone(),
            LuaValue::Bool(_) => self.boolean_metatable.clone(),
            LuaValue::Nil => self.nil_metatable.clone(),
            _ => None,
        };
        let Some(metatable) = metatable else {
            return Ok(None);
        };
        let value = metatable
            .borrow()
            .get(&LuaValue::String(Rc::new(name.to_vec())))?;
        Ok((value != LuaValue::Nil).then_some(value))
    }

    /// Non-blocking half of `#value` (`Instr::Len`) - see `IndexResolution`.
    fn len_resolve(&mut self, value: LuaValue) -> LuaResult<LenResolution> {
        if let Some(method) = self.metamethod(&value, b"__len")? {
            // Real Lua's `luaV_objlen` calls `__len` with the operand
            // duplicated as both arguments (`luaT_callTMres(L, tm, rb, rb,
            // ra)` in `lvm.c`), like the other unary metamethods.
            return Ok(LenResolution::Call {
                method,
                args: vec![value.clone(), value],
            });
        }
        match value {
            LuaValue::String(bytes) => {
                Ok(LenResolution::Value(LuaValue::Integer(bytes.len() as i64)))
            }
            LuaValue::Table(table) => Ok(LenResolution::Value(LuaValue::Integer(
                table.borrow().len() as i64,
            ))),
            other => Err(LuaError::new(format!(
                "attempt to get length of a {} value",
                other.type_name()
            ))),
        }
    }

    /// Non-blocking half of `__index` dispatch: mirrors real Lua's
    /// `MAXTAGLOOP`-guarded `__index` chain (`lvm.c`, "'__index' chain too
    /// long; possible loop") by walking the raw/metatable chain synchronously
    /// (table-to-table fallbacks are plain lookups, never a Lua call) up to
    /// `MAX_METATABLE_CHAIN` times, written as a loop rather than recursion
    /// so the bound costs O(1) native stack regardless of chain length. Only
    /// returns `Call` at the one point that needs to invoke a
    /// function-valued `__index` - see `IndexResolution`.
    pub(super) fn index_resolve(
        &mut self,
        mut value: LuaValue,
        key: LuaValue,
    ) -> LuaResult<IndexResolution> {
        for _ in 0..MAX_METATABLE_CHAIN {
            // Strings have no fields of their own to raw-get - real Lua
            // gives every string a shared metatable of `{ __index = string
            // }` (see `string_metatable`/`install_base`), so `s:upper()`/
            // `s.upper` always fall straight through to the `__index`
            // lookup below, which resolves through the `string` library
            // table (or whatever the shared metatable's `__index` has been
            // mutated to).
            // Only tables have raw fields to check first; every other type
            // (including a string, whose `__index` always resolves through
            // `string_metatable`, and a number/etc. that only indexes at all
            // because `debug.setmetatable` gave it an `__index`) has no raw
            // storage of its own and falls straight through to the
            // metamethod lookup below.
            let raw = match &value {
                LuaValue::Table(table) => table.borrow().get(&key)?,
                LuaValue::CanonicalTable(table) => {
                    c_api::canonical_table_get(self, table.object_id(), &key)?
                }
                _ => LuaValue::Nil,
            };
            if raw != LuaValue::Nil {
                return Ok(IndexResolution::Value(raw));
            }
            match self.metamethod(&value, b"__index")? {
                Some(LuaValue::Table(fallback)) => value = LuaValue::Table(fallback),
                Some(LuaValue::CanonicalTable(fallback)) => {
                    value = LuaValue::CanonicalTable(fallback)
                }
                Some(method) if method.type_name() == "function" => {
                    return Ok(IndexResolution::Call {
                        method,
                        args: vec![value, key],
                    })
                }
                // Lua follows a non-function `__index` value as the next
                // object in the chain. This is observable when the value is
                // a number: the next iteration, not an attempted call,
                // raises "attempt to index a number value".
                Some(fallback) => value = fallback,
                None if matches!(
                    value,
                    LuaValue::Table(_) | LuaValue::CanonicalTable(_) | LuaValue::String(_)
                ) =>
                {
                    return Ok(IndexResolution::Value(LuaValue::Nil))
                }
                None => {
                    return Err(LuaError::new(format!(
                        "attempt to index a {} value",
                        value.type_name()
                    )))
                }
            }
        }
        Err(LuaError::new("'__index' chain too long; possible loop"))
    }

    /// Non-blocking half of `__newindex` dispatch - see `index_resolve`; the
    /// same unbounded-cycle risk applies to `__newindex` table chains, and
    /// the same iterative-loop fix avoids native stack growth per fallback.
    fn set_index_resolve(
        &mut self,
        mut value: LuaValue,
        key: LuaValue,
        new_value: LuaValue,
    ) -> LuaResult<SetIndexResolution> {
        for _ in 0..MAX_METATABLE_CHAIN {
            let raw = match &value {
                LuaValue::Table(table) => table.borrow().get(&key)?,
                LuaValue::CanonicalTable(table) => {
                    c_api::canonical_table_get(self, table.object_id(), &key)?
                }
                _ => {
                    return Err(LuaError::new(format!(
                        "attempt to index a {} value",
                        value.type_name()
                    )))
                }
            };
            if raw != LuaValue::Nil {
                match &value {
                    LuaValue::Table(table) => table.borrow_mut().set(key, new_value)?,
                    LuaValue::CanonicalTable(table) => {
                        let key = c_api::lua_to_canonical(self, &key)?;
                        let new_value = c_api::lua_to_canonical(self, &new_value)?;
                        self.canonical_heap
                            .borrow_mut()
                            .table_set(table.object_id(), key, new_value)
                            .map_err(|error| LuaError::new(error.to_string()))?;
                    }
                    _ => unreachable!(),
                }
                return Ok(SetIndexResolution::Done);
            }
            match self.metamethod(&value, b"__newindex")? {
                Some(LuaValue::Table(fallback)) => value = LuaValue::Table(fallback),
                Some(LuaValue::CanonicalTable(fallback)) => {
                    value = LuaValue::CanonicalTable(fallback)
                }
                Some(method) => {
                    return Ok(SetIndexResolution::Call {
                        method,
                        args: vec![value, key, new_value],
                    })
                }
                None => {
                    match &value {
                        LuaValue::Table(table) => {
                            // `raw` was `Nil` above with no `__newindex` to
                            // fall back to, so this key is genuinely new to
                            // `table` - not an overwrite of an existing
                            // (possibly nil-tombstoned) entry.
                            self.charge_new_table_entry(table)?;
                            table.borrow_mut().set(key, new_value)?
                        }
                        LuaValue::CanonicalTable(table) => {
                            let key = c_api::lua_to_canonical(self, &key)?;
                            let new_value = c_api::lua_to_canonical(self, &new_value)?;
                            self.canonical_heap
                                .borrow_mut()
                                .table_set(table.object_id(), key, new_value)
                                .map_err(|error| LuaError::new(error.to_string()))?;
                        }
                        _ => unreachable!(),
                    }
                    return Ok(SetIndexResolution::Done);
                }
            }
        }
        Err(LuaError::new("'__newindex' chain too long; possible loop"))
    }

    pub(super) fn next(&self, value: LuaValue, key: LuaValue) -> LuaResult<Vec<LuaValue>> {
        let table = self.expect_table(&value)?;
        // Search the tombstone-inclusive view so a key that was live when
        // last returned by `next` - and has since been set to nil, which
        // real Lua explicitly permits mid-traversal - can still be located
        // to resume from, then skip forward past any nil-valued (deleted)
        // slots to find the next live entry.
        let entries = table.borrow().entries_with_tombstones();
        let start_index = if key == LuaValue::Nil {
            0
        } else {
            entries
                .iter()
                .position(|(entry_key, _)| entry_key == &key)
                .ok_or_else(|| LuaError::new("invalid key to 'next'"))?
                + 1
        };
        let result = entries[start_index..]
            .iter()
            .find(|(_, value)| *value != LuaValue::Nil)
            .map(|(key, value)| vec![key.clone(), value.clone()])
            .unwrap_or_else(|| vec![LuaValue::Nil]);
        Ok(result)
    }

    /// Shared by every `dispatch_step` call site that just resolved a
    /// callee (ordinary `Instr::Call`, or a metamethod/iterator-function
    /// dispatch): a `Closure` callee - directly, or one hop through a
    /// function-valued `__call` metamethod - gets pushed onto the
    /// frame-stack trampoline with no new native Rust call frame; `pcall`/
    /// `xpcall` get a `Frame::Native` marker (see `NativeCont`) below their
    /// own call, so they too need no new native Rust call frame regardless
    /// of how deep the protected call recurses. Anything else (native
    /// function, bridge, coroutine wrapper, a `__call` chain more than one
    /// hop deep, ...) still goes through the blocking `LuaRuntime::call`
    /// bridge via `StepResult::CallLeaf`, which already handles those cases
    /// (including erroring on a genuinely uncallable value) correctly.
    fn step_result_for_call(
        &mut self,
        callee: LuaValue,
        args: Vec<LuaValue>,
    ) -> LuaResult<StepResult> {
        match callee {
            LuaValue::NativeFunction(NativeFunction::PCall) => {
                let mut args = args;
                if args.is_empty() {
                    return Err(LuaError::new("bad argument #1 to 'pcall' (value expected)"));
                }
                let function = args.remove(0);
                return Ok(StepResult::CallNative {
                    cont: NativeCont::Pcall,
                    callee: function,
                    args,
                });
            }
            LuaValue::NativeFunction(NativeFunction::XCall) => {
                let mut args = args;
                if args.len() < 2 {
                    return Err(LuaError::new(format!(
                        "bad argument #{} to 'xpcall' (value expected)",
                        args.len() + 1
                    )));
                }
                // `xpcall(f, msgh, ...)` calls `f` with every argument after the
                // message handler (matches the blocking `call_native` arm below).
                let handler = args[1].clone();
                let function = args[0].clone();
                let extra_args = args.split_off(2);
                return Ok(StepResult::CallNative {
                    cont: NativeCont::Xpcall(XCallStage::Function { handler }),
                    callee: function,
                    args: extra_args,
                });
            }
            LuaValue::NativeFunction(NativeFunction::Pairs) => {
                // Only the `__pairs`-metamethod fallback is reentrant; the
                // ordinary (no `__pairs`) path below never calls back into Lua,
                // so it stays on the blocking `CallLeaf` path unchanged. Only a
                // function-valued `__pairs` gets the non-blocking treatment here
                // - a non-`Closure` one (rare) falls through to `CallLeaf`,
                // where the existing blocking `call_native` arm still handles it.
                if let Some(table) = args.first().cloned() {
                    if let Some(method @ LuaValue::Closure(_)) =
                        self.metamethod(&table, b"__pairs")?
                    {
                        return Ok(StepResult::CallNative {
                            cont: NativeCont::Once,
                            callee: method,
                            args: vec![table],
                        });
                    }
                }
                return Ok(StepResult::CallLeaf { callee, args });
            }
            LuaValue::NativeFunction(NativeFunction::DoFile) => {
                // Mirrors the existing blocking `call_native` arm's setup
                // exactly (deterministic-loader lookup, real-filesystem
                // fallback, compile - see that arm's doc comment for why), but
                // issues the resulting chunk's one call through the trampoline
                // instead of blocking on it.
                let name = match args.first() {
                    Some(value) => self.string(value)?.to_vec(),
                    None => {
                        return Err(LuaError::new(
                            "bad argument #1 to 'dofile' (value expected)",
                        ))
                    }
                };
                let source = if self.capabilities.package {
                    self.module_sources.get(&name).cloned()
                } else {
                    None
                };
                let source = match source {
                    Some(source) => source,
                    None if self.capabilities.filesystem => {
                        let real_path = bytes_to_path(&name)
                            .ok_or_else(|| LuaError::new("invalid file name"))?;
                        std::fs::read(&real_path).map_err(|error| {
                            LuaError::new(format!(
                                "cannot open '{}': {error}",
                                String::from_utf8_lossy(&name)
                            ))
                        })?
                    }
                    None if !self.capabilities.package => {
                        return Err(LuaError::new(
                            "package capability is disabled; register an in-memory module explicitly",
                        ));
                    }
                    None => {
                        return Err(LuaError::new(format!(
                            "dofile: module '{}' not found in the deterministic loader",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                };
                let closure = self.compile_chunk(&source, None).map_err(LuaError::new)?;
                return Ok(StepResult::CallNative {
                    cont: NativeCont::Once,
                    callee: closure,
                    args: Vec::new(),
                });
            }
            LuaValue::NativeFunction(NativeFunction::TableSort) => {
                // Mirrors the existing blocking `call_native` arm's setup
                // exactly (table/comparator extraction), but drives the
                // TimSort one comparison at a time through `sort_step`
                // instead of blocking on each comparator/`__lt` call.
                //
                // Gathered through `length_of`/`index_get` (not
                // `table.borrow().array.clone()`) so a table argument with
                // `__index`/`__newindex`/`__len` (a "proxy" whose real storage
                // lives behind those metamethods) is sorted correctly instead of
                // silently no-op'ing on its own empty raw array part.
                let table_value = match args.first() {
                    Some(value) => value.clone(),
                    None => {
                        return Err(LuaError::new(
                            "bad argument #1 to 'sort' (table expected, got no value)",
                        ))
                    }
                };
                self.expect_table(&table_value)?;
                // See the blocking `call_native` arm: an explicit `nil`
                // comparator must fall back to the default `<` order, not be
                // called as if it were a function.
                let comparator = args.get(1).cloned().filter(|v| *v != LuaValue::Nil);
                let size = self.length_of(table_value.clone())?;
                // See the blocking `call_native` arm: real Lua rejects an
                // oversized array before allocating anything at all.
                if size > 1 && size >= i32::MAX as i64 {
                    return Err(LuaError::new("bad argument #1 to 'sort' (array too big)"));
                }
                let mut values = Vec::with_capacity(size.max(0) as usize);
                for index in 1..=size {
                    values.push(self.index_get(table_value.clone(), LuaValue::Integer(index))?);
                }
                if values.len() <= 1 {
                    // Nothing to compare - no call can possibly be needed.
                    return Ok(StepResult::Resolved(Vec::new()));
                }
                let mut state = SortState {
                    table: table_value,
                    comparator,
                    sorter: super::tim_sort::TimSort::new(values),
                };
                return match self.sort_step(&mut state, None)? {
                    SortOutcome::Done(values) => Ok(StepResult::Resolved(values)),
                    SortOutcome::NeedsCall { callee, args } => Ok(StepResult::CallNative {
                        cont: NativeCont::Sort(state),
                        callee,
                        args,
                    }),
                };
            }
            LuaValue::NativeFunction(NativeFunction::StringGSub) => {
                // Mirrors the existing blocking `call_native` arm's setup and
                // validation exactly, but only builds `GsubState` (and drives it
                // one match/replacement at a time through `gsub_step`) when
                // `repl` is callable or a table (whose `__index` can itself be
                // callable). Other replacement forms run synchronously through
                // the shared `gsub_run_sync` helper.
                let source = match args.first() {
                    Some(LuaValue::String(source)) => source.clone(),
                    Some(value) => Rc::new(self.string(value)?.to_vec()),
                    None => {
                        return Err(LuaError::new(
                            "bad argument #1 to 'gsub' (string expected, got no value)",
                        ))
                    }
                };
                let pattern = match args.get(1) {
                    Some(value) => self.string(value)?.to_vec(),
                    None => {
                        return Err(LuaError::new(
                            "bad argument #2 to 'gsub' (string expected, got no value)",
                        ))
                    }
                };
                let repl = match args.get(2) {
                    Some(value) => value.clone(),
                    None => return Err(LuaError::new(
                        "bad argument #3 to 'gsub' (string/function/table expected, got no value)",
                    )),
                };
                if !matches!(
                    repl,
                    LuaValue::String(_)
                        | LuaValue::Integer(_)
                        | LuaValue::Float(_)
                        | LuaValue::Table(_)
                        | LuaValue::Closure(_)
                        | LuaValue::NativeFunction(_)
                        | LuaValue::RegisteredNative(_)
                        | LuaValue::GMatchIterator(_)
                ) {
                    return Err(LuaError::new(
                        "bad argument #3 to 'gsub' (string/function/table expected)",
                    ));
                }
                let max = args
                    .get(3)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .map(|value| value.max(0) as usize)
                    .unwrap_or(usize::MAX);
                let (anchored, body) = match pattern.first() {
                    Some(b'^') => (true, pattern[1..].to_vec()),
                    _ => (false, pattern.clone()),
                };
                if !matches!(
                    repl,
                    LuaValue::Closure(_)
                        | LuaValue::NativeFunction(_)
                        | LuaValue::RegisteredNative(_)
                        | LuaValue::GMatchIterator(_)
                        | LuaValue::CoroutineWrapper(_)
                        | LuaValue::Table(_)
                ) {
                    let (output, count) =
                        self.gsub_run_sync(source.clone(), &body, anchored, max, &repl)?;
                    return Ok(StepResult::Resolved(vec![
                        output,
                        LuaValue::Integer(count as i64),
                    ]));
                }
                let mut state = GsubState {
                    source,
                    body,
                    anchored,
                    max,
                    repl,
                    pos: 0,
                    count: 0,
                    changed: false,
                    output: Vec::new(),
                    last_match_end: None,
                    pending: None,
                };
                return match self.gsub_step(&mut state, None)? {
                    GsubOutcome::Done(output, count) => Ok(StepResult::Resolved(vec![
                        output,
                        LuaValue::Integer(count as i64),
                    ])),
                    GsubOutcome::NeedsCall { callee, args } => Ok(StepResult::CallNative {
                        cont: NativeCont::Gsub(state),
                        callee,
                        args,
                    }),
                };
            }
            _ => (),
        }
        // Resolve `callee` down to something actually callable, following a
        // chain of `__call` metamethods exactly as real Lua's `luaD_precall`
        // "retry" loop does: each hop that isn't itself callable prepends its
        // own (still-unresolved) value onto the front of the pending
        // argument list and moves on to *its* `__call` metamethod - a plain
        // loop, not native-stack recursion, and with no `call_depth` charge
        // per hop (a `__call` chain is O(1) C-stack in real Lua; only actual
        // nested Lua/native calls are charged). Bounded to `MAX_CALL_CHAIN`
        // hops, matching real Lua's `MAX_CCMT`, so a `__call` metatable cycle
        // reports `"'__call' chain too long"` instead of looping forever -
        // the bound applies only to successful hops (finding a `__call` and
        // moving on), not to the final terminal check, so a chain that
        // bottoms out in a genuinely uncallable value after exactly
        // `MAX_CALL_CHAIN` hops still reports the ordinary "attempt to call a
        // ... value" error, matching the pinned oracle exactly.
        //
        // This is what lets a tail call through an arbitrarily deep `__call`
        // chain resolve to a genuine `StepResult::PushClosure` (which the
        // `Instr::TailCall` dispatch above turns into `TailClosure`) instead
        // of degrading into a `CallLeaf` that blocks through the recursive,
        // charged `LuaRuntime::call` bridge and can never be a tail call.
        let mut resolved = callee;
        let mut full_args = args;
        let mut hops = 0usize;
        loop {
            if let LuaValue::Closure(closure) = &resolved {
                let proto = closure.proto.clone();
                let upvals = closure.upvals.borrow().clone();
                let globals = closure.globals.clone();
                return Ok(StepResult::PushClosure {
                    proto,
                    upvals,
                    globals,
                    args: full_args,
                    call_chain_hops: hops,
                });
            }
            if matches!(
                resolved,
                LuaValue::NativeFunction(NativeFunction::CoroutineYield)
            ) {
                if self.coroutine_stack.is_empty() {
                    return Err(LuaError::new("attempt to yield from outside a coroutine"));
                }
                if self.in_non_yieldable_call() {
                    return Err(LuaError::new("attempt to yield across a C-call boundary"));
                }
                // `coroutine.yield` never reaches `CallLeaf`/`self.call()` -
                // it's intercepted right here - so its "call" hook event (it
                // is, after all, still a real C function call from the
                // hook's perspective) has to be fired manually. The matching
                // "return" fires later, in `resume_coroutine`, when this
                // yield's result is fed back in.
                self.fire_hook("call", None)?;
                return Ok(StepResult::Yield(full_args));
            }
            if let LuaValue::CFunction(function) = &resolved {
                return match self.call_c_function_outcome(function.clone(), full_args)? {
                    c_api::CApiOutcome::Returned(values) => Ok(StepResult::Resolved(values)),
                    c_api::CApiOutcome::Yielded(values) => Ok(StepResult::Yield(values)),
                };
            }
            if matches!(
                resolved,
                LuaValue::NativeFunction(_)
                    | LuaValue::Native(_)
                    | LuaValue::RegisteredNative(_)
                    | LuaValue::GMatchIterator(_)
                    | LuaValue::CoroutineWrapper(_)
            ) {
                return Ok(StepResult::CallLeaf {
                    callee: resolved,
                    args: full_args,
                });
            }
            match self.metamethod(&resolved, b"__call")? {
                Some(next) => {
                    if hops >= MAX_CALL_CHAIN {
                        return Err(LuaError::new("'__call' chain too long"));
                    }
                    hops += 1;
                    full_args.insert(0, resolved);
                    resolved = next;
                }
                None => {
                    return Ok(StepResult::CallLeaf {
                        callee: resolved,
                        args: full_args,
                    });
                }
            }
        }
    }
}
