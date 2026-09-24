//! the debug library.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::frame::*;
use super::util::*;
use super::*;
use crate::lua_bytecode::{Instr, Proto};
use std::cell::Cell;

fn has_implicit_environment(proto: &Proto) -> bool {
    proto.instrs.iter().any(|instr| {
        matches!(
            instr,
            Instr::GetEnvironment(_)
                | Instr::SetEnvironment(_)
                | Instr::GetGlobal(_, _)
                | Instr::SetGlobal(_, _, _, _)
        )
    })
}

impl LuaRuntime {
    pub(super) fn call_native_debug(
        &mut self,
        function: NativeFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let required = |index: usize| {
            args.get(index).cloned().ok_or_else(|| {
                LuaError::new(format!(
                    "bad argument #{} to '{}' (value expected)",
                    index + 1,
                    function.name()
                ))
            })
        };
        match function {
            NativeFunction::DebugSetuservalue => {
                match required(0)? {
                    // `debug.upvalueid` returns a light userdata. Lua's
                    // debug API must reject it distinctly from full
                    // userdata; this is observable in errors.lua and avoids
                    // ever treating an opaque identity as writable storage.
                    LuaValue::LightUserdata(_) => Err(LuaError::new(
                        "bad argument #1 to 'setuservalue' (full userdata expected, got light userdata)",
                    )),
                    LuaValue::Userdata(_) => {
                        // Full canonical userdata storage is owned by the C
                        // API path. Preserve Lua's return convention here;
                        // dynamic uservalue mutation is wired once regular
                        // host userdata expose uservalue slots.
                        Ok(vec![required(0)?])
                    }
                    other => Err(LuaError::new(format!(
                        "bad argument #1 to 'setuservalue' (full userdata expected, got {})",
                        other.type_name()
                    ))),
                }
            }
            NativeFunction::DebugGetupvalue => {
                let closure = match required(0)? {
                    LuaValue::Closure(closure) => closure,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'getupvalue' (Lua function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                let index = coerce_integer(&required(1)?)?;
                let Some(index) = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                else {
                    return Ok(vec![LuaValue::Nil]);
                };
                if index < closure.proto.upvals.len() {
                    let name = closure
                        .proto
                        .upval_names
                        .get(index)
                        .map(String::as_bytes)
                        .unwrap_or(b"?");
                    let value = closure
                        .upvals
                        .borrow()
                        .get(index)
                        .map(|value| value.borrow().clone())
                        .unwrap_or(LuaValue::Nil);
                    return Ok(vec![LuaValue::String(Rc::new(name.to_vec())), value]);
                }
                // The current bytecode keeps its implicit `_ENV` in the
                // shared `Globals` scope rather than in `LuaClosure.upvals`.
                // Expose it at Lua's mandatory final upvalue slot so debug
                // clients can discover the environment even while the
                // compiler migrates to a physical implicit capture.
                if index == closure.proto.upvals.len() && has_implicit_environment(&closure.proto) {
                    Ok(vec![
                        LuaValue::String(Rc::new(b"_ENV".to_vec())),
                        closure.globals.as_value(),
                    ])
                } else {
                    Ok(vec![LuaValue::Nil])
                }
            }
            NativeFunction::DebugUpvalueid => {
                let subject = required(0)?;
                let index = coerce_integer(&required(1)?)?;
                // Real Lua's C closures (e.g. `string.gmatch`'s returned
                // iterator) carry their own upvalues too, distinct from Lua
                // closures' `UpVal` cells - Sol's `GMatchIterator` is the
                // one native-with-state callable this corpus exercises
                // through `debug.upvalueid`, so it gets a single opaque
                // "upvalue 1" identity from its own shared state cell;
                // any other index is out of range like a real C closure
                // with one upvalue would report.
                let identity = match &subject {
                    LuaValue::Closure(closure) => {
                        let upvals = closure.upvals.borrow();
                        usize::try_from(index)
                            .ok()
                            .and_then(|index| index.checked_sub(1))
                            .and_then(|index| upvals.get(index))
                            .map(|cell| Rc::as_ptr(cell) as usize)
                            // The legacy bytecode frame still stores its
                            // implicit environment beside lexical upvalues.
                            // Expose the final mandatory `_ENV` identity at
                            // the same logical slot as getupvalue/setupvalue.
                            .or_else(|| {
                                (usize::try_from(index).ok()
                                    == Some(closure.proto.upvals.len() + 1)
                                    && has_implicit_environment(&closure.proto))
                                .then(|| closure.globals.identity_address())
                            })
                    }
                    LuaValue::GMatchIterator(state) if index == 1 => {
                        Some(Rc::as_ptr(state) as usize)
                    }
                    LuaValue::GMatchIterator(_) => None,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'upvalueid' (Lua function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                Ok(vec![match identity {
                    Some(identity) => LuaValue::LightUserdata(identity),
                    None => LuaValue::Nil,
                }])
            }
            NativeFunction::DebugUpvaluejoin => {
                let closure_arg = |arg_index: usize, param_index: usize| {
                    match required(arg_index)? {
                    LuaValue::Closure(closure) => Ok(closure),
                    other => Err(LuaError::new(format!(
                        "bad argument #{param_index} to 'upvaluejoin' (Lua function expected, got {})",
                        other.type_name()
                    ))),
                }
                };
                let f1 = closure_arg(0, 1)?;
                let n1 = coerce_integer(&required(1)?)?;
                let f2 = closure_arg(2, 3)?;
                let n2 = coerce_integer(&required(3)?)?;
                let source_cell = usize::try_from(n2)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| f2.upvals.borrow().get(index).cloned())
                    .ok_or_else(|| {
                        LuaError::new("bad argument #4 to 'upvaluejoin' (invalid upvalue index)")
                    })?;
                let dst_index = usize::try_from(n1)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .filter(|&index| index < f1.upvals.borrow().len())
                    .ok_or_else(|| {
                        LuaError::new("bad argument #2 to 'upvaluejoin' (invalid upvalue index)")
                    })?;
                f1.upvals.borrow_mut()[dst_index] = source_cell;
                Ok(vec![])
            }
            NativeFunction::DebugSetupvalue => {
                let closure = match required(0)? {
                    LuaValue::Closure(closure) => closure,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'setupvalue' (Lua function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                let index = coerce_integer(&required(1)?)?;
                let index = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_sub(1));
                let value = required(2)?;
                let Some(index) = index else {
                    return Ok(vec![LuaValue::Nil]);
                };
                let upvals = closure.upvals.borrow_mut();
                if let Some(cell) = upvals.get(index) {
                    *cell.borrow_mut() = value;
                    let name = closure
                        .proto
                        .upval_names
                        .get(index)
                        .map(String::as_bytes)
                        .unwrap_or(b"?");
                    Ok(vec![LuaValue::String(Rc::new(name.to_vec()))])
                } else if index == closure.proto.upvals.len()
                    && has_implicit_environment(&closure.proto)
                {
                    // See `DebugGetupvalue`: `_ENV` is represented by the
                    // shared scope cell during the transition. It is already
                    // the caller's default environment in the only binary
                    // chunk shape this compatibility path exposes.
                    Ok(vec![LuaValue::String(Rc::new(b"_ENV".to_vec()))])
                } else {
                    Ok(vec![LuaValue::Nil])
                }
            }
            NativeFunction::DebugGetinfo => {
                // `debug.getinfo([thread,] f [, what])`: `f` is either a
                // numeric stack level or a function value (real Lua's
                // `ldblib.c`'s `db_getinfo`/`ldebug.c`'s `auxgetinfo`). The
                // option-string second argument is accepted but not
                // otherwise interpreted - every field below is always
                // populated when available, regardless of what letters were
                // requested. Level 1 is the function that called `getinfo`,
                // i.e. the topmost entry already on `self.frames` (the
                // caller was pushed back before this native call was
                // dispatched, same as every other leaf call - see
                // `dispatch.rs`'s `StepResult::CallLeaf` handling). Stack
                // levels count *every* frame, including `Frame::Native`
                // (`pcall`, `xpcall`, a coroutine's own base marker, etc.) -
                // real Lua's `lua_getstack` walks the full `CallInfo` chain,
                // C activation records included, not just Lua ones.
                //
                // `name`/`namewhat` for a level-based lookup prefer real
                // Lua's `funcnamefromcode`-equivalent (`LuaRuntime::
                // call_site_name`, `natives_core.rs`): it walks the
                // *caller's* bytecode at the call site to figure out how
                // the callee was referenced (global/local/upvalue/field/
                // method), reusing the same `describe_register` real
                // Lua-equivalent `getobjname` scan that annotates "attempt
                // to call/index" error messages (`dispatch.rs`). When that
                // has no answer - the outermost frame, a native caller, or
                // a tail call (which reuses the caller's own frame instead
                // of leaving one behind to resolve from) - fall back to the
                // callee's own declared local-function name, and finally to
                // Lua's own `nil`/`""` when neither resolves anything,
                // matching real Lua's own fallback whenever it can't
                // determine a name. A direct function-value lookup (the
                // `LuaValue::Closure` arm above) has no caller frame at all,
                // so it always uses Lua's fallback.
                let arg0 = required(0)?;
                if let Some(option) = args.get(1) {
                    let option = self.string(option).map_err(|_| {
                        LuaError::new("bad argument #2 to 'getinfo' (string expected)")
                    })?;
                    // Lua's `lua_getinfo` accepts only this option alphabet.
                    // We currently populate a conservative subset of the
                    // requested fields, but must still reject an invalid
                    // letter rather than silently accepting it.
                    if option.iter().any(|byte| {
                        !matches!(byte, b'n' | b'S' | b'l' | b't' | b'u' | b'f' | b'L' | b'r')
                    }) {
                        return Err(LuaError::new(
                            "bad argument #2 to 'getinfo' (invalid option)",
                        ));
                    }
                }
                let info = Rc::new(RefCell::new(LuaTable::default()));
                let mut set = |key: &[u8], value: LuaValue| {
                    info.borrow_mut()
                        .set(LuaValue::String(Rc::new(key.to_vec())), value)
                        .unwrap();
                };
                match &arg0 {
                    LuaValue::Closure(closure) => {
                        // A function value has no active call frame, so real
                        // Lua reports `currentline = -1` and no `__call`-chain
                        // hop count (`extraargs = 0`) here.
                        self.describe_lua_proto(&mut set, &closure.proto, -1, 0);
                        set(
                            b"nups",
                            LuaValue::Integer(closure.upvals.borrow().len() as i64),
                        );
                        // Only this direct-value lookup has the real closure
                        // `Rc` in hand. The level-based lookup below unpacks a
                        // `LuaFrame`'s `proto`/`upvals`/`globals` rather than
                        // keeping the original closure, and reconstructing one
                        // would fail `Rc::ptr_eq`-based `LuaValue` equality
                        // against the actual running closure - worse than
                        // leaving `func` unset there.
                        set(b"func", LuaValue::Closure(closure.clone()));
                    }
                    LuaValue::NativeFunction(_)
                    | LuaValue::Native(_)
                    | LuaValue::RegisteredNative(_) => {
                        Self::describe_native(&mut set);
                    }
                    _ => {
                        let level = coerce_integer(&arg0).map_err(|_| {
                            LuaError::new(
                                "bad argument #1 to 'getinfo' (function or level expected)",
                            )
                        })?;
                        if level < 0 {
                            return Ok(vec![LuaValue::Nil]);
                        }
                        if level == 0 {
                            return Err(LuaError::new(
                                "bad argument #1 to 'getinfo' (level 0 is unavailable)",
                            ));
                        }
                        let mut remaining = level;
                        let mut found = None;
                        for frame in self.frames.iter().rev() {
                            remaining -= 1;
                            if remaining == 0 {
                                found = Some(frame);
                                break;
                            }
                        }
                        let Some(frame) = found else {
                            return Ok(vec![LuaValue::Nil]);
                        };
                        match frame {
                            Frame::Lua(lua_frame) => {
                                let current_line = lua_frame
                                    .proto
                                    .source_map
                                    .location(lua_frame.header.pc)
                                    .map_or(-1, |location| location.line as i64);
                                // `extraargs`: despite its name, real Lua
                                // 5.5's `lua_Debug.extraargs` (`ldebug.c`'s
                                // `auxgetinfo`, `'t'` case) is not a vararg
                                // count - it is `(ci->callstatus &
                                // MAX_CCMT) >> CIST_CCMT`, the number of
                                // `__call` metamethod hops
                                // `luaD_precall`'s retry loop walked to
                                // reach this frame's closure. That is
                                // exactly `lua_frame.call_chain_hops`,
                                // tracked by `step_result_for_call`'s own
                                // `__call`-chain loop (see
                                // `MAX_CALL_CHAIN`). Verified against the
                                // pinned `lua5.5.1` oracle: a direct call
                                // (no `__call` involved) always reports 0
                                // here regardless of how many varargs the
                                // function actually received.
                                self.describe_lua_proto(
                                    &mut set,
                                    &lua_frame.proto,
                                    current_line,
                                    lua_frame.call_chain_hops as i64,
                                );
                                if let Some((namewhat, name)) = self.call_site_name(level) {
                                    set(
                                        b"namewhat",
                                        LuaValue::String(Rc::new(namewhat.as_bytes().to_vec())),
                                    );
                                    set(b"name", LuaValue::String(Rc::new(name.into_bytes())));
                                } else if let Some(name) = Self::declared_lua_name(&lua_frame.proto)
                                {
                                    set(b"namewhat", LuaValue::String(Rc::new(b"local".to_vec())));
                                    set(b"name", LuaValue::String(Rc::new(name.to_vec())));
                                }
                                set(b"nups", LuaValue::Integer(lua_frame.upvals.len() as i64));
                            }
                            Frame::Native(_) => Self::describe_native(&mut set),
                        }
                    }
                }
                Ok(vec![LuaValue::Table(info)])
            }
            NativeFunction::DebugGetmetatable => {
                // Unlike the ordinary (non-debug) `getmetatable`, this always
                // returns the real metatable - there is no `__metatable`
                // protection to honor here - and also covers numbers,
                // booleans, and nil, which have no per-value metatable slot
                // of their own but do share one
                // `number_metatable`/`boolean_metatable`/`nil_metatable`
                // each, like real Lua's single
                // `LUA_TNUMBER`/`LUA_TBOOLEAN`/`LUA_TNIL` basic-type
                // metatables. Note `required(0)` only errors when the
                // argument is entirely absent, not when it's an explicit
                // `nil` - `debug.getmetatable(nil)` and
                // `debug.setmetatable(nil, mt)` are both valid calls.
                Ok(vec![match required(0)? {
                    LuaValue::Table(table) => table
                        .borrow()
                        .metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::String(_) => LuaValue::Table(self.string_metatable.clone()),
                    LuaValue::Integer(_) | LuaValue::Float(_) => self
                        .number_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::Bool(_) => self
                        .boolean_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::Nil => self
                        .nil_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    _ => LuaValue::Nil,
                }])
            }
            NativeFunction::DebugSetmetatable => {
                let value = required(0)?;
                let new_metatable = match args.get(1).cloned().unwrap_or(LuaValue::Nil) {
                    LuaValue::Nil => None,
                    LuaValue::Table(meta) => Some(meta),
                    _ => {
                        return Err(LuaError::new(
                            "bad argument #2 to 'setmetatable' (nil or table expected)",
                        ));
                    }
                };
                match &value {
                    LuaValue::Table(table) => {
                        table.borrow_mut().metatable = new_metatable;
                    }
                    LuaValue::Integer(_) | LuaValue::Float(_) => {
                        self.number_metatable = new_metatable;
                    }
                    LuaValue::Bool(_) => {
                        self.boolean_metatable = new_metatable;
                    }
                    LuaValue::Nil => {
                        self.nil_metatable = new_metatable;
                    }
                    _ => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'setmetatable' ({} values are not supported by this debug library)",
                            value.type_name()
                        )));
                    }
                }
                Ok(vec![value])
            }
            NativeFunction::DebugTraceback => {
                // Minimal `debug.traceback([message])`: a non-string,
                // non-nil message is returned unchanged (matching real
                // Lua), otherwise the message (if any) is prefixed onto a
                // "stack traceback:" block. The frame trail comes from
                // `pending_error_stack` - stashed by `unwind_error_to_marker`
                // right before calling an `xpcall` message handler, which is
                // the only way this runtime still has that trail available
                // (the erroring frames themselves are already unwound and
                // gone from `self.frames` by the time a handler runs). A
                // direct, non-handler call (no pending error) just reports
                // an empty traceback, same as real Lua reports an empty
                // trace below the level it was asked to start from.
                let message = args.first().cloned().unwrap_or(LuaValue::Nil);
                let prefix = match &message {
                    LuaValue::Nil => None,
                    LuaValue::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
                    other => return Ok(vec![other.clone()]),
                };
                let trail = self.pending_error_stack.take().unwrap_or_default();
                let mut out = String::new();
                if let Some(prefix) = prefix {
                    out.push_str(&prefix);
                    out.push('\n');
                }
                out.push_str("stack traceback:");
                for entry in trail.iter().rev() {
                    out.push_str("\n\t");
                    out.push_str(entry);
                }
                Ok(vec![LuaValue::String(Rc::new(out.into_bytes()))])
            }
            NativeFunction::DebugSethook => {
                // `debug.sethook([thread,] hook, mask [, count])` -
                // `thread` defaults to the currently running coroutine/main
                // (matching `coroutine.isyieldable`/`coroutine.close`'s own
                // default-argument pattern above). Omitting `hook` (or
                // passing `nil`) clears that thread's hook, matching real
                // Lua's `debug.sethook()`/`debug.sethook(co)`.
                let mut index = 0;
                let target = if matches!(args.first(), Some(LuaValue::Thread(_))) {
                    index = 1;
                    Self::expect_coroutine(&args[0])?
                } else {
                    self.coroutine_stack
                        .last()
                        .cloned()
                        .unwrap_or_else(|| self.main_coroutine.clone())
                };
                let hook = args.get(index).cloned().unwrap_or(LuaValue::Nil);
                if matches!(hook, LuaValue::Nil) {
                    *target.hook.borrow_mut() = None;
                } else {
                    let mask_value = args.get(index + 1).cloned().ok_or_else(|| {
                        LuaError::new(
                            "bad argument #2 to 'sethook' (string expected, got no value)",
                        )
                    })?;
                    let mask_bytes = self.string(&mask_value)?;
                    let mut mask = HookMask::default();
                    for &byte in mask_bytes {
                        match byte {
                            b'c' => mask.call = true,
                            b'l' => mask.line = true,
                            b'r' => mask.ret = true,
                            _ => {}
                        }
                    }
                    let count = match args.get(index + 2) {
                        Some(value) => coerce_integer(value)?,
                        None => 0,
                    };
                    mask.count = count > 0;
                    *target.hook.borrow_mut() = Some(Rc::new(HookState {
                        callback: hook,
                        mask,
                        count,
                        count_remaining: Cell::new(count),
                    }));
                }
                // Refresh the cached fast-path copy immediately if `target`
                // is whichever coroutine/main is currently executing this
                // very call - see `active_hook`'s field doc on `LuaRuntime`.
                let running = self
                    .coroutine_stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| self.main_coroutine.clone());
                if Rc::ptr_eq(&target, &running) {
                    self.active_hook = target.hook.borrow().clone();
                    // `fire_line_and_count_hooks` only updates
                    // `hook_last_pc`/`hook_last_line` while a hook is
                    // actually active (real Lua's own `oldpc` tracking in
                    // `luaG_traceexec` runs unconditionally, hook or not, so
                    // by the time a hook is installed it already reflects
                    // "the line we're currently on"). Sol's per-frame
                    // tracking instead sits stale (frozen at its `-1`
                    // sentinel, or wherever it was when a previous hook was
                    // cleared) the whole time no hook is active. Left
                    // uncorrected, activating a fresh "l"-mode hook mid-frame
                    // would compare the frame's *current* line against that
                    // stale value on the very next instruction and misfire a
                    // spurious "line" event for a line the frame was already
                    // on, not a real transition - every live frame on this
                    // coroutine's stack needs its bookkeeping seeded to its
                    // own current position first.
                    if matches!(&self.active_hook, Some(hook) if hook.mask.line) {
                        for frame in self.frames.iter_mut() {
                            if let Frame::Lua(lua_frame) = frame {
                                let pc = lua_frame.header.pc as i64;
                                let line = lua_frame
                                    .proto
                                    .source_map
                                    .location(lua_frame.header.pc)
                                    .map(|location| location.line as i64)
                                    .unwrap_or(-1);
                                lua_frame.hook_last_pc = pc;
                                lua_frame.hook_last_line = line;
                            }
                        }
                    }
                }
                Ok(vec![])
            }
            NativeFunction::DebugGethook => {
                let target = if matches!(args.first(), Some(LuaValue::Thread(_))) {
                    Self::expect_coroutine(&args[0])?
                } else {
                    self.coroutine_stack
                        .last()
                        .cloned()
                        .unwrap_or_else(|| self.main_coroutine.clone())
                };
                let hook_state = target.hook.borrow().clone();
                match hook_state {
                    Some(hook) => {
                        let mut mask_string = String::new();
                        if hook.mask.call {
                            mask_string.push('c');
                        }
                        if hook.mask.line {
                            mask_string.push('l');
                        }
                        if hook.mask.ret {
                            mask_string.push('r');
                        }
                        Ok(vec![
                            hook.callback.clone(),
                            LuaValue::String(Rc::new(mask_string.into_bytes())),
                            LuaValue::Integer(hook.count),
                        ])
                    }
                    None => Ok(vec![LuaValue::Nil]),
                }
            }
            _ => unreachable!("call_native_debug received a non-debug NativeFunction"),
        }
    }

    /// Populates the static/definition-time fields `debug.getinfo` reports
    /// for a Lua function, whether looked up by value or by stack level.
    /// `current_line`/`extraargs` are execution-specific (a bare function
    /// value with no active frame passes `-1`/`0`; see the call site).
    fn describe_lua_proto(
        &self,
        set: &mut impl FnMut(&[u8], LuaValue),
        proto: &Rc<Proto>,
        current_line: i64,
        extraargs: i64,
    ) {
        set(b"currentline", LuaValue::Integer(current_line));
        set(b"extraargs", LuaValue::Integer(extraargs));
        let linedefined = proto.line_defined as i64;
        let lastlinedefined = proto.last_line_defined as i64;
        // `activelines`: real Lua's `funcinfo` builds this as a set (line ->
        // `true`) of every line this prototype's own bytecode maps to - not
        // its nested closures', which carry separate `Proto`s/source maps.
        let activelines = Rc::new(RefCell::new(LuaTable::default()));
        for pc in 0..proto.source_map.len() as u32 {
            if let Some(location) = proto.source_map.location(pc) {
                activelines
                    .borrow_mut()
                    .set(
                        LuaValue::Integer(location.line as i64),
                        LuaValue::Bool(true),
                    )
                    .unwrap();
            }
        }
        set(b"activelines", LuaValue::Table(activelines));
        set(b"linedefined", LuaValue::Integer(linedefined));
        set(b"lastlinedefined", LuaValue::Integer(lastlinedefined));
        set(b"isvararg", LuaValue::Bool(proto.metadata.arity.variadic));
        set(
            b"nparams",
            LuaValue::Integer(proto.metadata.arity.parameters as i64),
        );
        set(b"istailcall", LuaValue::Bool(false));
        // The top-level chunk's own function is compiled under the fixed
        // name `"main"` (`natives_load.rs`'s `compile_chunk_named`) -
        // exactly real Lua's own "main"/"Lua" distinction for `what`.
        let what: &[u8] = if proto.metadata.name == "main" {
            b"main"
        } else {
            b"Lua"
        };
        set(b"what", LuaValue::String(Rc::new(what.to_vec())));
        set(b"namewhat", LuaValue::String(Rc::new(Vec::new())));
        set(b"name", LuaValue::Nil);
        if let Some(source) = self.chunk_sources.get(&(Rc::as_ptr(proto) as usize)) {
            set(b"source", LuaValue::String(source.clone()));
            set(
                b"short_src",
                LuaValue::String(Rc::new(Self::short_src(source))),
            );
        }
    }

    /// The parser preserves the declaration name for `local function f()`
    /// and `function f()`.  Anonymous expressions and the main chunk use
    /// synthetic names and must retain `debug.getinfo`'s nil-name fallback.
    ///
    /// This is deliberately only used for active stack frames: asking for
    /// information about a bare function value has no call site, so Lua
    /// reports no name even if the function was declared with one.
    fn declared_lua_name(proto: &Proto) -> Option<&[u8]> {
        let name = proto.metadata.name.as_bytes();
        (!name.is_empty() && name != b"main" && !name.starts_with(b"<anonymous@")).then_some(name)
    }

    /// The fields `debug.getinfo` reports for a C/native activation record
    /// (a `NativeFunction`/`Frame::Native` entry) - real Lua's `auxgetinfo`
    /// `"C"` case: no line info, `isvararg` unconditionally true (a C
    /// function has no fixed Lua parameter list), and the fixed synthetic
    /// `"=[C]"`/`"[C]"` source real Lua uses for every C function.
    fn describe_native(set: &mut impl FnMut(&[u8], LuaValue)) {
        set(b"currentline", LuaValue::Integer(-1));
        set(b"extraargs", LuaValue::Integer(0));
        set(b"linedefined", LuaValue::Integer(-1));
        set(b"lastlinedefined", LuaValue::Integer(-1));
        set(b"isvararg", LuaValue::Bool(true));
        set(b"nparams", LuaValue::Integer(0));
        set(b"nups", LuaValue::Integer(0));
        set(b"istailcall", LuaValue::Bool(false));
        set(b"what", LuaValue::String(Rc::new(b"C".to_vec())));
        set(b"namewhat", LuaValue::String(Rc::new(Vec::new())));
        set(b"name", LuaValue::Nil);
        set(b"source", LuaValue::String(Rc::new(b"=[C]".to_vec())));
        set(b"short_src", LuaValue::String(Rc::new(b"[C]".to_vec())));
    }

    /// `luaO_chunkid`: `=name` sources report `name` verbatim (truncated to
    /// `LUA_IDSIZE - 1` bytes, one held back for the C string's `'\0'`),
    /// `@path` sources report `path` (truncated the same way, but keeping the
    /// tail behind a `...` prefix so a long path's distinguishing suffix
    /// stays visible), and anything else (a literal chunk of source text,
    /// e.g. from `load`) reports `[string "line one..."]`, truncated to
    /// whatever fits alongside that fixed decoration. `load`'s own
    /// syntax-error prefix (`natives_load.rs`'s `format_chunk_diagnostic`)
    /// routes through this same function rather than a separate formatter,
    /// matching real Lua's single `luaO_chunkid` used by both
    /// `debug.getinfo` and `lua_load`'s error path.
    pub(super) fn short_src(source: &[u8]) -> Vec<u8> {
        const IDSIZE: usize = 60;
        const MAXLEN: usize = IDSIZE - 1;
        if let Some(rest) = source.strip_prefix(b"=") {
            let mut short = rest.to_vec();
            short.truncate(MAXLEN);
            short
        } else if let Some(rest) = source.strip_prefix(b"@") {
            if rest.len() <= MAXLEN {
                rest.to_vec()
            } else {
                let mut short = b"...".to_vec();
                short.extend_from_slice(&rest[rest.len() - (MAXLEN - 3)..]);
                short
            }
        } else {
            const PRE: &[u8] = b"[string \"";
            const POS: &[u8] = b"\"]";
            const RETS: &[u8] = b"...";
            // Real Lua reserves space for PRE/RETS/POS and a trailing '\0'
            // up front, then only appends RETS when the content itself
            // doesn't fit within what's left - so the untruncated case gets
            // one more byte of headroom than the truncated one.
            let content_budget = IDSIZE - PRE.len() - RETS.len() - POS.len() - 1;
            let first_line_end = source.iter().position(|&b| b == b'\n');
            let has_newline = first_line_end.is_some();
            let line = &source[..first_line_end.unwrap_or(source.len())];
            let mut short = PRE.to_vec();
            if line.len() < content_budget && !has_newline {
                short.extend_from_slice(line);
            } else {
                let take = line.len().min(content_budget);
                short.extend_from_slice(&line[..take]);
                short.extend_from_slice(RETS);
            }
            short.extend_from_slice(POS);
            short
        }
    }
}
