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

fn visible_upvalue_count(proto: &Proto) -> i64 {
    proto.upvals.len() as i64 + i64::from(has_implicit_environment(proto))
}

/// Resolves Lua's one-based local index at an instruction boundary.  The
/// compiler can recycle registers after a scope exits, so this must consult
/// `Proto::locals`' lexical PC ranges rather than treating a raw register
/// number as a local-variable index.
fn active_local(proto: &Proto, pc: u32, index: i64) -> Option<(usize, &str)> {
    let index = usize::try_from(index).ok()?.checked_sub(1)?;
    proto
        .locals
        .iter()
        .filter(|local| local.start_pc <= pc && pc < local.end_pc)
        .nth(index)
        .map(|local| (local.register as usize, local.name.as_str()))
}

/// Lua exposes a synthetic positive local named `"(vararg table)"` after a
/// vararg function's fixed parameters.  It is distinct from the negative
/// `"(vararg)"` entries used to access each individual argument.  The
/// compiler does not allocate a physical register for that legacy debug
/// view, so `usize::MAX` is an internal sentinel whose value is always nil.
fn active_stack_local(proto: &Proto, pc: u32, index: i64) -> Option<(usize, &str)> {
    // A stripped prototype has no lexical-local records, but its register
    // values remain observable.  Lua reports those slots as unnamed
    // temporaries rather than hiding them altogether.
    if proto.locals.is_empty() && index > 0 {
        return usize::try_from(index - 1)
            .ok()
            .filter(|register| *register < proto.metadata.registers as usize)
            .map(|register| (register, "(temporary)"));
    }
    let parameter_count = proto.metadata.arity.parameters as i64;
    // A chunk is internally variadic so its top-level `...` machinery can
    // share the function compiler, but Lua does not expose that implementation
    // detail as a `(vararg table)` local through the debug API.
    if proto.line_defined != 0 && proto.metadata.arity.variadic && index == parameter_count + 1 {
        return Some((usize::MAX, "(vararg table)"));
    }
    let adjusted = if proto.line_defined != 0
        && proto.metadata.arity.variadic
        && index > parameter_count + 1
    {
        index - 1
    } else {
        index
    };
    active_local(proto, pc, adjusted)
}

fn active_frame_local(frame: &LuaFrame, index: i64) -> Option<(usize, &str)> {
    // A closure definition is observable one instruction before its local is
    // installed. Lua presents that pending destination as an unnamed
    // temporary, which is what lets a line hook distinguish `local A =
    // function ... end`'s closing `end` from the following local binding.
    // Check it before lexical-local metadata because the compiler records the
    // binding's PC range while emitting the same instruction stream.
    if matches!(frame.proto.instrs.get(frame.header.pc as usize), Some(Instr::NewClosure(_, _)))
        && index == 1
    {
        if let Some(Instr::NewClosure(register, _)) =
            frame.proto.instrs.get(frame.header.pc as usize)
        {
            return Some((*register as usize, "(temporary)"));
        }
    }
    if let Some(local) = active_stack_local(&frame.proto, frame.header.pc, index) {
        return Some(local);
    }
    // A suspended call keeps expression results below its callee register.
    // Those are Lua's unnamed temporaries. The callee slot itself and its
    // argument slots are call setup, not the caller's available locals.
    let named = frame
        .proto
        .locals
        .iter()
        .filter(|local| local.start_pc <= frame.header.pc && frame.header.pc < local.end_pc)
        .count()
        + usize::from(frame.proto.metadata.arity.variadic);
    let temporary = usize::try_from(index).ok()?.checked_sub(named + 1)?;
    let Pending::Call { base, .. } = frame.pending else {
        return None;
    };
    let first_register = frame
        .proto
        .locals
        .iter()
        .filter(|local| local.start_pc <= frame.header.pc && frame.header.pc < local.end_pc)
        .map(|local| local.register as usize + 1)
        .max()
        .unwrap_or(0);
    (first_register..base)
        .filter(|&register| {
            let last_write = frame.proto.instrs[..frame.header.pc as usize]
                .iter()
                .rposition(|instr| super::dispatch::instr_writes(instr, register as u16));
            let Some(last_write) = last_write else {
                // Reserved but never populated slots are not Lua stack
                // temporaries. Sol's register allocator can leave gaps
                // before a live expression result.
                return false;
            };
            !matches!(
                frame.proto.instrs.get(last_write),
                Some(Instr::LoadNil(reg))
                    if *reg as usize == register
                        && matches!(
                            frame.proto.instrs.get(last_write.wrapping_sub(1)),
                            Some(Instr::DetachCell(detached)) if *detached as usize == register
                        )
            )
        })
        .nth(temporary)
        .map(|register| (register, "(temporary)"))
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
            NativeFunction::DebugGetregistry => {
                Ok(vec![LuaValue::Table(TableRef::new(self.c_registry))])
            }
            NativeFunction::DebugGetuservalue => {
                let userdata = match required(0)? {
                    LuaValue::Userdata(userdata) => userdata,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'getuservalue' (full userdata expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                let index = args.get(1).map(coerce_integer).transpose()?.unwrap_or(1);
                let Some(index) = usize::try_from(index).ok().and_then(|i| i.checked_sub(1))
                else {
                    return Ok(vec![LuaValue::Nil]);
                };
                let value = {
                    let heap = self.canonical_heap.borrow();
                    let object = heap.userdata(userdata.object_id()).map_err(|error| {
                        LuaError::new(format!("internal error: {error}"))
                    })?;
                    object.user_values.get(index).copied()
                };
                match value {
                    Some(value) => Ok(vec![self.decode_value(value)?, LuaValue::Bool(true)]),
                    None => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::DebugSetuservalue => {
                match required(0)? {
                    // `debug.upvalueid` returns a light userdata. Lua's debug
                    // API must reject it distinctly from full userdata; this is
                    // observable in errors.lua and avoids ever treating an
                    // opaque identity as writable storage.
                    LuaValue::LightUserdata(_) => Err(LuaError::new(
                        "bad argument #1 to 'setuservalue' (full userdata expected, got light userdata)",
                    )),
                    LuaValue::Userdata(userdata) => {
                        let value = required(1)?;
                        let index = args.get(2).map(coerce_integer).transpose()?.unwrap_or(1);
                        let Some(index) = usize::try_from(index).ok().and_then(|i| i.checked_sub(1))
                        else {
                            return Ok(vec![LuaValue::Nil]);
                        };
                        let value = self.encode_value(&value)?;
                        let present = self.canonical_heap.borrow_mut()
                            .set_userdata_user_value(userdata.object_id(), index, value)
                            .map_err(|error| LuaError::new(format!("internal error: {error}")))?;
                        Ok(vec![if present { LuaValue::Userdata(userdata) } else { LuaValue::Nil }])
                    }
                    other => Err(LuaError::new(format!(
                        "bad argument #1 to 'setuservalue' (full userdata expected, got {})",
                        other.type_name()
                    ))),
                }
            }
            NativeFunction::DebugGetupvalue => {
                let subject = required(0)?;
                let index = coerce_integer(&required(1)?)?;
                if let LuaValue::GMatchIterator(state) = &subject {
                    let (source, pattern, _, _) = self.gmatch_read(*state)?;
                    let value = match index {
                        1 => Some(LuaValue::String(self.intern_str(source))),
                        2 => Some(LuaValue::String(self.intern_str(pattern))),
                        3 => {
                            let id = {
                                let heap = self.canonical_heap.borrow();
                                match heap.object(state.object_id()) {
                                    Ok(sol_core::HeapObject::NativeCallable(callable)) =>
                                        callable.captures.get(4).and_then(|value| value.as_object()),
                                    _ => None,
                                }
                            };
                            id.map(|id| LuaValue::Userdata(CanonicalUserdata::root_existing(
                                self.canonical_heap.clone(), id,
                            )))
                        }
                        _ => None,
                    };
                    return Ok(value.map(|value| vec![
                        LuaValue::String(self.intern_str(b"")), value,
                    ]).unwrap_or_else(|| vec![LuaValue::Nil]));
                }
                let closure = match subject {
                    LuaValue::Closure(closure) => closure,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'getupvalue' (Lua function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                let Some(index) = usize::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                else {
                    return Ok(vec![LuaValue::Nil]);
                };
                let (proto, upvalues, globals) = self.closure_parts(closure)?;
                if index < proto.upvals.len() {
                    let name = proto
                        .upval_names
                        .get(index)
                        .map(String::as_bytes)
                        .unwrap_or(b"(no name)");
                    let value = match upvalues.get(index) {
                        Some(&cell) => self.upvalue_get(cell)?,
                        None => LuaValue::Nil,
                    };
                    return Ok(vec![LuaValue::String(self.intern_str(name)), value]);
                }
                // The current bytecode keeps its implicit `_ENV` in the shared
                // `Globals` scope rather than in `LuaClosure.upvals`. Expose it
                // at Lua's mandatory final upvalue slot so debug clients can
                // discover the environment even while the compiler migrates to
                // a physical implicit capture.
                if index == proto.upvals.len() && has_implicit_environment(&proto) {
                    Ok(vec![
                        LuaValue::String(self.intern_str(b"_ENV")),
                        globals.as_value(),
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
                // closures' `UpVal` cells - Sol's `GMatchIterator` is the one
                // native-with-state callable this corpus exercises through
                // `debug.upvalueid`, so it gets a single opaque "upvalue 1"
                // identity from its own shared state cell; any other index is
                // out of range like a real C closure with one upvalue would report.
                let identity = match &subject {
                    LuaValue::Closure(closure) => {
                        let (proto, upvalues, globals) = self.closure_parts(*closure)?;
                        usize::try_from(index)
                            .ok()
                            .and_then(|index| index.checked_sub(1))
                            // `upvalues` also carries the synthetic trailing
                            // environment cell `new_closure` appends (see its
                            // own doc comment) - excluded here so an
                            // out-of-range Lua index never resolves to it
                            // except through the explicit `_ENV` check below.
                            .filter(|&index| index < proto.upvals.len())
                            .and_then(|index| upvalues.get(index))
                            .map(|cell| cell.raw() as usize)
                            // The legacy bytecode frame still stores its
                            // implicit environment beside lexical upvalues.
                            // Expose the final mandatory `_ENV` identity at
                            // the same logical slot as getupvalue/setupvalue.
                            .or_else(|| {
                                (usize::try_from(index).ok() == Some(proto.upvals.len() + 1)
                                    && has_implicit_environment(&proto))
                                .then(|| globals.identity_address())
                            })
                    }
                    LuaValue::GMatchIterator(state) => {
                        let capture = match index {
                            1 => Some(0),
                            2 => Some(1),
                            3 => Some(4),
                            _ => None,
                        };
                        let heap = self.canonical_heap.borrow();
                        match heap.object(state.object_id()) {
                            Ok(sol_core::HeapObject::NativeCallable(callable)) => capture
                                .and_then(|capture| callable.captures.get(capture))
                                .and_then(|value| value.as_object())
                                .map(|id| id.raw() as usize),
                            _ => None,
                        }
                    }
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
                // Bounded by `proto.upvals.len()`, not the raw upvalue-cell
                // count: see `DebugUpvalueid`'s own comment - `upvalues` also
                // carries `new_closure`'s synthetic trailing environment
                // cell, which a Lua-visible upvalue index must never reach.
                let (f2_proto, f2_upvalues, _) = self.closure_parts(f2)?;
                let source_cell = usize::try_from(n2)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .filter(|&index| index < f2_proto.upvals.len())
                    .and_then(|index| f2_upvalues.get(index).copied())
                    .ok_or_else(|| {
                        LuaError::new("bad argument #4 to 'upvaluejoin' (invalid upvalue index)")
                    })?;
                let (f1_proto, _f1_upvalues, _) = self.closure_parts(f1)?;
                let dst_index = usize::try_from(n1)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .filter(|&index| index < f1_proto.upvals.len())
                    .ok_or_else(|| {
                        LuaError::new("bad argument #2 to 'upvaluejoin' (invalid upvalue index)")
                    })?;
                self.closure_set_upvalue_cell(f1, dst_index, source_cell)?;
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
                let (proto, upvalues, _globals) = self.closure_parts(closure)?;
                // See `DebugUpvalueid`: `upvalues` also carries the synthetic
                // trailing environment cell, excluded here the same way so it
                // is only reachable through the explicit `_ENV` branch below.
                if index < proto.upvals.len() && upvalues.get(index).is_some() {
                    let cell = upvalues[index];
                    self.upvalue_set(cell, value)?;
                    let name = proto
                        .upval_names
                        .get(index)
                        .map(String::as_bytes)
                        .unwrap_or(b"(no name)");
                    Ok(vec![LuaValue::String(self.intern_str(name))])
                } else if index == proto.upvals.len() && has_implicit_environment(&proto) {
                    // See `DebugGetupvalue`: `_ENV` is represented by the
                    // shared scope cell during the transition. It is already
                    // the caller's default environment in the only binary
                    // chunk shape this compatibility path exposes.
                    Ok(vec![LuaValue::String(self.intern_str(b"_ENV"))])
                } else {
                    Ok(vec![LuaValue::Nil])
                }
            }
            NativeFunction::DebugGetlocal => {
                // The optional leading coroutine selects an activation only
                // for numeric-level queries.  A prototype query is
                // independent of an activation, but Lua still accepts the
                // same optional thread spelling (`getlocal(co, f, n)`).
                let subject_index = match required(0)? {
                    LuaValue::Thread(_) | LuaValue::CoroutineWrapper(_) => 1,
                    _ => 0,
                };
                let subject = required(subject_index)?;
                let index = coerce_integer(&required(subject_index + 1)?)?;
                if let Some(LuaValue::Thread(thread) | LuaValue::CoroutineWrapper(thread)) = args.first() {
                    if let LuaValue::Closure(closure) = subject {
                        let proto = self.closure_prototype(closure)?;
                        return Ok(active_local(&proto, 0, index).map(|(_, name)| {
                            vec![LuaValue::String(self.intern_str(name.as_bytes()))]
                        }).unwrap_or_else(|| vec![LuaValue::Nil]));
                    }
                    let level = coerce_integer(&subject).map_err(|_| {
                        LuaError::new("bad argument #2 to 'getlocal' (level expected)")
                    })?;
                    if level < 1 {
                        return Ok(vec![LuaValue::Nil]);
                    }
                    let frame = {
                        let coroutine = self.coroutine(*thread);
                        let frames = coroutine.frames.borrow();
                        let frame = frames.iter().rev()
                            .nth((level - 1) as usize)
                            .and_then(|frame| match frame {
                                Frame::Lua(frame) => Some(frame.clone()),
                                Frame::Native(_) => None,
                            });
                        frame
                    };
                    let Some(frame) = frame else {
                        return Ok(vec![LuaValue::Nil]);
                    };
                    let result = if index < 0 {
                        index.checked_neg().and_then(|index| index.checked_sub(1))
                            .and_then(|index| usize::try_from(index).ok())
                            .and_then(|index| frame.varargs.get(index)).cloned()
                            .map(|value| (b"(vararg)".as_slice(), value))
                    } else {
                        active_frame_local(&frame, index).map(|(register, name)| (
                            name.as_bytes(),
                            if register == usize::MAX { LuaValue::Nil }
                            else { reg_get(self, &frame.regs, &frame.cells, register) },
                        ))
                    };
                    return Ok(result.map(|(name, value)| vec![
                        LuaValue::String(self.intern_str(name)), value,
                    ]).unwrap_or_else(|| vec![LuaValue::Nil]));
                }
                match subject {
                    // `debug.getlocal(function, n)` queries names from a
                    // prototype at PC zero; it has no activation, so Lua
                    // returns the name alone.  At that PC only parameters
                    // (and Sol's named-vararg binding, when present) are
                    // live.
                    LuaValue::Closure(closure) => {
                        let proto = self.closure_prototype(closure)?;
                        Ok(active_local(&proto, 0, index)
                            .map(|(_, name)| {
                                vec![LuaValue::String(self.intern_str(name.as_bytes()))]
                            })
                            .unwrap_or_else(|| vec![LuaValue::Nil]))
                    }
                    LuaValue::NativeFunction(_)
                    | LuaValue::Native(_)
                    | LuaValue::RegisteredNative(_)
                    | LuaValue::GMatchIterator(_)
                    | LuaValue::CoroutineWrapper(_) => Ok(vec![LuaValue::Nil]),
                    _ => {
                        let level = coerce_integer(&subject).map_err(|_| {
                            LuaError::new(
                                "bad argument #1 to 'getlocal' (function or level expected)",
                            )
                        })?;
                        if level < 0 {
                            return Err(LuaError::new(
                                "bad argument #1 to 'getlocal' (level must be non-negative)",
                            ));
                        }
                        if level == 0 {
                            // Level zero is this native `getlocal` call. Its
                            // arguments are the C activation's temporary
                            // slots, in the same order Lua exposes them.
                            return Ok(usize::try_from(index)
                                .ok()
                                .and_then(|index| index.checked_sub(1))
                                .and_then(|index| args.get(index).cloned())
                                .map(|value| {
                                    vec![LuaValue::String(self.intern_str(b"(C temporary)")), value]
                                })
                                .unwrap_or_else(|| vec![LuaValue::Nil]));
                        }
                        if self.hook_callback_frame
                            .is_some_and(|callback| level as usize == self.frames.len() - callback + 1)
                        {
                            if let Some(transfer) = &self.hook_transfer {
                                if !transfer.temporary_name.is_empty() {
                                    if let Some(value) = usize::try_from(index).ok()
                                        .and_then(|index| index.checked_sub(transfer.first))
                                        .and_then(|offset| transfer.values.get(offset))
                                    {
                                        return Ok(vec![
                                            LuaValue::String(self.intern_str(transfer.temporary_name)),
                                            value.clone(),
                                        ]);
                                    }
                                }
                            }
                        }
                        let interrupted_level = self.hook_callback_frame
                            .map(|callback| self.frames.len() - callback + 1);
                        let interrupted = if interrupted_level == Some(level as usize) {
                            self.hook_interrupted_locals.as_ref()
                        } else {
                            None
                        };
                        let frame = if let Some(frame) = interrupted {
                            Some(frame)
                        } else {
                            match self.frames.iter().rev().nth((level - 1) as usize) {
                                Some(Frame::Lua(frame)) => Some(frame),
                                Some(Frame::Native(_)) => return Ok(vec![LuaValue::Nil]),
                                None => None,
                            }
                        };
                        let frame = frame.ok_or_else(|| {
                            LuaError::new("bad argument #1 to 'getlocal' (level out of range)")
                        })?;
                        let result = match frame {
                            frame if index < 0 => index
                                .checked_neg()
                                .and_then(|index| index.checked_sub(1))
                                .and_then(|index| usize::try_from(index).ok())
                                .and_then(|index| frame.varargs.get(index))
                                .cloned()
                                .map(|value| (b"(vararg)".as_slice(), value)),
                            frame => {
                                active_frame_local(frame, index).map(|(register, name)| {
                                    (
                                        name.as_bytes(),
                                        if register == usize::MAX {
                                            LuaValue::Nil
                                        } else {
                                            reg_get(self, &frame.regs, &frame.cells, register)
                                        },
                                    )
                                })
                            }
                        };
                        Ok(result
                            .map(|(name, value)| {
                                vec![LuaValue::String(self.intern_str(name)), value]
                            })
                            .unwrap_or_else(|| vec![LuaValue::Nil]))
                    }
                }
            }
            NativeFunction::DebugSetlocal => {
                if let Some(LuaValue::Thread(thread) | LuaValue::CoroutineWrapper(thread)) = args.first() {
                    let level = coerce_integer(&required(1)?).map_err(|_| {
                        LuaError::new("bad argument #2 to 'setlocal' (level expected)")
                    })?;
                    let index = coerce_integer(&required(2)?)?;
                    let value = required(3)?;
                    if level <= 0 {
                        return Err(LuaError::new(
                            "bad argument #2 to 'setlocal' (level must be positive)",
                        ));
                    }
                    let coroutine = self.coroutine(*thread);
                    let mut frames = coroutine.frames.borrow_mut();
                    let Some(Frame::Lua(frame)) = frames.iter_mut().rev().nth((level - 1) as usize)
                    else { return Ok(vec![LuaValue::Nil]); };
                    let target = if index < 0 {
                        index.checked_neg().and_then(|index| index.checked_sub(1))
                            .and_then(|index| usize::try_from(index).ok())
                            .filter(|&index| index < frame.varargs.len())
                            .map(|index| (None, index, b"(vararg)".to_vec()))
                    } else {
                        active_frame_local(frame, index).map(|(register, name)| (
                            (register != usize::MAX).then(|| (register, frame.cells[register])),
                            usize::MAX,
                            name.as_bytes().to_vec(),
                        ))
                    };
                    let Some((local, vararg, name)) = target else {
                        return Ok(vec![LuaValue::Nil]);
                    };
                    if let Some((register, cell)) = local {
                        if let Some(cell) = cell {
                            drop(frames);
                            let value = self.encode_value(&value)?;
                            self.canonical_heap.borrow_mut().set_upvalue(cell, value)
                                .expect("local cell must address a live upvalue object");
                        } else {
                            frame.regs[register] = value;
                        }
                    } else if vararg != usize::MAX {
                        frame.varargs[vararg] = value;
                    }
                    return Ok(vec![LuaValue::String(self.intern_str(&name))]);
                }
                let level = coerce_integer(&required(0)?)
                    .map_err(|_| LuaError::new("bad argument #1 to 'setlocal' (level expected)"))?;
                let index = coerce_integer(&required(1)?)?;
                let value = required(2)?;
                if level <= 0 {
                    return Err(LuaError::new(
                        "bad argument #1 to 'setlocal' (level must be positive)",
                    ));
                }
                let target =
                    self.frames.iter().rev().nth((level - 1) as usize).and_then(
                        |frame| match frame {
                            Frame::Lua(frame) if index < 0 => index
                                .checked_neg()
                                .and_then(|index| index.checked_sub(1))
                                .and_then(|index| usize::try_from(index).ok())
                                .filter(|&index| index < frame.varargs.len())
                                .map(|index| (None, index, b"(vararg)".to_vec())),
                            Frame::Lua(frame) => {
                                active_frame_local(frame, index).map(|(register, name)| {
                                    (
                                        (register != usize::MAX)
                                            .then(|| (register, frame.cells[register])),
                                        usize::MAX,
                                        name.as_bytes().to_vec(),
                                    )
                                })
                            }
                            Frame::Native(_) => None,
                        },
                    );
                let Some((local, vararg, name)) = target else {
                    return Ok(vec![LuaValue::Nil]);
                };
                if let Some((register, cell)) = local {
                    if let Some(cell) = cell {
                        let value = self.encode_value(&value)?;
                        self.canonical_heap
                            .borrow_mut()
                            .set_upvalue(cell, value)
                            .expect("local cell must address a live upvalue object");
                    } else if let Some(Frame::Lua(frame)) =
                        self.frames.iter_mut().rev().nth((level - 1) as usize)
                    {
                        frame.regs[register] = value;
                    }
                } else if vararg != usize::MAX {
                    if let Some(Frame::Lua(frame)) =
                        self.frames.iter_mut().rev().nth((level - 1) as usize)
                    {
                        frame.varargs[vararg] = value;
                    }
                }
                Ok(vec![LuaValue::String(self.intern_str(&name))])
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
                if let Some(LuaValue::Thread(thread)) = args.first() {
                    let level = coerce_integer(&required(1)?).map_err(|_| {
                        LuaError::new("bad argument #2 to 'getinfo' (function or level expected)")
                    })?;
                    if level < 1 {
                        return Ok(vec![LuaValue::Nil]);
                    }
                    if let Some(option) = args.get(2) {
                        let option = self.string(option).map_err(|_| {
                            LuaError::new("bad argument #3 to 'getinfo' (string expected)")
                        })?;
                        if option.iter().any(|byte| !matches!(byte,
                            b'n' | b'S' | b'l' | b't' | b'u' | b'f' | b'L' | b'r'))
                        {
                            return Err(LuaError::new(
                                "bad argument #3 to 'getinfo' (invalid option)",
                            ));
                        }
                    }
                    let target = {
                        let coroutine = self.coroutine(*thread);
                        let frames = coroutine.frames.borrow();
                        frames.iter().rev().nth((level - 1) as usize).and_then(|frame| {
                            let Frame::Lua(frame) = frame else { return None; };
                            let line = frame.proto.source_map.location(frame.header.pc)
                                .map_or(-1, |location| location.line as i64);
                            Some((frame.proto.clone(), frame.closure, line, frame.is_tail_call))
                        })
                    };
                    let Some((proto, closure, line, is_tail_call)) = target else {
                        return Ok(vec![LuaValue::Nil]);
                    };
                    let info = self.new_table(None)?;
                    let mut set = |key: &[u8], value: LuaValue| {
                        self.table_set(info, LuaValue::String(self.intern_str(key)), value).unwrap();
                    };
                    self.describe_lua_proto(&mut set, &proto, line, 0);
                    set(b"nups", LuaValue::Integer(visible_upvalue_count(&proto)));
                    set(b"func", LuaValue::Closure(closure));
                    set(b"istailcall", LuaValue::Bool(is_tail_call));
                    return Ok(vec![LuaValue::Table(info)]);
                }
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
                let info = self.new_table(None)?;
                let mut set = |key: &[u8], value: LuaValue| {
                    self.table_set(info, LuaValue::String(self.intern_str(key)), value)
                        .unwrap();
                };
                match &arg0 {
                    LuaValue::Closure(closure) => {
                        // A function value has no active call frame, so real
                        // Lua reports `currentline = -1` and no `__call`-chain
                        // hop count (`extraargs = 0`) here.
                        let proto = self.closure_prototype(*closure)?;
                        self.describe_lua_proto(&mut set, &proto, -1, 0);
                        set(
                            b"nups",
                            LuaValue::Integer(visible_upvalue_count(&proto)),
                        );
                        // Preserve the exact closure identity for a direct
                        // function-value query.
                        set(b"func", LuaValue::Closure(*closure));
                    }
                    LuaValue::NativeFunction(_)
                    | LuaValue::Native(_)
                    | LuaValue::RegisteredNative(_) => {
                        Self::describe_native(&self.canonical_heap, &mut set);
                    }
                    LuaValue::GMatchIterator(_) => {
                        Self::describe_native(&self.canonical_heap, &mut set);
                        set(b"nups", LuaValue::Integer(3));
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
                        // The blocking native-call bridge fires a call hook
                        // before entering its callee and does not push a
                        // separate explicit `Frame::Native`. Present that
                        // callee as the level just below the hook callback,
                        // including its exact function identity.
                        if let (Some(callback_index), Some(callee)) =
                            (self.hook_callback_frame, self.hook_event_callee.as_ref())
                        {
                            let callee_level = self.frames.len() - callback_index + 1;
                            if level as usize == callee_level {
                                match callee {
                                    LuaValue::Closure(closure) => {
                                        let proto = self.closure_prototype(*closure)?;
                                        self.describe_lua_proto(&mut set, &proto, -1, 0);
                                    }
                                    _ => Self::describe_native(&self.canonical_heap, &mut set),
                                }
                                set(b"func", callee.clone());
                                if let Some(transfer) = &self.hook_transfer {
                                    set(b"ftransfer", LuaValue::Integer(transfer.first as i64));
                                    set(b"ntransfer", LuaValue::Integer(transfer.values.len() as i64));
                                }
                                return Ok(vec![LuaValue::Table(info)]);
                            }
                        }
                        if let (Some(callback_index), Some((proto, closure, current_line))) =
                            (self.hook_callback_frame, self.hook_interrupted_info.as_ref())
                        {
                            let interrupted_level = self.frames.len() - callback_index + 1;
                            if level as usize == interrupted_level {
                                self.describe_lua_proto(&mut set, proto, *current_line, 0);
                                if let Some((namewhat, name)) = self.call_site_name(1) {
                                    set(b"namewhat", LuaValue::String(self.intern_str(namewhat.as_bytes())));
                                    set(b"name", LuaValue::String(self.intern_str(name.into_bytes())));
                                } else if let Some(name) = Self::declared_lua_name(proto) {
                                    set(b"namewhat", LuaValue::String(self.intern_str(b"local")));
                                    set(b"name", LuaValue::String(self.intern_str(name)));
                                }
                                set(b"func", LuaValue::Closure(*closure));
                                if let Some(transfer) = &self.hook_transfer {
                                    set(b"ftransfer", LuaValue::Integer(transfer.first as i64));
                                    set(b"ntransfer", LuaValue::Integer(transfer.values.len() as i64));
                                }
                                return Ok(vec![LuaValue::Table(info)]);
                            }
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
                                set(b"istailcall", LuaValue::Bool(lua_frame.is_tail_call));
                                if let Some(label) = lua_frame.entry_label {
                                    set(b"namewhat", LuaValue::String(self.intern_str(b"metamethod")));
                                    set(b"name", LuaValue::String(self.intern_str(label.as_bytes())));
                                } else if self.hook_callback_frame
                                    == self.frames.len().checked_sub(level as usize)
                                {
                                    set(b"namewhat", LuaValue::String(self.intern_str(b"hook")));
                                    set(b"name", LuaValue::String(self.intern_str(b"?")));
                                } else if let Some((namewhat, name)) = self.call_site_name(level) {
                                    set(
                                        b"namewhat",
                                        LuaValue::String(self.intern_str(namewhat.as_bytes())),
                                    );
                                    set(
                                        b"name",
                                        LuaValue::String(self.intern_str(name.into_bytes())),
                                    );
                                } else if let Some(name) = Self::declared_lua_name(&lua_frame.proto)
                                {
                                    set(b"namewhat", LuaValue::String(self.intern_str(b"local")));
                                    set(b"name", LuaValue::String(self.intern_str(name)));
                                }
                                set(b"nups", LuaValue::Integer(visible_upvalue_count(&lua_frame.proto)));
                                set(b"func", LuaValue::Closure(lua_frame.closure));
                            }
                            Frame::Native(_) => {
                                Self::describe_native(&self.canonical_heap, &mut set)
                            }
                        }
                        if self.hook_callback_frame
                            .is_some_and(|callback| level as usize == self.frames.len() - callback + 1)
                        {
                            if let Some(transfer) = &self.hook_transfer {
                                set(b"ftransfer", LuaValue::Integer(transfer.first as i64));
                                set(b"ntransfer", LuaValue::Integer(transfer.values.len() as i64));
                            }
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
                    LuaValue::Table(table) => self
                        .table_metatable(table)
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::String(_) => LuaValue::Table(self.string_metatable),
                    LuaValue::Integer(_) | LuaValue::Float(_) => self
                        .number_metatable
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::Bool(_) => self
                        .boolean_metatable
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil),
                    LuaValue::Nil => self
                        .nil_metatable
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
                        self.table_set_metatable(*table, new_metatable)?;
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
                // Error-handler calls use the trail saved before unwind;
                // ordinary calls walk live frames. During a line/count hook
                // the interrupted frame is temporarily outside `frames`,
                // so its saved traceback line follows the callback's frame.
                if let Some(LuaValue::Thread(thread) | LuaValue::CoroutineWrapper(thread)) = args.first() {
                    let message = args.get(1).cloned().unwrap_or(LuaValue::Nil);
                    let prefix = match &message {
                        LuaValue::Nil => None,
                        LuaValue::String(bytes) => {
                            Some(String::from_utf8_lossy(bytes.as_bytes()).into_owned())
                        }
                        other => return Ok(vec![other.clone()]),
                    };
                    let level = match args.get(2) {
                        Some(LuaValue::Nil) | None => 0,
                        Some(value) => coerce_integer(value)?.max(0) as usize,
                    };
                    let coroutine = self.coroutine(*thread);
                    let frames = coroutine.frames.borrow();
                    let mut entries = Vec::new();
                    // A suspended coroutine is paused inside the leaf
                    // `coroutine.yield` call, which does not have a durable
                    // trampoline frame. Reconstruct its visible C frame for
                    // the same traceback shape Lua exposes.
                    if frames.iter().any(|frame| matches!(frame, Frame::Lua(_))) {
                        entries.push("[C]: in field 'yield'".to_owned());
                    }
                    for frame in frames.iter().rev() {
                        if let Frame::Lua(frame) = frame {
                            let line = frame.proto.source_map.location(frame.header.pc)
                                .map_or(0, |location| location.line);
                            let label = self.traceback_frame_label(&frame.proto, line);
                            let entry = if frame.proto.line_defined == 0 {
                                format!("{label} in main chunk")
                            } else if let Some(name) = Self::declared_lua_name(&frame.proto) {
                                format!("{label} in function '{}'", String::from_utf8_lossy(name))
                            } else {
                                format!("{label} in function <{label}>")
                            };
                            entries.push(entry);
                        }
                    }
                    if entries.is_empty() {
                        entries.extend(coroutine.dead_trace.borrow().iter().cloned());
                    }
                    let mut out = prefix.map(|prefix| format!("{prefix}\n")).unwrap_or_default();
                    out.push_str("stack traceback:");
                    for entry in entries.into_iter().skip(level) {
                        out.push_str("\n\t");
                        out.push_str(&entry);
                    }
                    return Ok(vec![LuaValue::String(self.fresh_str(out.into_bytes()))]);
                }
                let message = args.first().cloned().unwrap_or(LuaValue::Nil);
                let prefix = match &message {
                    LuaValue::Nil => None,
                    LuaValue::String(bytes) => {
                        Some(String::from_utf8_lossy(bytes.as_bytes()).into_owned())
                    }
                    other => return Ok(vec![other.clone()]),
                };
                let level = match args.get(1) {
                    Some(value) => coerce_integer(value)?.max(0) as usize,
                    None => 1,
                };
                let trail = self.pending_error_stack.take();
                let mut out = String::new();
                if let Some(prefix) = prefix {
                    out.push_str(&prefix);
                    out.push('\n');
                }
                out.push_str("stack traceback:");
                if let Some(trail) = trail {
                    for entry in trail.iter().rev() {
                        out.push_str("\n\t");
                        out.push_str(entry);
                    }
                } else {
                    let mut entries = Vec::new();
                    if level == 0 {
                        entries.push("[C]: in function 'traceback'".to_owned());
                    }
                    for (index, frame) in self
                        .frames
                        .iter()
                        .enumerate()
                        .rev()
                        .skip(level.saturating_sub(1))
                    {
                        let entry = match frame {
                            Frame::Lua(frame) => {
                                let line = frame
                                    .proto
                                    .source_map
                                    .location(frame.header.pc)
                                    .map(|location| location.line)
                                    .unwrap_or(0);
                                let mut entry = self.traceback_frame_label(&frame.proto, line);
                                if self.hook_callback_frame == Some(index) {
                                    entry.push_str(" in hook '?'");
                                } else if frame.proto.line_defined == 0 {
                                    entry.push_str(" in main chunk");
                                } else if let Some(name) = Self::declared_lua_name(&frame.proto) {
                                    entry.push_str(" in function '");
                                    entry.push_str(&String::from_utf8_lossy(name));
                                    entry.push('\'');
                                } else {
                                    entry.push_str(" in function <?>");
                                }
                                Some(entry)
                            }
                            Frame::Native(cont) => match cont {
                                NativeCont::Pcall => Some("[C]: in function 'pcall'".to_owned()),
                                NativeCont::Xpcall(_) => Some("[C]: in function 'xpcall'".to_owned()),
                                NativeCont::Sort(_) => Some("[C]: in function 'sort'".to_owned()),
                                NativeCont::Gsub(_) => Some("[C]: in function 'gsub'".to_owned()),
                                // `Once` is the trampoline's private
                                // continuation for one-shot calls (notably
                                // `coroutine.wrap`).  It has no Lua stack
                                // frame counterpart, so it must not appear
                                // in `debug.traceback`; Lua resumes the
                                // wrapped function directly.
                                NativeCont::Once => None,
                            },
                        };
                        if let Some(entry) = entry {
                            entries.push(entry);
                        }
                    }
                    if let Some(interrupted) = &self.hook_interrupted_frame {
                        entries.push(interrupted.clone());
                    }
                    // Lua keeps the first ten and final eleven entries of a
                    // very deep traceback, inserting one synthetic line in
                    // between.  Besides avoiding unbounded diagnostics this
                    // is observable through `debug.traceback` itself.
                    const FIRST: usize = 10;
                    const LAST: usize = 11;
                    if entries.len() > FIRST + LAST {
                        let skipped = entries.len() - FIRST - LAST;
                        let tail = entries.split_off(FIRST);
                        for entry in entries {
                            out.push_str("\n\t");
                            out.push_str(&entry);
                        }
                        out.push_str(&format!("\n\t...\t(skipping {skipped} levels)"));
                        for entry in tail.into_iter().skip(skipped) {
                            out.push_str("\n\t");
                            out.push_str(&entry);
                        }
                    } else {
                        for entry in entries {
                            out.push_str("\n\t");
                            out.push_str(&entry);
                        }
                    }
                }
                Ok(vec![LuaValue::String(self.fresh_str(out.into_bytes()))])
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
                        .unwrap_or(self.main_coroutine)
                };
                let hook = args.get(index).cloned().unwrap_or(LuaValue::Nil);
                let target_co = self.coroutine(target);
                let registry = TableRef::new(self.c_registry);
                let Some(LuaValue::Table(hook_table)) =
                    self.table_get_str_field(registry, b"_HOOKKEY")
                else {
                    return Err(LuaError::new("debug hook registry is unavailable"));
                };
                self.table_set(hook_table, LuaValue::Thread(target), hook.clone())?;
                if matches!(hook, LuaValue::Nil) {
                    *target_co.hook.borrow_mut() = None;
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
                    *target_co.hook.borrow_mut() = Some(Rc::new(HookState {
                        callback: hook,
                        mask,
                        count,
                        // Installing a Lua hook itself is expressed through
                        // Sol's explicit native-call trampoline. Its setup
                        // and return instructions are not Lua bytecode that
                        // the newly installed hook may observe. Start after
                        // that fixed bridge tail. The 31-instruction debit
                        // also covers call-site setup already dispatched
                        // before the hook becomes visible; subsequent periods
                        // remain exactly `count` bytecode instructions apart.
                        count_remaining: Cell::new(count.saturating_add(31)),
                    }));
                }
                // Refresh the cached fast-path copy immediately if `target`
                // is whichever coroutine/main is currently executing this
                // very call - see `active_hook`'s field doc on `LuaRuntime`.
                let running = self
                    .coroutine_stack
                    .last()
                    .cloned()
                    .unwrap_or(self.main_coroutine);
                if target == running {
                    self.active_hook = target_co.hook.borrow().clone();
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
                        .unwrap_or(self.main_coroutine)
                };
                let hook_state = self.coroutine(target).hook.borrow().clone();
                match hook_state {
                    Some(hook) => {
                        let mut mask_string = String::new();
                        if hook.mask.call {
                            mask_string.push('c');
                        }
                        if hook.mask.ret {
                            mask_string.push('r');
                        }
                        if hook.mask.line {
                            mask_string.push('l');
                        }
                        Ok(vec![
                            hook.callback.clone(),
                            LuaValue::String(self.intern_str(mask_string.into_bytes())),
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
        set(b"ftransfer", LuaValue::Integer(0));
        set(b"ntransfer", LuaValue::Integer(0));
        let linedefined = proto.line_defined as i64;
        let lastlinedefined = proto.last_line_defined as i64;
        // `activelines`: real Lua's `funcinfo` builds this as a set (line ->
        // `true`) of every line this prototype's own bytecode maps to - not
        // its nested closures', which carry separate `Proto`s/source maps.
        let activelines = TableRef::alloc(&mut self.canonical_heap.borrow_mut());
        for pc in 0..proto.source_map.len() as u32 {
            if let Some(location) = proto.source_map.location(pc) {
                self.table_set(
                    activelines,
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
        // Chunks report `what = "main"`, while an ordinary function named
        // `main` is still a Lua function. `line_defined == 0` is the
        // bytecode-level marker for the explicitly tracked AST chunk shape.
        let what: &[u8] = if proto.line_defined == 0 {
            b"main"
        } else {
            b"Lua"
        };
        set(b"what", LuaValue::String(self.intern_str(what)));
        set(b"namewhat", LuaValue::String(self.intern_str(Vec::new())));
        set(b"name", LuaValue::Nil);
        if proto.source_map.is_empty() {
            set(b"source", LuaValue::String(self.intern_str(b"=?")));
            set(b"short_src", LuaValue::String(self.intern_str(b"?")));
        } else if let Some(source) = self.chunk_sources.get(&(Rc::as_ptr(proto) as usize)) {
            set(
                b"source",
                LuaValue::String(self.intern_str(source.as_slice())),
            );
            set(
                b"short_src",
                LuaValue::String(self.intern_str(Self::short_src(source))),
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
    fn describe_native(heap: &RcRef<sol_core::Heap>, set: &mut impl FnMut(&[u8], LuaValue)) {
        set(b"currentline", LuaValue::Integer(-1));
        set(b"extraargs", LuaValue::Integer(0));
        set(b"ftransfer", LuaValue::Integer(0));
        set(b"ntransfer", LuaValue::Integer(0));
        set(b"linedefined", LuaValue::Integer(-1));
        set(b"lastlinedefined", LuaValue::Integer(-1));
        set(b"isvararg", LuaValue::Bool(true));
        set(b"nparams", LuaValue::Integer(0));
        set(b"nups", LuaValue::Integer(0));
        set(b"istailcall", LuaValue::Bool(false));
        set(
            b"what",
            LuaValue::String(CanonicalString::intern(heap.clone(), b"C")),
        );
        set(
            b"namewhat",
            LuaValue::String(CanonicalString::intern(heap.clone(), b"")),
        );
        set(b"name", LuaValue::Nil);
        set(
            b"source",
            LuaValue::String(CanonicalString::intern(heap.clone(), b"=[C]")),
        );
        set(
            b"short_src",
            LuaValue::String(CanonicalString::intern(heap.clone(), b"[C]")),
        );
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
