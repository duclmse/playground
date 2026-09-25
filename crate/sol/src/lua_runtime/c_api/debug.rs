use super::*;

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_getupvalue(
    state: *mut lua_State,
    function_index: c_int,
    upvalue_index: c_int,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let Some(index) = usize::try_from(upvalue_index)
        .ok()
        .and_then(|index| index.checked_sub(1))
    else {
        return ptr::null();
    };
    let subject = state.value(function_index).cloned();
    let is_lua = matches!(subject, Some(LuaValue::Closure(_)));
    let value = match subject {
        Some(LuaValue::Closure(closure)) => {
            let runtime = unsafe { state.runtime() };
            runtime
                .closure_upvalue_cell(closure, index)
                .and_then(|cell| runtime.upvalue_get(cell).ok())
        }
        Some(LuaValue::CFunction(function)) => {
            let canonical = {
                let runtime = unsafe { state.runtime() };
                let heap = runtime.canonical_heap.borrow();
                let Ok(sol_core::HeapObject::NativeCallable(callable)) =
                    heap.object(function.object_id())
                else {
                    return ptr::null();
                };
                callable.captures.get(index).copied()
            };
            canonical
                .map(|value| state.from_canonical(value))
                .transpose()
                .unwrap_or_else(|error| api_jump(state, error))
        }
        _ => None,
    };
    let Some(value) = value else {
        return ptr::null();
    };
    state.stack.push(value);
    if is_lua {
        c"_ENV".as_ptr()
    } else {
        c"".as_ptr()
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_setupvalue(
    state: *mut lua_State,
    function_index: c_int,
    upvalue_index: c_int,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let Some(index) = usize::try_from(upvalue_index)
        .ok()
        .and_then(|index| index.checked_sub(1))
    else {
        return ptr::null();
    };
    let Some(subject) = state.value(function_index).cloned() else {
        return ptr::null();
    };
    let Some(value) = state.stack.last().cloned() else {
        return ptr::null();
    };
    let name = match subject {
        LuaValue::Closure(closure) => {
            let runtime = unsafe { state.runtime() };
            let Some(cell) = runtime.closure_upvalue_cell(closure, index) else {
                return ptr::null();
            };
            if let Err(error) = runtime.upvalue_set(cell, value) {
                api_jump(state, error);
            }
            c"_ENV".as_ptr()
        }
        LuaValue::CFunction(function) => {
            let canonical = match state.to_canonical(&value) {
                Ok(value) => value,
                Err(error) => api_jump(state, error),
            };
            let update = {
                let runtime = unsafe { state.runtime() };
                let mut heap = runtime.canonical_heap.borrow_mut();
                let captures = match heap.object(function.object_id()) {
                    Ok(sol_core::HeapObject::NativeCallable(callable)) => callable.captures.clone(),
                    _ => return ptr::null(),
                };
                if index >= captures.len() {
                    return ptr::null();
                }
                let mut captures = captures;
                captures[index] = canonical;
                heap.set_native_callable_captures(function.object_id(), captures)
            };
            if let Err(error) = update {
                api_jump(state, LuaError::new(error.to_string()));
            }
            c"".as_ptr()
        }
        _ => return ptr::null(),
    };
    state.stack.pop();
    name
}

#[no_mangle]
pub unsafe extern "C" fn lua_upvalueid(
    state: *mut lua_State,
    function_index: c_int,
    upvalue_index: c_int,
) -> *mut c_void {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null_mut();
    };
    let Some(index) = usize::try_from(upvalue_index)
        .ok()
        .and_then(|index| index.checked_sub(1))
    else {
        return ptr::null_mut();
    };
    match state.value(function_index).cloned() {
        Some(LuaValue::Closure(closure)) => {
            let runtime = unsafe { state.runtime() };
            let exists = runtime
                .closure_upvalues(closure)
                .map(|upvalues| index < upvalues.len())
                .unwrap_or(false);
            if !exists {
                return ptr::null_mut();
            }
            let mut identities = runtime.c_upvalue_identities.borrow_mut();
            let identity = identities
                .entry((closure.object_id(), index))
                .or_insert_with(|| Box::new(0));
            (&mut **identity as *mut u8).cast()
        }
        Some(LuaValue::CFunction(function)) => {
            let exists = {
                let runtime = unsafe { state.runtime() };
                let heap = runtime.canonical_heap.borrow();
                matches!(
                    heap.object(function.object_id()),
                    Ok(sol_core::HeapObject::NativeCallable(callable))
                        if index < callable.captures.len()
                )
            };
            if !exists {
                return ptr::null_mut();
            }
            let runtime = unsafe { state.runtime() };
            let mut identities = runtime.c_upvalue_identities.borrow_mut();
            let identity = identities
                .entry((function.object_id(), index))
                .or_insert_with(|| Box::new(0));
            (&mut **identity as *mut u8).cast()
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_upvaluejoin(
    state: *mut lua_State,
    first_function: c_int,
    first_upvalue: c_int,
    second_function: c_int,
    second_upvalue: c_int,
) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let (Some(first), Some(second)) = (
        state.value(first_function).cloned(),
        state.value(second_function).cloned(),
    ) else {
        return;
    };
    let (LuaValue::Closure(first), LuaValue::Closure(second)) = (first, second) else {
        return;
    };
    let Some(first_index) = usize::try_from(first_upvalue)
        .ok()
        .and_then(|index| index.checked_sub(1))
    else {
        return;
    };
    let Some(second_index) = usize::try_from(second_upvalue)
        .ok()
        .and_then(|index| index.checked_sub(1))
    else {
        return;
    };
    let runtime = unsafe { state.runtime() };
    let Some(source) = runtime.closure_upvalue_cell(second, second_index) else {
        return;
    };
    let _ = runtime.closure_set_upvalue_cell(first, first_index, source);
}

#[no_mangle]
pub unsafe extern "C" fn lua_getstack(
    state: *mut lua_State,
    level: c_int,
    debug: *mut LuaDebug,
) -> c_int {
    let (Some(state), Some(debug)) = (unsafe { state_mut(state) }, unsafe { debug.as_mut() })
    else {
        return 0;
    };
    if level < 0 {
        return 0;
    }
    let exists = {
        let runtime = unsafe { state.runtime() };
        runtime
            .frames
            .iter()
            .rev()
            .filter(|frame| matches!(frame, Frame::Lua(_)))
            .nth(level as usize)
            .is_some()
    } || {
        let thread = state.thread;
        let runtime = unsafe { state.runtime() };
        runtime
            .coroutine(thread)
            .frames
            .borrow()
            .iter()
            .rev()
            .filter(|frame| matches!(frame, Frame::Lua(_)))
            .nth(level as usize)
            .is_some()
    };
    if !exists {
        return 0;
    }
    debug.i_ci = (level as usize + 1) as *mut c_void;
    1
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_getinfo(
    state: *mut lua_State,
    what: *const c_char,
    debug: *mut LuaDebug,
) -> c_int {
    let (Some(state), Some(debug)) = (unsafe { state_mut(state) }, unsafe { debug.as_mut() })
    else {
        return 0;
    };
    if what.is_null() {
        return 0;
    }
    let options = unsafe { CStr::from_ptr(what) }.to_bytes();
    let descriptor = if options.first() == Some(&b'>') {
        match state.stack.pop() {
            Some(LuaValue::Closure(closure)) => {
                let runtime = unsafe { state.runtime() };
                let proto_result = runtime.closure_prototype(closure);
                let nups_result = runtime.closure_upvalues(closure).map(|upvalues| upvalues.len());
                let proto = match proto_result {
                    Ok(proto) => proto,
                    Err(error) => api_jump(state, error),
                };
                let nups = match nups_result {
                    Ok(nups) => nups,
                    Err(error) => api_jump(state, error),
                };
                Some((proto, -1, nups, 0usize))
            }
            Some(LuaValue::CFunction(_)) | Some(LuaValue::NativeFunction(_)) => None,
            _ => return 0,
        }
    } else {
        let level = (debug.i_ci as usize).checked_sub(1).unwrap_or(0);
        let active = {
            let runtime = unsafe { state.runtime() };
            runtime
                .frames
                .iter()
                .rev()
                .filter_map(|frame| match frame {
                    Frame::Lua(frame) => Some((
                        frame.proto.clone(),
                        frame.header.pc as i64,
                        frame.upvals.len(),
                        frame.call_chain_hops,
                    )),
                    Frame::Native(_) => None,
                })
                .nth(level)
        };
        active.or_else(|| {
            let thread = state.thread;
            let runtime = unsafe { state.runtime() };
            runtime
                .coroutine(thread)
                .frames
                .borrow()
                .iter()
                .rev()
                .filter_map(|frame| match frame {
                    Frame::Lua(frame) => Some((
                        frame.proto.clone(),
                        frame.header.pc as i64,
                        frame.upvals.len(),
                        frame.call_chain_hops,
                    )),
                    Frame::Native(_) => None,
                })
                .nth(level)
        })
    };
    let Some((proto, pc, upvalues, extraargs)) = descriptor else {
        debug.name = ptr::null();
        debug.namewhat = c"".as_ptr();
        debug.what_ = c"C".as_ptr();
        debug.source = c"=[C]".as_ptr();
        debug.srclen = 4;
        debug.currentline = -1;
        debug.linedefined = -1;
        debug.lastlinedefined = -1;
        debug.nups = 0;
        debug.nparams = 0;
        debug.isvararg = 1;
        debug.extraargs = 0;
        debug.istailcall = 0;
        debug.short_src.fill(0);
        for (slot, byte) in debug.short_src.iter_mut().zip(b"[C]") {
            *slot = *byte as c_char;
        }
        return 1;
    };
    let name = proto.metadata.name.as_bytes().to_vec();
    let source = {
        let runtime = unsafe { state.runtime() };
        runtime
            .chunk_sources
            .get(&(Rc::as_ptr(&proto) as usize))
            .map(|source| source.as_ref().clone())
            .unwrap_or_else(|| b"=?".to_vec())
    };
    let current_line = usize::try_from(pc)
        .ok()
        .and_then(|pc| proto.source_map.location(pc as u32))
        .map_or(-1, |location| location.line as c_int);
    let last_line = (0..proto.source_map.len() as u32)
        .filter_map(|pc| proto.source_map.location(pc).map(|location| location.line))
        .max()
        .unwrap_or(proto.line_defined) as c_int;
    debug.name = store_c_bytes(state, &name);
    debug.namewhat = c"global".as_ptr();
    debug.what_ = c"Lua".as_ptr();
    debug.source = store_c_bytes(state, &source);
    debug.srclen = source.len();
    debug.currentline = current_line;
    debug.linedefined = proto.line_defined as c_int;
    debug.lastlinedefined = last_line;
    debug.nups = upvalues.min(u8::MAX as usize) as u8;
    debug.nparams = proto.metadata.arity.parameters.min(u8::MAX as u32) as u8;
    debug.isvararg = proto.metadata.arity.variadic as c_char;
    debug.extraargs = extraargs.min(u8::MAX as usize) as u8;
    debug.istailcall = 0;
    debug.ftransfer = 0;
    debug.ntransfer = 0;
    debug.short_src.fill(0);
    for (slot, byte) in debug.short_src.iter_mut().zip(source.iter().take(59)) {
        *slot = *byte as c_char;
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn lua_getlocal(
    state: *mut lua_State,
    debug: *const LuaDebug,
    local_index: c_int,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let (Some(debug), Some(index)) = (
        unsafe { debug.as_ref() },
        usize::try_from(local_index)
            .ok()
            .and_then(|index| index.checked_sub(1)),
    ) else {
        return ptr::null();
    };
    let level = (debug.i_ci as usize).saturating_sub(1);
    let value = {
        let runtime = unsafe { state.runtime() };
        runtime
            .frames
            .iter()
            .rev()
            .filter_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame),
                Frame::Native(_) => None,
            })
            .nth(level)
            .and_then(|frame| frame.regs.get(index).cloned())
    };
    let Some(value) = value else {
        return ptr::null();
    };
    state.stack.push(value);
    c"(temporary)".as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn lua_setlocal(
    state: *mut lua_State,
    debug: *const LuaDebug,
    local_index: c_int,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let (Some(debug), Some(index), Some(value)) = (
        unsafe { debug.as_ref() },
        usize::try_from(local_index)
            .ok()
            .and_then(|index| index.checked_sub(1)),
        state.stack.pop(),
    ) else {
        return ptr::null();
    };
    let level = (debug.i_ci as usize).saturating_sub(1);
    let runtime = unsafe { state.runtime() };
    let Some(frame) = runtime
        .frames
        .iter_mut()
        .rev()
        .filter_map(|frame| match frame {
            Frame::Lua(frame) => Some(frame),
            Frame::Native(_) => None,
        })
        .nth(level)
    else {
        return ptr::null();
    };
    let Some(slot) = frame.regs.get_mut(index) else {
        return ptr::null();
    };
    *slot = value;
    c"(temporary)".as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn lua_sethook(
    state: *mut lua_State,
    hook: Option<LuaHook>,
    mask: c_int,
    count: c_int,
) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.hook = hook;
        state.hook_mask = if hook.is_some() { mask } else { 0 };
        state.hook_count = count;
        state.hook_remaining = count;
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_gethook(state: *mut lua_State) -> Option<LuaHook> {
    unsafe { state.as_ref() }.and_then(|state| state.hook)
}

#[no_mangle]
pub unsafe extern "C" fn lua_gethookmask(state: *mut lua_State) -> c_int {
    unsafe { state.as_ref() }
        .map(|state| state.hook_mask)
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn lua_gethookcount(state: *mut lua_State) -> c_int {
    unsafe { state.as_ref() }
        .map(|state| state.hook_count)
        .unwrap_or(0)
}
