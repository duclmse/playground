use super::*;

#[no_mangle]
pub unsafe extern "C" fn lua_createtable(state: *mut lua_State, _narray: c_int, _nrec: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let heap = unsafe { state.runtime() }.canonical_heap.clone();
    state
        .stack
        .push(LuaValue::CanonicalTable(CanonicalTable::allocate(heap)));
}

#[no_mangle]
pub unsafe extern "C" fn lua_getglobal(state: *mut lua_State, name: *const c_char) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    if name.is_null() {
        state.stack.push(LuaValue::Nil);
        return LUA_TNIL;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let runtime = unsafe { state.runtime() };
    let value = runtime.globals.get(runtime, &name);
    let tag = value_tag(&value);
    state.stack.push(value);
    tag
}

#[no_mangle]
pub unsafe extern "C" fn lua_setglobal(state: *mut lua_State, name: *const c_char) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let Some(value) = state.stack.pop() else {
        return;
    };
    if name.is_null() {
        return;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let runtime = unsafe { state.runtime() };
    runtime.globals.define(runtime, &name, value, false);
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_gettable(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let Some(key) = state.stack.pop() else {
        return LUA_TNONE;
    };
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_get(key))
    } else {
        match table {
            Some(table) => unsafe { state.runtime() }.index_get(table, key),
            None => Err(LuaError::new("invalid table index")),
        }
    };
    match result {
        Ok(value) => {
            let tag = value_tag(&value);
            state.stack.push(value);
            tag
        }
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_settable(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let Some(value) = state.stack.pop() else {
        return;
    };
    let Some(key) = state.stack.pop() else { return };
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_set(key, &value))
    } else {
        match table {
            Some(table) => unsafe { state.runtime() }.index_set(table, key, value),
            None => Err(LuaError::new("invalid table index")),
        }
    };
    if let Err(error) = result {
        api_jump(state, error);
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_rawget(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let Some(key) = state.stack.pop() else {
        return LUA_TNONE;
    };
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_get(key))
    } else {
        table
            .ok_or_else(|| LuaError::new("invalid table index"))
            .and_then(|table| raw_table_get(state, table, &key))
    };
    match result {
        Ok(value) => {
            let tag = value_tag(&value);
            state.stack.push(value);
            tag
        }
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_rawset(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let Some(value) = state.stack.pop() else {
        return;
    };
    let Some(key) = state.stack.pop() else { return };
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_set(key, &value))
    } else {
        table
            .ok_or_else(|| LuaError::new("invalid table index"))
            .and_then(|table| raw_table_set(state, table, key, value))
    };
    if let Err(error) = result {
        api_jump(state, error);
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_rawgetp(
    state: *mut lua_State,
    index: c_int,
    pointer: *const c_void,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let key = LuaValue::LightUserdata(pointer as usize);
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_get(key))
    } else {
        table
            .ok_or_else(|| LuaError::new("invalid table index"))
            .and_then(|table| raw_table_get(state, table, &key))
    };
    match result {
        Ok(value) => {
            let tag = value_tag(&value);
            state.stack.push(value);
            tag
        }
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_rawsetp(
    state: *mut lua_State,
    index: c_int,
    pointer: *const c_void,
) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let table = if index == LUA_REGISTRYINDEX {
        None
    } else {
        state.value(index).cloned()
    };
    let Some(value) = state.stack.pop() else {
        return;
    };
    let key = LuaValue::LightUserdata(pointer as usize);
    let result = if index == LUA_REGISTRYINDEX {
        state
            .to_canonical(&key)
            .and_then(|key| state.registry_set(key, &value))
    } else {
        table
            .ok_or_else(|| LuaError::new("invalid table index"))
            .and_then(|table| raw_table_set(state, table, key, value))
    };
    if let Err(error) = result {
        api_jump(state, error);
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_geti(
    state: *mut lua_State,
    index: c_int,
    key: LuaInteger,
) -> c_int {
    let absolute = unsafe { lua_absindex(state, index) };
    unsafe { lua_pushinteger(state, key) };
    unsafe { lua_gettable(state, absolute) }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_seti(state: *mut lua_State, index: c_int, key: LuaInteger) {
    let absolute = unsafe { lua_absindex(state, index) };
    unsafe { lua_pushinteger(state, key) };
    unsafe { lua_rotate(state, -2, 1) };
    unsafe { lua_settable(state, absolute) };
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_getfield(
    state: *mut lua_State,
    index: c_int,
    key: *const c_char,
) -> c_int {
    let absolute = unsafe { lua_absindex(state, index) };
    unsafe { lua_pushstring(state, key) };
    unsafe { lua_gettable(state, absolute) }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_setfield(
    state: *mut lua_State,
    index: c_int,
    key: *const c_char,
) {
    let absolute = unsafe { lua_absindex(state, index) };
    unsafe { lua_pushstring(state, key) };
    unsafe { lua_rotate(state, -2, 1) };
    unsafe { lua_settable(state, absolute) };
}

#[no_mangle]
pub unsafe extern "C" fn lua_getmetatable(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let Some(value) = state.value(index).cloned() else {
        return 0;
    };
    match value {
        LuaValue::Table(table) => {
            let metatable = unsafe { state.runtime() }.table_metatable(table);
            if let Some(metatable) = metatable {
                state.stack.push(LuaValue::Table(metatable));
                1
            } else {
                0
            }
        }
        LuaValue::Userdata(value) => push_canonical_metatable(state, value.object_id()),
        LuaValue::CanonicalTable(value) => push_canonical_metatable(state, value.object_id()),
        _ => 0,
    }
}

fn push_canonical_metatable(state: &mut lua_State, object: sol_core::ObjectId) -> c_int {
    let heap_ref = unsafe { state.runtime() }.canonical_heap.clone();
    let metatable = {
        let heap = heap_ref.borrow();
        match heap.object(object) {
            Ok(sol_core::HeapObject::Userdata(value)) => value.metatable,
            Ok(sol_core::HeapObject::Table(value)) => value.metatable,
            _ => None,
        }
    };
    if let Some(metatable) = metatable {
        state
            .stack
            .push(LuaValue::CanonicalTable(CanonicalTable::root_existing(
                heap_ref, metatable,
            )));
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_setmetatable(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let target = state.value(index).cloned();
    let Some(metatable) = state.stack.pop() else {
        return 0;
    };
    match (target, metatable) {
        (Some(LuaValue::Table(target)), LuaValue::Table(metatable)) => {
            let runtime = unsafe { state.runtime() };
            runtime.table_set_metatable(target, Some(metatable)).is_ok() as c_int
        }
        (Some(LuaValue::Table(target)), LuaValue::Nil) => {
            let runtime = unsafe { state.runtime() };
            runtime.table_set_metatable(target, None).is_ok() as c_int
        }
        (Some(LuaValue::Userdata(target)), LuaValue::CanonicalTable(metatable)) => {
            let runtime = unsafe { state.runtime() };
            runtime
                .canonical_heap
                .borrow_mut()
                .set_metatable(target.object_id(), Some(metatable.object_id()))
                .is_ok() as c_int
        }
        (Some(LuaValue::CanonicalTable(target)), LuaValue::CanonicalTable(metatable)) => {
            let runtime = unsafe { state.runtime() };
            runtime
                .canonical_heap
                .borrow_mut()
                .set_metatable(target.object_id(), Some(metatable.object_id()))
                .is_ok() as c_int
        }
        (Some(LuaValue::Userdata(target)), LuaValue::Nil) => {
            let runtime = unsafe { state.runtime() };
            runtime
                .canonical_heap
                .borrow_mut()
                .set_metatable(target.object_id(), None)
                .is_ok() as c_int
        }
        (Some(LuaValue::CanonicalTable(target)), LuaValue::Nil) => {
            let runtime = unsafe { state.runtime() };
            runtime
                .canonical_heap
                .borrow_mut()
                .set_metatable(target.object_id(), None)
                .is_ok() as c_int
        }
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawgeti(
    state: *mut lua_State,
    index: c_int,
    key: LuaInteger,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    let value = if index == LUA_REGISTRYINDEX {
        state
            .registry_get(sol_core::Value::integer(key))
            .unwrap_or(LuaValue::Nil)
    } else {
        match state.value(index).cloned() {
            Some(LuaValue::Table(table)) => unsafe { state.runtime() }
                .table_get(table, &LuaValue::Integer(key))
                .unwrap_or(LuaValue::Nil),
            Some(LuaValue::CanonicalTable(table)) => canonical_table_get(
                unsafe { state.runtime() },
                table.object_id(),
                &LuaValue::Integer(key),
            )
            .unwrap_or(LuaValue::Nil),
            _ => LuaValue::Nil,
        }
    };
    let tag = value_tag(&value);
    state.stack.push(value);
    tag
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawseti(state: *mut lua_State, index: c_int, key: LuaInteger) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let absolute = state.absolute(index);
    let Some(value) = state.stack.pop() else {
        return;
    };
    if index == LUA_REGISTRYINDEX {
        if let Err(error) = state.registry_set(sol_core::Value::integer(key), &value) {
            state.pending_error = Some(error);
        }
    } else if let Some(absolute) = absolute {
        if let LuaValue::Table(table) = &state.stack[absolute] {
            let table = *table;
            let _ = unsafe { state.runtime() }.table_set(table, LuaValue::Integer(key), value);
        } else if let LuaValue::CanonicalTable(table) = state.stack[absolute].clone() {
            match state.to_canonical(&value) {
                Ok(value) => {
                    let runtime = unsafe { state.runtime() };
                    let _ = runtime.canonical_heap.borrow_mut().table_set(
                        table.object_id(),
                        sol_core::Value::integer(key),
                        value,
                    );
                }
                Err(error) => state.pending_error = Some(error),
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_getiuservalue(state: *mut lua_State, index: c_int, n: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    let Some(LuaValue::Userdata(userdata)) = state.value(index).cloned() else {
        state.stack.push(LuaValue::Nil);
        return LUA_TNONE;
    };
    let value = if n > 0 {
        let runtime = unsafe { state.runtime() };
        let canonical = runtime
            .canonical_heap
            .borrow()
            .userdata_user_value(userdata.object_id(), n as usize - 1)
            .unwrap_or(sol_core::Value::NIL);
        canonical_to_lua(runtime, canonical).unwrap_or(LuaValue::Nil)
    } else {
        LuaValue::Nil
    };
    let tag = value_tag(&value);
    state.stack.push(value);
    tag
}

#[no_mangle]
pub unsafe extern "C" fn lua_setiuservalue(state: *mut lua_State, index: c_int, n: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let target = state.value(index).cloned();
    let Some(value) = state.stack.pop() else {
        return 0;
    };
    let Some(LuaValue::Userdata(userdata)) = target else {
        return 0;
    };
    if n <= 0 {
        return 0;
    }
    let value = match state.to_canonical(&value) {
        Ok(value) => value,
        Err(error) => {
            state.pending_error = Some(error);
            return 0;
        }
    };
    unsafe { state.runtime() }
        .canonical_heap
        .borrow_mut()
        .set_userdata_user_value(userdata.object_id(), n as usize - 1, value)
        .unwrap_or(false) as c_int
}
