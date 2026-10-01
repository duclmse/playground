//! The bytecode trampoline's dispatch core: `call`/`drive`/`dispatch_step`
//! drive execution one step at a time through the explicit `Frame` stack
//! (see `frame`) instead of native Rust recursion, so execution can be
//! paused/resumed at any point (coroutines, the step debugger). Also holds
//! index/arithmetic/metamethod resolution (`index_resolve`,
//! `set_index_resolve`, `binary_resolve`, `metamethod`) and the
//! frame/register-buffer recycling pools.

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

/// Bound on `LuaRuntime::native_call_depth` - real Lua's fixed
/// `LUAI_MAXCCALLS` (`luaconf.h`), confirmed against the pinned `lua5.5`
/// oracle: recursive `coroutine.create`/`coroutine.resume` (each resume
/// dispatches the coroutine's `NativeFunction` through `call()`, then
/// synchronously re-enters the interpreter to run its body, so the whole
/// chain is genuinely Rust-stack-recursive through `call()`) and a
/// recursively-erroring `xpcall` message handler (invoked via `call()` from
/// `natives_core.rs`) both raise exactly `"C stack overflow"` at 200 levels
/// deep, not the much larger `max_call_depth` budget - see
/// `lua-5.5.1-tests/errors.lua`'s two `"C stack overflow"` assertions.
const MAX_NATIVE_CALL_DEPTH: usize = 200;

mod bytecode;

/// Whether `instr` writes `reg` as one of its destination registers -
/// conservative for multi-result/range-writing instructions (treats the
/// whole plausible range as written) so `describe_register`'s backward scan
/// never walks past a write it can't fully account for.
pub(super) fn instr_writes(instr: &Instr, reg: Reg) -> bool {
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
        | VarargIndexGet(dst, _)
        | VarargFieldGet(dst, _)
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
/// with (and, via `LuaRuntime::call_site_name`, `debug.getinfo`'s
/// `name`/`namewhat`): scans backward from just before `pc` for the most
/// recent write to `reg`, chasing through `Move` the way real Lua's
/// bytecode scan does, and gives up (returning `None`) at the first write
/// it can't name - matching real Lua's own conservative fallback rather
/// than guessing. Returns `(namewhat, name)`, mirroring real Lua's own
/// `getobjname` output pair - `namewhat` is always one of `"local"`,
/// `"upvalue"`, `"global"`, `"field"`, `"method"`.
pub(super) fn describe_register(
    proto: &Proto,
    pc: usize,
    reg: Reg,
) -> Option<(&'static str, String)> {
    let mut target = reg;
    for (index, instr) in proto.instrs[..pc].iter().enumerate().rev() {
        match instr {
            // `compile_expr` emits this self-move only after an `and`/`or`
            // join. The value may have come from either branch, so Lua has
            // no stable source name to expose in a later diagnostic.
            Instr::Move(dst, src) if *dst == target && *src == target => return None,
            Instr::Move(dst, src) if *dst == target => target = *src,
            Instr::NewLocal(dst, _, name_idx) if *dst == target => {
                let name = String::from_utf8_lossy(&name_const(proto, *name_idx)).into_owned();
                return Some(("local", name));
            }
            Instr::GetUpval(dst, index) if *dst == target => {
                let name = proto.upval_names.get(*index as usize)?;
                return Some(("upvalue", name.clone()));
            }
            Instr::GetGlobal(dst, name) if *dst == target => {
                return Some(("global", name.to_string()));
            }
            Instr::GetField(dst, receiver, name_idx) if *dst == target => {
                let name = String::from_utf8_lossy(&name_const(proto, *name_idx)).into_owned();
                // Confirmed against the pinned `lua5.5` oracle
                // (`aaa = {bbb = {}}; aaa.bbb.ccc:ddd()`,
                // `foo = {bar = {}}; foo.bar.baz.qux()`): real Lua's own
                // `getobjname` never chases *through* a `GETFIELD` to
                // describe some earlier receiver, even when the field load
                // reuses its receiver's own register (the ordinary,
                // register-reuse-optimized shape for a dotted chain like
                // `a.b.c` - table-base register freed and immediately
                // reallocated as the field's own destination). It always
                // reports the most recent field name only - `foo.bar.baz`
                // failing on `.qux` is `(field 'baz')`, never
                // `(global 'foo')` - so this stops here without recursing
                // into `receiver` at all.
                //
                // The one real exception is a lexically rebound `_ENV`:
                // `_ENV.name` lowers to a field load of `name` on `_ENV`
                // rather than a dedicated `GetGlobal`, and Lua's own
                // wording for that is `global 'name'`, not `field 'name'`.
                if let Some((receiver_kind, receiver_name)) =
                    describe_register(proto, index, *receiver)
                {
                    if matches!(receiver_kind, "local" | "upvalue") && receiver_name == "_ENV" {
                        return Some(("global", name));
                    }
                }
                // `compile_method_base` emits `GetField(base, receiver,
                // name)` immediately followed by `Move(base + 1, receiver)`
                // to materialize the implicit `self`. That bytecode shape
                // is exactly the call-site distinction Lua exposes as a
                // `method` rather than a plain `field` in diagnostics.
                // Lua's `SELF` instruction can encode its method-name
                // constant only while it remains in the RK window. Sol
                // stores global names directly in `GetGlobal` instead of
                // pooling them, so reconstruct the equivalent pressure from
                // the preceding global-name loads for diagnostic purposes.
                // Beyond that window Lua lowers the call as an ordinary
                // field access, and its error must say `field`, not
                // `method` (errors.lua's RK-limit probe).
                let rk_window_exhausted = proto.instrs[..index]
                    .iter()
                    .filter(|instruction| matches!(instruction, Instr::GetGlobal(..)))
                    .count()
                    >= 255;
                let is_method = !rk_window_exhausted && matches!(
                    proto.instrs.get(index + 1),
                    Some(Instr::Move(self_reg, source))
                        if *self_reg == target + 1 && *source == *receiver
                );
                let kind = if is_method { "method" } else { "field" };
                return Some((kind, name));
            }
            // A named-vararg parameter's `t.xx` field read lowers to this
            // dedicated instruction instead of `GetField` (see
            // `vararg_in_range_index`/`materialize_vararg_view`'s lazy
            // view), but real Lua's `getobjname` has no such distinction -
            // the vararg table is materialized already, and a field read
            // off it is exactly the `field 'xx'` shape `GetField` reports.
            Instr::VarargFieldGet(dst, name_idx) if *dst == target => {
                let name = String::from_utf8_lossy(&name_const(proto, *name_idx)).into_owned();
                return Some(("field", name));
            }
            other if instr_writes(other, target) => return None,
            _ => {}
        }
    }
    // No instruction in `[0, pc)` ever wrote `target` - real Lua's
    // `getobjname` would still resolve it via `getlocalname`'s static
    // `locvars` lookup rather than giving up, and the only Sol register
    // that shape describes is a function parameter: it arrives already
    // populated by the calling convention, with no `Instr::NewLocal` (or
    // any other write) to chase. `param_names` is exactly that minimal,
    // `upval_names`-style debug metadata for this one case.
    let name = proto.param_names.get(target as usize)?;
    Some(("local", name.clone()))
}

/// Appends real Lua's "(global 'x')"/"(field 'x')" suffix to a fresh
/// "attempt to index a ... value" error, when the indexed base's source is
/// nameable - never touches an already-annotated or unrelated error (e.g.
/// the metatable-chain-too-long error also raised from this path).
fn annotate_index_error(err: &mut LuaError, proto: &Proto, pc: usize, base: Reg) {
    if err.message.starts_with("attempt to index a ") && err.message.ends_with(" value") {
        if let Some((kind, name)) = describe_register(proto, pc, base) {
            err.message = format!("{} ({kind} '{name}')", err.message);
        }
    }
}

/// Mirrors `sol_core::heap`'s private float-table-key normalization (a
/// float with no fractional part, in `i64` range, becomes that integer) -
/// duplicated here since it's `sol-core`-private and the lazy vararg
/// view's fast path needs it before ever reaching the canonical heap's own
/// key encoding (see `vararg_in_range_index`).
fn exact_integer(value: f64) -> Option<i64> {
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if value.is_finite() && value.fract() == 0.0 && value >= i64::MIN as f64 && value < I64_UPPER_EXCLUSIVE
    {
        Some(value as i64)
    } else {
        None
    }
}

/// Whether `key` normalizes (exactly like a real table key would) to a
/// 1-based index within `len` (the lazy named-vararg view's current
/// `#varargs`) - the only key shape `Instr::VarargIndexGet`/
/// `VarargIndexSet`'s fast path answers without materializing a real
/// `Table`. Returns the 1-based index itself (so callers can subtract 1
/// for `varargs`'s 0-based storage).
fn vararg_in_range_index(key: &LuaValue, len: usize) -> Option<usize> {
    let integer = match key {
        LuaValue::Integer(i) => *i,
        LuaValue::Float(f) => exact_integer(*f)?,
        _ => return None,
    };
    (integer >= 1 && (integer as usize) <= len).then_some(integer as usize)
}

/// `Instr::VarargIndexGet`'s unmaterialized fast path: exactly what a real
/// `table.pack`-shaped table would return for `key` - the `i`'th vararg for
/// an in-range positive integer, the vararg count for the string key
/// `"n"`, `Nil` for everything else (including out-of-range/non-integral
/// keys, `0`/negative integers, and unrelated types) - without allocating.
fn vararg_view_get(varargs: &[LuaValue], key: &LuaValue) -> LuaValue {
    if let Some(index) = vararg_in_range_index(key, varargs.len()) {
        return varargs[index - 1].clone();
    }
    if matches!(key, LuaValue::String(s) if s.as_bytes() == b"n") {
        return LuaValue::Integer(varargs.len() as i64);
    }
    LuaValue::Nil
}

/// The call counterpart of `annotate_index_error`. Lua's debug-name lookup
/// uses the instruction that loaded the callee, so a direct global or field
/// call retains its useful source name even when the callee is a non-function.
fn annotate_call_error(err: &mut LuaError, proto: &Proto, pc: usize, base: Reg) {
    if err.message.starts_with("attempt to call a ") && err.message.ends_with(" value") {
        // Real Lua's `funcnamefromcode` (`ldebug.c`) hardcodes `namewhat`/
        // `name` to the literal "for iterator" for `OP_TFORCALL` rather than
        // tracing the callee register's origin - the generic-`for` iterator
        // slot is a fixed calling-convention position, not a named variable.
        if matches!(proto.instrs.get(pc), Some(Instr::TForCall(b, _)) if *b == base) {
            err.message = format!("{} (for iterator 'for iterator')", err.message);
            return;
        }
        if let Some((kind, name)) = describe_register(proto, pc, base) {
            err.message = format!("{} ({kind} '{name}')", err.message);
        }
    }
}

/// Real Lua's `luaL_argerror` (`lauxlib.c`): when the call that raised a
/// `"bad argument #N to 'NAME'"` error was itself a method call (`t:name(...)`),
/// argument numbers exclude the implicit `self` (`arg--`), and if the
/// decremented number reaches 0 - the self argument itself failed - the
/// message becomes `"calling 'NAME' on bad self (EXTRAMSG)"` instead of
/// `"bad argument #0 to 'NAME'"` (`lua-5.5.1-tests/errors.lua`'s `aaa:sub()`/
/// `('a'):sub{}` tests). Mirrors `annotate_call_error`'s pattern: a post-hoc
/// rewrite driven by the same `describe_register` call-site name resolution,
/// applied only when the raised message already carries the "bad argument
/// #N to 'NAME'" shape a native function itself produced (`checked_string`/
/// `checked_integer` in `natives.rs`, or any of the other native functions
/// that already format their own argument-check errors this way).
fn annotate_bad_argument_error(err: &mut LuaError, proto: &Proto, pc: usize, base: Reg) {
    let Some(("method", name)) = describe_register(proto, pc, base) else {
        return;
    };
    let Some(rest) = err.message.strip_prefix("bad argument #") else {
        return;
    };
    let Some((number, rest)) = rest.split_once(" to '") else {
        return;
    };
    let Ok(argument) = number.parse::<u32>() else {
        return;
    };
    let Some(rest) = rest.strip_prefix(&format!("{name}' (")) else {
        return;
    };
    err.message = match argument {
        0 => return,
        1 => format!("calling '{name}' on bad self ({rest}"),
        n => format!("bad argument #{} to '{name}' ({rest}", n - 1),
    };
}

impl LuaRuntime {
    /// Interns `bytes` as a canonical Lua string value. Strings are leaves in
    /// the value graph (never reference other values), so unlike
    /// tables/closures/coroutines they are already fully canonical.
    pub(super) fn intern_str(&self, bytes: impl AsRef<[u8]>) -> CanonicalString {
        CanonicalString::intern(self.canonical_heap.clone(), bytes)
    }

    /// Allocates `bytes` as a fresh canonical Lua string value with its own
    /// identity, never aliasing an existing equal-content string. Use this
    /// for every runtime-computed string (concatenation, string-library
    /// results, formatted output, ...); use `intern_str` only for
    /// compile-time literal constants and fixed structural labels.
    pub(super) fn fresh_str(&self, bytes: impl AsRef<[u8]>) -> CanonicalString {
        CanonicalString::fresh(self.canonical_heap.clone(), bytes)
    }

    /// Semantic-ABI entry point used by specialized-tier adapters. The
    /// callable remains a normal value in the dynamic global environment;
    /// only scalar boxing policy belongs in the adapter.
    pub fn call_global_outcome(
        &mut self,
        name: &str,
        args: Vec<LuaValue>,
    ) -> sol_core::CallOutcome<LuaValue, LuaError> {
        match self.call(self.globals.get(self, name), args) {
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
        if self.native_call_depth >= MAX_NATIVE_CALL_DEPTH {
            // See `MAX_NATIVE_CALL_DEPTH`'s doc - this check must come
            // before the general `call_depth` budget below and its message
            // must stay exactly `"C stack overflow"` (no position prefix,
            // no `(...)` suffix): `errors.lua` asserts `msg == "C stack
            // overflow"` verbatim for the recursive-message-handler case.
            return Err(LuaError::new("C stack overflow"));
        }
        if self.call_depth >= self.max_call_depth {
            // Real Lua's own message for this condition is the plain
            // string "stack overflow" (`ldo.c`'s `luaD_growstack`/
            // `luaE_incCstack` via `LUAI_MAXCCALLS`), and a fair number of
            // corpus files (`errors.lua`, `coroutine.lua`, `locals.lua`)
            // assert on `string.find(msg, "stack overflow")` after a deep
            // recursion. Keep the budget wording (which env-var docs and
            // other diagnostics reference) but include the substring real
            // scripts actually search for. See `call_depth_overflow_error`
            // for the second-overflow-in-a-row ("error in error handling")
            // case.
            return Err(self.call_depth_overflow_error());
        }
        self.call_depth += 1;
        self.native_call_depth += 1;
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
        let hook_callee = fires_hook.then(|| value.clone());
        let hook_args = fires_hook.then(|| args.clone());
        let result = (|| -> LuaResult<Vec<LuaValue>> {
            if fires_hook {
                let mut roots = Vec::with_capacity(args.len() + 1);
                roots.push(self.encode_value(&value)?);
                for arg in &args {
                    roots.push(self.encode_value(arg)?);
                }
                self.pinned_roots.push(roots);
                let previous_callee = if self.running_hook {
                    None
                } else {
                    std::mem::replace(&mut self.hook_event_callee, Some(value.clone()))
                };
                let previous_transfer = self.hook_transfer.replace(HookTransfer {
                    first: 1,
                    values: args.clone(),
                    temporary_name: b"(C temporary)",
                });
                let hook_result = self.fire_hook("call", None);
                self.hook_transfer = previous_transfer;
                if !self.running_hook {
                    self.hook_event_callee = previous_callee;
                }
                self.pinned_roots.pop();
                hook_result?;
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
                LuaValue::CoroutineWrapper(thread) => self.call_coroutine_wrapper(thread, args),
                LuaValue::Closure(closure) => {
                    let (proto, upvals, globals) = self.closure_parts(closure)?;
                    let name = proto.metadata.name.clone();
                    self.call_closure(closure, proto, upvals, globals, args)
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
            if fires_hook {
                if let Ok(values) = &result {
                    let roots = values
                        .iter()
                        .map(|value| self.encode_value(value))
                        .collect::<LuaResult<Vec<_>>>()?;
                    self.pinned_roots.push(roots);
                    let previous_callee = std::mem::replace(
                        &mut self.hook_event_callee,
                        hook_callee.clone(),
                    );
                    let first = match (&hook_callee, &hook_args) {
                        (Some(LuaValue::NativeFunction(NativeFunction::Select)), Some(args))
                            if !values.is_empty() && values.len() <= args.len() =>
                        {
                            args.len() - values.len() + 1
                        }
                        (_, Some(args)) => args.len() + 1,
                        _ => 1,
                    };
                    let previous_transfer = self.hook_transfer.replace(HookTransfer {
                        first,
                        values: values.clone(),
                        temporary_name: b"(C temporary)",
                    });
                    let hook_result = self.fire_hook("return", None);
                    self.hook_transfer = previous_transfer;
                    self.hook_event_callee = previous_callee;
                    self.pinned_roots.pop();
                    hook_result?;
                }
            }
            result
        })();
        self.release_call_depth(1);
        self.native_call_depth -= 1;
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
        let inferred_transfer = if matches!(event, "call" | "tail call") && self.hook_transfer.is_none()
            && self.hook_event_callee.is_none()
        {
            match self.frames.last() {
                Some(Frame::Lua(frame)) => Some(HookTransfer {
                    first: 1,
                    values: (0..frame.proto.metadata.arity.parameters as usize)
                        .map(|index| frame.regs.get(index).cloned().unwrap_or(LuaValue::Nil))
                        .collect(),
                    temporary_name: b"",
                }),
                _ => None,
            }
        } else {
            None
        };
        let previous_transfer = inferred_transfer
            .map(|transfer| self.hook_transfer.replace(transfer));
        let mut args = vec![LuaValue::String(self.intern_str(event.as_bytes()))];
        if let Some(line) = line {
            args.push(LuaValue::Integer(line));
        }
        self.running_hook = true;
        self.hook_callback_frame = Some(self.frames.len());
        let result = self.call(hook.callback.clone(), args);
        if let Some(previous) = previous_transfer {
            self.hook_transfer = previous;
        }
        self.hook_callback_frame = None;
        self.running_hook = false;
        result.map(|_| ())
    }

    /// Runs a debug-hook callback while `frame` is temporarily absent from
    /// `self.frames`. `drive_result` pops a Lua frame before dispatching an
    /// instruction, so a hook invoked from that instruction can itself call
    /// `collectgarbage()` while every value reachable only from the
    /// interrupted frame would otherwise look dead. Pin the same precise
    /// roots `frame_roots` would have found had the frame remained on the
    /// stack for the callback's complete dynamic extent.
    fn fire_hook_for_active_frame(
        &mut self,
        event: &'static str,
        line: Option<i64>,
        frame: &LuaFrame,
        returned: Option<&[LuaValue]>,
    ) -> LuaResult<()> {
        let mut roots = Vec::new();
        self.push_lua_frame_roots(frame, &mut roots);
        self.pinned_roots.push(roots);
        let interrupted = frame
            .proto
            .source_map
            .location(frame.header.pc)
            .map(|location| {
                let source = self.traceback_frame_label(&frame.proto, location.line);
                if frame.proto.line_defined == 0 {
                    format!("{source} in main chunk")
                } else {
                    format!("{source} in function <{}>", frame.proto.line_defined)
                }
            });
        let previous = std::mem::replace(&mut self.hook_interrupted_frame, interrupted);
        let current_line = frame.proto.source_map.location(frame.header.pc)
            .map_or(-1, |location| location.line as i64);
        let previous_info = self.hook_interrupted_info.replace((
            frame.proto.clone(), frame.closure, current_line,
        ));
        let previous_locals = self.hook_interrupted_locals.replace(frame.clone());
        let previous_transfer = returned.map(|values| {
            let first = match frame.proto.instrs.get(frame.header.pc as usize) {
                Some(Instr::Return(base, _)) => {
                    *base as usize + 1 + usize::from(frame.proto.metadata.arity.variadic)
                }
                _ => 1,
            };
            self.hook_transfer.replace(HookTransfer {
                first,
                values: values.to_vec(),
                temporary_name: b"(temporary)",
            })
        });
        let result = self.fire_hook(event, line);
        if let Some(previous) = previous_transfer {
            self.hook_transfer = previous;
        }
        self.hook_interrupted_locals = previous_locals;
        self.hook_interrupted_info = previous_info;
        self.hook_interrupted_frame = previous;
        self.pinned_roots.pop();
        result
    }

    /// Per-instruction `line`/`count` hook check, called from `dispatch_step`'s
    /// `'exec: loop` right after `frame.header.pc` is set to the instruction
    /// about to execute. Cheap no-op when no hook is active (checked before it
    /// is even called) or when neither mask bit is set. Count-hook decrementing
    /// happens even while a "line" event doesn't fire and vice versa - real
    /// Lua's `hookcount`/line-change checks are independent of each other.
    fn fire_line_and_count_hooks(&mut self, frame: &mut LuaFrame, pc: usize) -> LuaResult<()> {
        c_api::fire_c_instruction_hooks(self, frame, pc)?;
        // Lua disables all debug-hook accounting while executing the hook
        // callback itself.  In particular, a count hook must not consume its
        // own callback's instructions; otherwise a callback such as
        // `function () a = a + 1 end` makes every count interval collapse to
        // one VM instruction.
        if self.running_hook {
            return Ok(());
        }
        let Some(hook) = self.active_hook.clone() else {
            return Ok(());
        };
        if hook.mask.count {
            let remaining = hook.count_remaining.get() - 1;
            if remaining <= 0 {
                hook.count_remaining.set(hook.count);
                self.fire_hook_for_active_frame("count", None, frame, None)?;
            } else {
                hook.count_remaining.set(remaining);
            }
        }
        if hook.mask.line {
            if let Some(location) = frame.proto.source_map.location(pc as u32) {
                let line = location.line as i64;
                let pc = pc as i64;
                // Fires again whenever the source line actually changes, or
                // whenever `pc` jumps back to (or before) the *immediately
                // preceding* instruction this frame examined - a loop's
                // back-edge re-executing an earlier, possibly identical,
                // line. That comparison point (`frame.hook_last_pc`) has to
                // be updated on every instruction this function sees, not
                // only the ones that actually fire a "line" event - real
                // Lua's own equivalent (`oldpc` in `luaG_traceexec`) runs
                // unconditionally the same way. Otherwise, inside a loop
                // whose whole body sits on one source line (so only the
                // *first* instruction of the first iteration ever changes
                // `frame.hook_last_line` and thus updates this watermark),
                // every later iteration's back-edge would land on a `pc`
                // *greater* than that stale, early watermark and never
                // register as "backward" - silently swallowing every
                // repeat-iteration line event after the first. Only
                // `frame.hook_last_line` stays scoped to actual fires, since
                // it exists purely to dedupe consecutive same-line, non-
                // backward instructions within one logical line.
                if line != frame.hook_last_line || pc <= frame.hook_last_pc {
                    frame.hook_last_line = line;
                    self.fire_hook_for_active_frame("line", Some(line), frame, None)?;
                }
                frame.hook_last_pc = pc;
            } else if frame.proto.source_map.is_empty() && pc == 0 {
                // A stripped Lua prototype retains its executable code but
                // has no line-info record for its first instruction.  Lua
                // still delivers that initial line-hook boundary, with a
                // nil line argument (the behavior exercised by db.lua's
                // stripped-function probe), rather than silently omitting
                // the event altogether.
                frame.hook_last_pc = pc as i64;
                self.fire_hook_for_active_frame("line", None, frame, None)?;
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
        // Provider 0 is `CanonicalAdapter::import_native_function`'s "portable
        // Lua standard library" registry key (`canonical.rs`) - a
        // `NativeFunction` that got imported into the canonical heap (for
        // example, as a file-handle metatable entry: see `FileGc`'s doc
        // comment in `natives_os_io.rs`) reads back out through ordinary
        // indexing as this same `CFunction` shape (`canonical_to_lua`), so
        // calling it must route back to `call_native` instead of the
        // `c_functions` registry below, which is reserved for the embedder's
        // own `lua_pushcfunction`-registered callables (provider 1/2).
        if callable_id.provider == 0 && callable_id.function == NativeFunction::FileGc as u32 {
            let values = self.call_native(NativeFunction::FileGc, args)?;
            return Ok(c_api::CApiOutcome::Returned(values));
        }
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
        let mut heap = self.canonical_heap.borrow_mut();
        cells.extend(captured_registers.iter().map(|&captured| {
            if captured {
                Some(heap.alloc_upvalue(sol_core::Value::NIL, None))
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
        closure: ClosureRef,
        proto: Rc<Proto>,
        upvals: Rc<[std::cell::Cell<sol_core::ObjectId>]>,
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
        let mut frame = self.new_lua_frame(closure, proto, upvals, globals, args, 0)?;
        // `call_closure` is also used by runtime-originated calls such as a
        // table finalizer.  Those do not pass through `drive_result`'s
        // `PushClosure` arm, which normally consumes this label.
        frame.entry_label = self.pending_frame_label.take();
        self.frames.push(Frame::Lua(frame));
        let mut depth_charged: usize = 0;
        match self.drive(base_depth, &mut depth_charged, None) {
            DriveOutcome::Returned(values) => Ok(values),
            DriveOutcome::Yielded(_) => {
                let error = LuaError::new("attempt to yield across a C-call boundary");
                let error = self.close_frames_above(base_depth, error);
                self.release_call_depth(depth_charged);
                Err(error)
            }
            DriveOutcome::Raised(error) => {
                let error = self.close_frames_above(base_depth, error);
                self.release_call_depth(depth_charged);
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
        closure: ClosureRef,
        proto: Rc<Proto>,
        upvals: Rc<[std::cell::Cell<sol_core::ObjectId>]>,
        globals: Globals,
        args: Vec<LuaValue>,
        call_chain_hops: usize,
    ) -> LuaResult<LuaFrame> {
        let function = self.prototype_id(&proto)?;
        // U9 baseline JIT hot-function counter (single choke point for every
        // Lua-function activation): on crossing `promote_threshold()`,
        // attempt promotion. `wrapping_add` because a `Proto` run far more
        // than `u32::MAX` times should keep running, not panic - it simply
        // stops re-triggering `try_promote` at the exact power-of-threshold
        // boundary again until the counter wraps back around, which is
        // harmless (promotion is idempotent: `try_promote` is a no-op once
        // `native_status` is no longer `Interpreted`).
        let call_count = proto.call_count.get().wrapping_add(1);
        proto.call_count.set(call_count);
        if call_count == dynjit::promote_threshold() {
            self.try_promote(&proto);
        }
        let register_count = proto.metadata.registers as usize;
        let parameter_count = proto.metadata.arity.parameters as usize;
        self.pinned_roots.push(vec![sol_core::Value::object(closure.object_id())]);
        let charge = self.charge_allocation(
            (register_count + proto.captured_cell_count) * std::mem::size_of::<LuaValue>(),
            None,
        );
        self.pinned_roots.pop();
        charge?;
        let mut regs = self.take_regs_buffer(register_count);
        let cells = self.take_cells_buffer(&proto.captured_registers);
        for i in 0..parameter_count.min(register_count) {
            let value = args.get(i).cloned().unwrap_or(LuaValue::Nil);
            reg_set(self, &mut regs, &cells, i, value);
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
        // `vararg_lazy`: the register is left at its default `Nil` and
        // answered directly out of `varargs` by the `Vararg*` opcodes (see
        // `materialize_vararg_view` and `dispatch/bytecode.rs`) - no table
        // allocation unless some rare shape (an out-of-range/non-integer
        // write) forces materialization later.
        if let Some(vararg_reg) = proto.vararg_name {
            if !proto.vararg_lazy {
                let table = self.values_table(&varargs)?;
                let LuaValue::Table(named_varargs) = &table else {
                    unreachable!("values_table always returns a table")
                };
                let named_varargs = *named_varargs;
                let n_key = LuaValue::String(self.intern_str(b"n"));
                self.table_set(named_varargs, n_key, LuaValue::Integer(varargs.len() as i64))?;
                reg_set(self, &mut regs, &cells, vararg_reg as usize, table);
            }
        }
        Ok(LuaFrame {
            header: FrameHeader::new(function, 0, 0),
            closure,
            proto,
            upvals,
            globals,
            regs,
            cells,
            varargs,
            call_chain_hops,
            is_tail_call: false,
            pending: Pending::None,
            to_close: Vec::new(),
            entry_label: None,
            hook_last_pc: -1,
            hook_last_line: -1,
            c_hook_last_pc: -1,
            c_hook_last_line: -1,
        })
    }

    /// Builds the lazy named-vararg view's real backing `Table` on demand -
    /// the same `values_table`+`table_set("n", ...)` construction
    /// `new_lua_frame` runs eagerly for a non-lazy `Proto`, but run lazily
    /// here only when some rare access (an out-of-range/non-integer-key
    /// write, or a `VarargFieldSet`) actually needs full table semantics.
    /// Writes the new table into the vararg register and returns it; a
    /// no-op (just returning the existing table) if some earlier access
    /// already materialized it.
    fn materialize_vararg_view(&mut self, frame: &mut LuaFrame) -> LuaResult<TableRef> {
        let vararg_reg = frame
            .proto
            .vararg_name
            .expect("materialize_vararg_view only called for a named vararg parameter")
            as usize;
        if let LuaValue::Table(table) = reg_get(self, &frame.regs, &frame.cells, vararg_reg) {
            return Ok(table);
        }
        let table = self.values_table(&frame.varargs)?;
        let LuaValue::Table(named_varargs) = &table else {
            unreachable!("values_table always returns a table")
        };
        let named_varargs = *named_varargs;
        let n_key = LuaValue::String(self.intern_str(b"n"));
        self.table_set(named_varargs, n_key, LuaValue::Integer(frame.varargs.len() as i64))?;
        reg_set(self, &mut frame.regs, &frame.cells, vararg_reg, table);
        Ok(named_varargs)
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
                                self.fire_hook_for_active_frame("return", None, &frame, Some(&values))?;
                            }
                            self.recycle_frame_buffers(
                                std::mem::take(&mut frame.regs),
                                std::mem::take(&mut frame.cells),
                            );
                            self.recycle_values_buffer(std::mem::take(&mut frame.varargs));
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(values));
                            }
                            incoming = Some(values);
                        }
                        Ok(StepResult::PushClosure {
                            closure,
                            proto,
                            upvals,
                            globals,
                            args,
                            call_chain_hops,
                        }) => {
                            if self.call_depth >= self.max_call_depth {
                                let entry_label = frame.entry_label;
                                // This frame stays on `self.frames` (unlike
                                // the `dispatch_step` `Err` arm below), so
                                // `unwind_error_to_marker`'s own frame walk
                                // records its position - adding it again
                                // here would duplicate the entry.
                                self.frames.push(Frame::Lua(frame));
                                let error = self.call_depth_overflow_error();
                                let error = if error.double_fault {
                                    error
                                } else {
                                    match entry_label {
                                        Some(label) => {
                                            error.at(&format!("in metamethod '{label}'"))
                                        }
                                        None => error,
                                    }
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
                                    closure,
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
                            closure,
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
                            let mut replacement =
                                self.new_lua_frame(closure, proto, upvals, globals, args, call_chain_hops)?;
                            replacement.is_tail_call = true;
                            self.frames.push(Frame::Lua(replacement));
                            self.fire_hook("tail call", None)?;
                        }
                        Ok(StepResult::CallLeaf { callee, args }) => {
                            let call_proto = frame.proto.clone();
                            let call_pc = frame.header.pc as usize;
                            let call_base = match frame.pending {
                                Pending::Call { base, .. } => Some(base as Reg),
                                Pending::TailCall => match call_proto.instrs.get(call_pc) {
                                    Some(Instr::TailCall(base, _)) => Some(*base),
                                    _ => None,
                                },
                                Pending::TForCall { base, .. } => Some(base as Reg),
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
                                        annotate_bad_argument_error(
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
                                    // This leaf call resolves through
                                    // `step_result_for_call`/`CallLeaf` rather
                                    // than `dispatch_step`'s direct `Err`
                                    // return, so it never reaches the other
                                    // `Err(mut error) =>` arm below that
                                    // otherwise owns this prefixing - apply
                                    // the same implicit-error convention here
                                    // (see that arm's comment) so calling a
                                    // non-callable value gets a position
                                    // prefix too. Applied last, after the
                                    // annotators above that pattern-match on
                                    // the unprefixed message text.
                                    if error.value.is_none() {
                                        if let Some(prefix) =
                                            self.runtime_error_prefix(&call_proto, call_pc as u32)
                                        {
                                            error.message = format!("{prefix}{}", error.message);
                                        }
                                    }
                                    // The calling frame's own position isn't
                                    // added here - it stayed on
                                    // `self.frames` (pushed above), so
                                    // `unwind_error_to_marker`'s frame walk
                                    // records it, along with every other
                                    // call-chain level between here and the
                                    // enclosing pcall/xpcall marker.
                                    match self.unwind_error_to_marker(
                                        match entry_label {
                                            Some(label) => {
                                                error.at(&format!("in metamethod '{label}'"))
                                            }
                                            None => error,
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
                        Err(mut error) => {
                            // Only errors the VM itself synthesizes (no
                            // `LuaError::value`) get an automatic position
                            // prefix here - explicit `error()`/`assert()`
                            // calls always carry a `value` and already added
                            // their own prefix via `where_prefix` before
                            // reaching this unwind site, matching real Lua's
                            // `luaG_runerror` (implicit) vs `luaB_error`
                            // (explicit) split. This fires exactly once per
                            // error, at the innermost frame `dispatch_step`
                            // raised it from - `unwind_error_to_marker` only
                            // pops outer frames to close `<close>` values, it
                            // never re-drives them through this arm.
                            if error.value.is_none() {
                                if let Some(prefix) =
                                    self.runtime_error_prefix(&frame.proto, frame.header.pc)
                                {
                                    error.message = format!("{prefix}{}", error.message);
                                }
                            }
                            let error = match frame.proto.source_map.location(frame.header.pc) {
                                Some(location) => {
                                    error.at(&self.traceback_frame_label(&frame.proto, location.line))
                                }
                                None => error,
                            };
                            let error = match frame.entry_label {
                                Some(label) => error.at(&format!("in metamethod '{label}'")),
                                None => error,
                            };
                            // `frame` was popped above (line 845) and never
                            // pushed back for this arm, unlike the
                            // `PushClosure` overflow arm above - so unlike
                            // that arm, it is no longer in `self.frames` for
                            // `unwind_error_to_marker`'s own `removed =
                            // self.frames.len() - marker_index` frame count
                            // to see. Release its own charge here first (the
                            // same `self.frames.len() > base_depth` gate
                            // `finish_frame` uses, since a bare base frame
                            // was charged by `call`'s own bookkeeping, not
                            // `depth_charged`), or a runtime error caught by
                            // an ancestor's pcall/xpcall silently leaks one
                            // `call_depth` unit per occurrence.
                            if self.frames.len() > base_depth {
                                self.release_call_depth(1);
                                *depth_charged -= 1;
                            }
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
                        NativeCont::Pcall => {
                            self.fire_hook("return", None)?;
                            let mut wrapped = vec![LuaValue::Bool(true)];
                            wrapped.extend(values);
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(wrapped));
                            }
                            incoming = Some(wrapped);
                        }
                        NativeCont::Xpcall(XCallStage::Function { entry_retry_depth, .. }) => {
                            // `f` returned normally - this protected call is
                            // done, so restore the `nCcalls`-equivalent save
                            // point exactly as if it had never been charged
                            // (see `XCallStage::Function`'s doc).
                            self.xcall_retry_depth = entry_retry_depth;
                            self.fire_hook("return", None)?;
                            let mut wrapped = vec![LuaValue::Bool(true)];
                            wrapped.extend(values);
                            if self.finish_frame(base_depth, depth_charged) {
                                return Ok(DriveOutcome::Returned(wrapped));
                            }
                            incoming = Some(wrapped);
                        }
                        NativeCont::Xpcall(XCallStage::Handler { entry_retry_depth, .. }) => {
                            // The message handler itself returned normally
                            // (no further error) - this `xpcall` is done,
                            // same restore as the `Function` arm above. `f`
                            // (or a retry of the handler) *did* error for
                            // this stage to be reached at all, so - unlike
                            // that arm - this is also one of the points real
                            // Lua's `luaD_pcall` shrinks the stack back down
                            // after catching a non-OK status; see
                            // `reset_call_depth_overflow`'s doc.
                            self.xcall_retry_depth = entry_retry_depth;
                            self.reset_call_depth_overflow();
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
                                    match self.resolve_call(
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
                                    match self.resolve_call(
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
            self.release_call_depth(1);
            *depth_charged -= 1;
        }
        self.frames.len() == base_depth
    }

    /// Releases `n` `call_depth` units, clearing `call_depth_overflowed_once`
    /// once the whole call stack has drained back to empty - see that
    /// field's doc for why the flag isn't cleared on every unwind.
    fn release_call_depth(&mut self, n: usize) {
        self.call_depth -= n;
        if self.call_depth == 0 {
            self.call_depth_overflowed_once = false;
        }
    }

    /// Builds the error for a `call_depth >= max_call_depth` check site (see
    /// `call_depth_overflowed_once`'s doc on `LuaRuntime`): the first crossing
    /// gets an ordinary, catchable `"stack overflow (...)"` error and arms
    /// the flag; hitting the check again before the stack has drained back
    /// to `call_depth == 0` gets a `double_fault` error instead, matching
    /// real Lua's `luaD_growstack` throwing `LUA_ERRERR` directly the second
    /// time round rather than raising another catchable overflow.
    fn call_depth_overflow_error(&mut self) -> LuaError {
        if self.call_depth_overflowed_once {
            LuaError::new("error in error handling").make_double_fault()
        } else {
            self.call_depth_overflowed_once = true;
            LuaError::new("stack overflow (Lua call-depth budget exhausted)")
        }
    }

    /// Clears `call_depth_overflowed_once`. Called at every point a
    /// `pcall`/`xpcall` marker finally, terminally resolves an error it
    /// caught (as opposed to an `xpcall` message-handler retry routing back
    /// into the *same* marker, or a protected call returning with no error
    /// at all) - real Lua's `luaD_pcall` unconditionally runs
    /// `luaD_shrinkstack` whenever it catches a non-`LUA_OK` status from its
    /// protected call, regardless of what that status was, which is what
    /// lets an unrelated, later deep recursion get its own fresh, catchable
    /// first overflow instead of an immediate `"error in error handling"`.
    fn reset_call_depth_overflow(&mut self) {
        self.call_depth_overflowed_once = false;
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
            let error = self.call_depth_overflow_error();
            return self.unwind_error_to_marker(error, base_depth, depth_charged);
        }
        // Only `xpcall`'s own protected-call/message-handler dispatch
        // mirrors real Lua's `nCcalls` budget here (see `XCallStage`'s doc) -
        // `Pcall`/`Once`/`Sort`/`Gsub` markers never retry themselves the way
        // an `xpcall` message handler can, so they can never accumulate this
        // charge unboundedly and don't need to be charged for it at all. The
        // budget ceiling itself is enforced by the caller
        // (`unwind_error_to_marker`'s `XCallStage::Handler` arm), which is
        // the only place a retry is ever decided, not here.
        if matches!(cont, NativeCont::Xpcall(_)) {
            self.xcall_retry_depth += 1;
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
        // `callee` here is always a callback native code is about to invoke
        // on the Lua-visible function's behalf (`pcall`/`xpcall`'s protected
        // function, `table.sort`'s comparator, `string.gsub`'s replacement) -
        // never the wrapping native itself, which the bytecode `Instr::Call`
        // (or a nested reentrant native call) already named at its own call
        // site. It therefore never has a bytecode call site of its own, so
        // real Lua's `pushglobalfuncname` fallback naming applies uniformly
        // here, matching `resolve_call`'s callers.
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
    /// this), `drive`'s `NativeCont::Sort`/`NativeCont::Gsub` resume arms
    /// (which push an *uncharged* continuation marker - the same logical call
    /// as before, not a new one - before calling this), and
    /// `resume_coroutine` (a coroutine body's first call). Every caller's
    /// `callee` is, by construction, a callback native code is invoking on
    /// the Lua-visible function's behalf - `pcall`/`xpcall`'s protected
    /// function, `table.sort`'s comparator, `string.gsub`'s replacement, a
    /// coroutine's body - never something reached through a bytecode
    /// `Instr::Call` (that path uses `annotate_call_error`/
    /// `annotate_bad_argument_error` instead, over on the `CallLeaf`
    /// dispatch). Such a callee never has a bytecode call site of its own,
    /// so a "bad argument" error from it is named using real Lua's
    /// `pushglobalfuncname` fallback (`qualify_callback_argument_error`),
    /// confirmed against the pinned Lua 5.5.1 oracle: `table.sort({1,2,3},
    /// table.sort)` and `coroutine.resume(coroutine.create(table.sort))`
    /// both report `bad argument #1 to 'table.sort' (...)` (qualified),
    /// while `table.sort({1,2,3}, 5)` - an ordinary bytecode-issued call -
    /// reports bare `'sort'`.
    pub(super) fn resolve_call(
        &mut self,
        callee: LuaValue,
        args: Vec<LuaValue>,
        base_depth: usize,
        depth_charged: &mut usize,
    ) -> LuaResult<CallStep> {
        let callee_for_naming = callee.clone();
        match self.step_result_for_call(callee, args) {
            Ok(StepResult::PushClosure {
                closure,
                proto,
                upvals,
                globals,
                args,
                call_chain_hops,
            }) => {
                if self.call_depth >= self.max_call_depth {
                    let error = self.call_depth_overflow_error();
                    return self.unwind_error_to_marker(error, base_depth, depth_charged);
                }
                self.call_depth += 1;
                *depth_charged += 1;
                let child = self.new_lua_frame(closure, proto, upvals, globals, args, call_chain_hops)?;
                self.frames.push(Frame::Lua(child));
                self.fire_hook("call", None)?;
                Ok(CallStep::Pending)
            }
            Ok(StepResult::TailClosure { .. }) => {
                unreachable!("only TailCall instruction dispatch creates TailClosure")
            }
            Ok(StepResult::CallLeaf { callee, args }) => match self.call(callee, args) {
                Ok(values) => Ok(CallStep::Done(values)),
                Err(mut error) => {
                    self.qualify_callback_argument_error(&mut error, &callee_for_naming);
                    self.unwind_error_to_marker(error, base_depth, depth_charged)
                }
            },
            Ok(StepResult::CallNative { cont, callee, args }) => {
                self.push_native_call(cont, callee, args, base_depth, depth_charged)
            }
            Ok(StepResult::Resolved(values)) => Ok(CallStep::Done(values)),
            Ok(StepResult::Yield(values)) => Ok(CallStep::Yielded(values)),
            Ok(StepResult::Done(_)) => unreachable!("step_result_for_call never returns Done"),
            Err(mut error) => {
                self.qualify_callback_argument_error(&mut error, &callee_for_naming);
                self.unwind_error_to_marker(error, base_depth, depth_charged)
            }
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
                .map(|e| e.into_lua_value(&self.canonical_heap))
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
                        | Frame::Native(NativeCont::Xpcall(XCallStage::Handler { .. }))
                )
            })
        };
        let Some(marker_index) = marker_index else {
            // No enclosing `pcall`/`xpcall` catches this - it propagates all
            // the way out through `call_closure`/`resume_coroutine`, whose
            // own `close_frames_above` discards these same frames
            // afterward. Real Lua's traceback for an uncaught error still
            // records every discarded frame's line (the standalone `lua`
            // CLI wraps the whole script in its own top-level protected
            // call with a `msghandler` that calls `luaL_traceback`, exactly
            // like the marker branch below does explicitly), so record each
            // frame's position here too - but without popping or closing any
            // of them, since `drive`'s contract promises `self.frames` is
            // left untouched on this error path and `close_frames_above`
            // still owns discarding/closing them.
            let mut error = error;
            for frame in self.frames[base_depth..].iter().rev() {
                if let Frame::Lua(frame) = frame {
                    if let Some(location) = frame.proto.source_map.location(frame.header.pc) {
                        error = error
                            .at_outer_frame(&self.traceback_frame_label(&frame.proto, location.line));
                    }
                }
            }
            return Err(error);
        };
        let marker_index = base_depth + marker_index;
        let removed = self.frames.len() - marker_index;
        // Close every discarded Lua frame's pending `<close>` values first,
        // innermost (highest index, popped first) to outermost, threading the
        // propagating error through each one. Each discarded frame is also
        // its own call-chain level between the error site and this marker -
        // real Lua's traceback (`lauxlib.c`'s `luaL_traceback`) walks every
        // activation record the same way, so record this frame's own paused
        // line too (its callers above it in `self.frames` get their own
        // entry on a later iteration of this same loop).
        let mut error = error;
        while self.frames.len() > marker_index + 1 {
            if let Frame::Lua(mut frame) = self.frames.pop().expect("len > marker_index + 1") {
                if let Some(location) = frame.proto.source_map.location(frame.header.pc) {
                    error = error.at_outer_frame(&self.traceback_frame_label(&frame.proto, location.line));
                }
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
        self.release_call_depth(removed);
        *depth_charged -= removed;
        if error.double_fault {
            // Real Lua's `LUA_ERRERR` (see `double_fault`'s doc) bypasses
            // every message handler entirely - it never calls `f`'s handler
            // again, it resolves straight to this fixed message at whichever
            // `pcall`/`xpcall` marker is nearest, exactly like the ordinary
            // success/give-up paths below restoring this xpcall's `nCcalls`
            // save point.
            if let NativeCont::Xpcall(
                XCallStage::Function { entry_retry_depth, .. }
                | XCallStage::Handler { entry_retry_depth, .. },
            ) = cont
            {
                self.xcall_retry_depth = entry_retry_depth;
            }
            self.reset_call_depth_overflow();
            return Ok(CallStep::Done(vec![
                LuaValue::Bool(false),
                LuaValue::String(self.intern_str(b"error in error handling")),
            ]));
        }
        match cont {
            NativeCont::Pcall => {
                self.reset_call_depth_overflow();
                Ok(CallStep::Done(vec![
                    LuaValue::Bool(false),
                    error.into_lua_value(&self.canonical_heap),
                ]))
            }
            NativeCont::Xpcall(XCallStage::Function {
                handler,
                entry_retry_depth,
            }) => {
                // Stashed for `debug.traceback` to pick up if `handler` is (or
                // calls) it: real Lua's message handler runs with the erroring
                // stack still in place, but this trampoline has already unwound
                // and discarded those frames by this point (see the loop above),
                // so the frame-name/line trail collected on `error.stack` while
                // unwinding is the only surviving record of it.
                self.pending_error_stack = Some(error.stack.clone());
                let error_value = error.into_lua_value(&self.canonical_heap);
                self.push_native_call(
                    NativeCont::Xpcall(XCallStage::Handler {
                        handler: handler.clone(),
                        entry_retry_depth,
                    }),
                    handler,
                    vec![error_value],
                    base_depth,
                    depth_charged,
                )
            }
            NativeCont::Xpcall(XCallStage::Handler {
                handler,
                entry_retry_depth,
            }) => {
                // The message handler itself raised while running. Real
                // Lua's `luaG_errormsg` re-invokes `L->errfunc` again on
                // this very error, completely unconditionally (confirmed
                // against the pinned oracle: `xpcall(error, err, 300)`
                // retries a self-recursing handler ~300 times and still
                // surfaces the handler's own eventual `"C stack overflow"`
                // return value, not a synthesized double-fault) - it only
                // gives up with the fixed `"error in error handling"`
                // message past a hard cutoff a bit above the ordinary
                // native-call budget (`lstate.c`'s `luaE_checkcstack`,
                // `LUAI_MAXCCALLS/10*11`), to stop a handler that never
                // terminates (e.g. `xpcall(error, error)`, whose handler
                // keeps re-raising `nil` forever) from genuinely recursing
                // without bound.
                if self.xcall_retry_depth >= MAX_NATIVE_CALL_DEPTH * 11 / 10 {
                    self.xcall_retry_depth = entry_retry_depth;
                    self.reset_call_depth_overflow();
                    Ok(CallStep::Done(vec![
                        LuaValue::Bool(false),
                        LuaValue::String(self.intern_str(b"error in error handling")),
                    ]))
                } else {
                    self.pending_error_stack = Some(error.stack.clone());
                    // At (or past) the budget itself, real Lua's own
                    // `luaE_checkcstack` synthesizes exactly this message as
                    // a fresh runtime error and routes it through the same
                    // `luaG_errormsg` handler dispatch as any other error,
                    // rather than ending the retry loop outright - it is
                    // still just another value for `handler` to see (and,
                    // for a handler that recognizes it like this corpus's
                    // `err`, to return unchanged, which is what ends the
                    // loop on the *next* iteration via the ordinary success
                    // path instead of this cutoff).
                    let error_value = if self.xcall_retry_depth >= MAX_NATIVE_CALL_DEPTH {
                        LuaValue::String(self.intern_str(b"C stack overflow"))
                    } else {
                        error.into_lua_value(&self.canonical_heap)
                    };
                    self.push_native_call(
                        NativeCont::Xpcall(XCallStage::Handler {
                            handler: handler.clone(),
                            entry_retry_depth,
                        }),
                        handler,
                        vec![error_value],
                        base_depth,
                        depth_charged,
                    )
                }
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
                Err(_) => match self.metamethod(&value, b"__unm")? {
                    Some(method) => Ok(UnaryResolution::Call {
                        method,
                        // Real Lua's `luaT_trybiniTM` calls unary metamethods
                        // with the operand duplicated as both arguments (see
                        // `lvm.c`'s `luaT_callTMres(L, tm, p1, p1, res)`), not
                        // just once.
                        args: vec![value.clone(), value],
                    }),
                    // Real Lua's `luaV_execute`'s `OP_UNM` falls back to
                    // `luaT_trybiniTM`'s own `luaG_opinterror(L, p1, p1,
                    // "perform arithmetic on")` when there's no `__unm`
                    // metamethod, which reports the operand's type rather
                    // than the generic "number expected" `coerce_number`
                    // raises for any non-number - matching the binary
                    // arithmetic operators' wording (and the analogous
                    // `BitNot`/`__bnot` fix above).
                    None => Err(LuaError::new(format!(
                        "attempt to perform arithmetic on a {} value",
                        self.error_type_label(&value)
                    ))),
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
        // `Option::or` takes its argument by value, so a naive
        // `self.metamethod(left, name)?.or(self.metamethod(right, name)?)`
        // would evaluate the right-hand lookup (metatable fetch + intern +
        // hash lookup) unconditionally, even when the left operand already
        // supplied the metamethod - the common case, since Lua operator
        // overloading typically defines the metamethod on one side.
        if let Some(method) = self.metamethod(left, name)? {
            return Ok(Some(method));
        }
        self.metamethod(right, name)
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
                    return Ok(BinaryResolution::Value(LuaValue::String(self.fresh_str(a))));
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
                        let message = format!("attempt to perform arithmetic on a {label} value");
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

    pub(super) fn expect_table(&self, value: &LuaValue) -> LuaResult<TableRef> {
        match value {
            LuaValue::Table(table) => Ok(*table),
            value => Err(LuaError::new(format!(
                "table expected, got {}",
                value.type_name()
            ))),
        }
    }

    /// Real Lua's `pushglobalfuncname` (`lauxlib.c`), the fallback
    /// `luaL_argerror` uses whenever the erroring call has no nameable Lua
    /// call site - a native function invoked directly by another native's
    /// own Rust code (e.g. `table.sort`'s comparator call, `string.gsub`'s
    /// replacement-function call), rather than reached through a bytecode
    /// `Call` instruction that `describe_register`/`annotate_bad_argument_error`
    /// can name. Searches the globals table itself, then one level into each
    /// of its nested library tables, for the exact callee value, and reports
    /// the qualified `modname.name` when found nested (or the bare name when
    /// found directly at the top level, matching `pushglobalfuncname`'s own
    /// `_G`-stripping behavior) - this is what turns a bad-argument error
    /// raised from inside `table.sort(..., table.sort)`'s own comparator call
    /// into `'table.sort'` instead of the ambiguous bare `'sort'`.
    pub(super) fn global_function_name(&self, value: &LuaValue) -> Option<String> {
        let LuaValue::Table(globals_table) = self.globals.as_value() else {
            return None;
        };
        let entries = self.table_entries(globals_table).ok()?;
        let as_name = |key: &LuaValue| match key {
            LuaValue::String(name) => Some(String::from_utf8_lossy(name.as_bytes()).into_owned()),
            _ => None,
        };
        for (key, entry_value) in &entries {
            if entry_value == value {
                if let Some(name) = as_name(key) {
                    return Some(name);
                }
            }
        }
        for (outer_key, entry_value) in &entries {
            let LuaValue::Table(inner_table) = entry_value else {
                continue;
            };
            let Some(outer_name) = as_name(outer_key) else {
                continue;
            };
            let inner_entries = self.table_entries(*inner_table).ok()?;
            for (inner_key, inner_value) in &inner_entries {
                if inner_value == value {
                    if let Some(inner_name) = as_name(inner_key) {
                        return Some(format!("{outer_name}.{inner_name}"));
                    }
                }
            }
        }
        None
    }

    /// Applied only at callback-invocation sites that have no bytecode call
    /// site of their own (`table.sort`'s comparator, `string.gsub`'s
    /// replacement function): if `error` carries the `"bad argument #N to
    /// '<name>'"` shape a native function's own argument check produces, and
    /// `callee`'s bare name can be resolved to a more specific qualified
    /// global name (see `global_function_name`), rewrite the message to use
    /// it - mirroring real Lua's `pushglobalfuncname` fallback exactly.
    /// A bytecode-issued call never reaches this: its own call-site name
    /// (already bare, and already correct - real Lua's `getobjname` also
    /// reports a bare field name for a direct `table.sort(...)` call) is
    /// handled separately by `annotate_bad_argument_error`.
    pub(super) fn qualify_callback_argument_error(&self, error: &mut LuaError, callee: &LuaValue) {
        let LuaValue::NativeFunction(native) = callee else {
            return;
        };
        let bare = native.name();
        let Some(qualified) = self.global_function_name(callee) else {
            return;
        };
        if qualified == bare {
            return;
        }
        let Some(rest) = error.message.strip_prefix("bad argument #") else {
            return;
        };
        let Some((digits, rest)) = rest.split_once(" to '") else {
            return;
        };
        if digits.parse::<u32>().is_err() {
            return;
        }
        let Some(rest) = rest.strip_prefix(&format!("{bare}' (")) else {
            return;
        };
        error.message = format!("bad argument #{digits} to '{qualified}' ({rest}");
    }

    pub(super) fn raw_index(&self, value: LuaValue, key: LuaValue) -> LuaResult<LuaValue> {
        let table = self.expect_table(&value)?;
        self.table_get(table, &key)
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
        match self.set_index_resolve(value, key, new_value, None)? {
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
        active_frame: Option<&LuaFrame>,
    ) -> LuaResult<()> {
        let table = self.expect_table(&value)?;
        if self.table_get(table, &key)? == LuaValue::Nil {
            self.charge_new_table_entry(active_frame)?;
        }
        self.table_set(table, key, new_value)
    }

    pub(super) fn metamethod(&self, value: &LuaValue, name: &[u8]) -> LuaResult<Option<LuaValue>> {
        if let LuaValue::Userdata(value) = value {
            return c_api::canonical_metamethod(self, value.object_id(), name);
        }
        if let LuaValue::CanonicalTable(value) = value {
            return c_api::canonical_metamethod(self, value.object_id(), name);
        }
        let metatable = match value {
            LuaValue::Table(table) => self.table_metatable(*table),
            LuaValue::String(_) => Some(self.string_metatable),
            LuaValue::Integer(_) | LuaValue::Float(_) => self.number_metatable,
            LuaValue::Bool(_) => self.boolean_metatable,
            LuaValue::Nil => self.nil_metatable,
            _ => None,
        };
        let Some(metatable) = metatable else {
            return Ok(None);
        };
        let value = self.table_get(metatable, &LuaValue::String(self.intern_str(name)))?;
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
                self.table_len(table) as i64,
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
                LuaValue::Table(table) => self.table_get(*table, &key)?,
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
        active_frame: Option<&LuaFrame>,
    ) -> LuaResult<SetIndexResolution> {
        for _ in 0..MAX_METATABLE_CHAIN {
            let raw = match &value {
                LuaValue::Table(table) => self.table_get(*table, &key)?,
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
                    LuaValue::Table(table) => self.table_set(*table, key, new_value)?,
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
                            self.charge_new_table_entry(active_frame)?;
                            self.table_set(*table, key, new_value)?;
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
        match self.table_next(table, &key)? {
            Some((key, value)) => Ok(vec![key, value]),
            None => Ok(vec![LuaValue::Nil]),
        }
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
                    cont: NativeCont::Xpcall(XCallStage::Function {
                        handler,
                        entry_retry_depth: self.xcall_retry_depth,
                    }),
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
                if let Err(error) = self.expect_table(&table_value) {
                    return Err(LuaError::new(format!(
                        "bad argument #1 to 'sort' ({})",
                        error.message
                    )));
                }
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
                    Some(value) => self.fresh_str(self.string(value)?),
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
                let (proto, upvals, globals) = self.closure_parts(*closure)?;
                return Ok(StepResult::PushClosure {
                    closure: *closure,
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
