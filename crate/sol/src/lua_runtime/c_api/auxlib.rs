use super::*;

#[repr(C)]
pub struct LuaLReg {
    pub name: *const c_char,
    pub function: Option<LuaCFunction>,
}

#[no_mangle]
pub unsafe extern "C" fn luaL_setfuncs(
    state: *mut lua_State,
    functions: *const LuaLReg,
    nup: c_int,
) {
    if functions.is_null() || nup != 0 {
        return;
    }
    let mut current = functions;
    while unsafe { !(*current).name.is_null() } {
        unsafe { lua_pushcclosure(state, (*current).function, 0) };
        unsafe { lua_setfield(state, -2, (*current).name) };
        current = unsafe { current.add(1) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn luaL_newmetatable(state: *mut lua_State, name: *const c_char) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    if name.is_null() {
        return 0;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    let key = {
        let runtime = unsafe { state.runtime() };
        sol_core::Value::object(runtime.canonical_heap.borrow_mut().alloc_string(name))
    };
    if let Ok(LuaValue::CanonicalTable(table)) = state.registry_get(key) {
        state.stack.push(LuaValue::CanonicalTable(table));
        return 0;
    }
    let heap = unsafe { state.runtime() }.canonical_heap.clone();
    let table = LuaValue::CanonicalTable(CanonicalTable::allocate(heap));
    if state.registry_set(key, &table).is_err() {
        return 0;
    }
    state.stack.push(table);
    1
}

#[no_mangle]
pub unsafe extern "C" fn luaL_testudata(
    state: *mut lua_State,
    index: c_int,
    name: *const c_char,
) -> *mut c_void {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null_mut();
    };
    let Some(LuaValue::Userdata(userdata)) = state.value(index).cloned() else {
        return ptr::null_mut();
    };
    if name.is_null() {
        return ptr::null_mut();
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    let key = {
        let runtime = unsafe { state.runtime() };
        sol_core::Value::object(runtime.canonical_heap.borrow_mut().alloc_string(name))
    };
    let Ok(LuaValue::CanonicalTable(expected)) = state.registry_get(key) else {
        return ptr::null_mut();
    };
    let actual = {
        let runtime = unsafe { state.runtime() };
        let heap = runtime.canonical_heap.borrow();
        match heap.object(userdata.object_id()) {
            Ok(sol_core::HeapObject::Userdata(value)) => value.metatable,
            _ => None,
        }
    };
    if actual == Some(expected.object_id()) {
        userdata.bytes_ptr()
    } else {
        ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkudata(
    state: *mut lua_State,
    index: c_int,
    name: *const c_char,
) -> *mut c_void {
    let value = unsafe { luaL_testudata(state, index, name) };
    if value.is_null() {
        if let Some(state) = unsafe { state_mut(state) } {
            state.pending_error = Some(LuaError::new(format!(
                "bad argument #{} (userdata expected)",
                index
            )));
        }
        resume_unwind(Box::new(CApiJump));
    }
    value
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkinteger(
    state: *mut lua_State,
    index: c_int,
) -> LuaInteger {
    let mut valid = 0;
    let value = unsafe { lua_tointegerx(state, index, &mut valid) };
    if valid == 0 {
        if let Some(state) = unsafe { state_mut(state) } {
            state.pending_error = Some(LuaError::new(format!(
                "bad argument #{} (integer expected)",
                index
            )));
        }
        resume_unwind(Box::new(CApiJump));
    }
    value
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checklstring(
    state: *mut lua_State,
    index: c_int,
    len: *mut usize,
) -> *const c_char {
    let value = unsafe { lua_tolstring(state, index, len) };
    if value.is_null() {
        if let Some(state) = unsafe { state_mut(state) } {
            state.pending_error = Some(LuaError::new(format!(
                "bad argument #{} (string expected)",
                index
            )));
        }
        resume_unwind(Box::new(CApiJump));
    }
    value
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checknumber(state: *mut lua_State, index: c_int) -> LuaNumber {
    let mut valid = 0;
    let value = unsafe { lua_tonumberx(state, index, &mut valid) };
    if valid == 0 {
        if let Some(state) = unsafe { state_mut(state) } {
            state.pending_error = Some(LuaError::new(format!(
                "bad argument #{} (number expected)",
                index
            )));
        }
        resume_unwind(Box::new(CApiJump));
    }
    value
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_optinteger(
    state: *mut lua_State,
    index: c_int,
    default: LuaInteger,
) -> LuaInteger {
    if matches!(unsafe { lua_type(state, index) }, LUA_TNONE | LUA_TNIL) {
        default
    } else {
        unsafe { luaL_checkinteger(state, index) }
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_optnumber(
    state: *mut lua_State,
    index: c_int,
    default: LuaNumber,
) -> LuaNumber {
    if matches!(unsafe { lua_type(state, index) }, LUA_TNONE | LUA_TNIL) {
        default
    } else {
        unsafe { luaL_checknumber(state, index) }
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_optlstring(
    state: *mut lua_State,
    index: c_int,
    default: *const c_char,
    len: *mut usize,
) -> *const c_char {
    if matches!(unsafe { lua_type(state, index) }, LUA_TNONE | LUA_TNIL) {
        if !len.is_null() {
            unsafe {
                *len = if default.is_null() {
                    0
                } else {
                    CStr::from_ptr(default).to_bytes().len()
                }
            };
        }
        default
    } else {
        unsafe { luaL_checklstring(state, index, len) }
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checktype(
    state: *mut lua_State,
    index: c_int,
    expected: c_int,
) {
    let actual = unsafe { lua_type(state, index) };
    if actual != expected {
        let expected_name =
            unsafe { CStr::from_ptr(lua_typename(state, expected)) }.to_string_lossy();
        let Some(state) = (unsafe { state_mut(state) }) else {
            return;
        };
        api_jump(
            state,
            LuaError::new(format!(
                "bad argument #{} ({} expected)",
                index, expected_name
            )),
        );
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkany(state: *mut lua_State, index: c_int) {
    if unsafe { lua_type(state, index) } == LUA_TNONE {
        let Some(state) = (unsafe { state_mut(state) }) else {
            return;
        };
        api_jump(
            state,
            LuaError::new(format!("bad argument #{} (value expected)", index)),
        );
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkstack(
    state: *mut lua_State,
    size: c_int,
    message: *const c_char,
) {
    if unsafe { lua_checkstack(state, size) } == 0 {
        let suffix = if message.is_null() {
            "stack overflow".into()
        } else {
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        };
        let Some(state) = (unsafe { state_mut(state) }) else {
            return;
        };
        api_jump(state, LuaError::new(suffix));
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkversion_(
    state: *mut lua_State,
    version: LuaNumber,
    sizes: usize,
) {
    let expected_sizes = std::mem::size_of::<LuaInteger>() * 16 + std::mem::size_of::<LuaNumber>();
    if version != 505.0 || sizes != expected_sizes {
        let Some(state) = (unsafe { state_mut(state) }) else {
            return;
        };
        api_jump(
            state,
            LuaError::new("Lua core and library have incompatible versions"),
        );
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_len(state: *mut lua_State, index: c_int) -> LuaInteger {
    unsafe { lua_len(state, index) };
    let length = unsafe { lua_tointegerx(state, -1, ptr::null_mut()) };
    unsafe { lua_settop(state, -2) };
    length
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_setmetatable(state: *mut lua_State, name: *const c_char) {
    unsafe { lua_getfield(state, LUA_REGISTRYINDEX, name) };
    unsafe { lua_setmetatable(state, -2) };
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_argerror(
    state: *mut lua_State,
    argument: c_int,
    message: *const c_char,
) -> c_int {
    let message = if message.is_null() {
        "invalid argument".into()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    api_jump(
        state,
        LuaError::new(format!("bad argument #{} ({})", argument, message)),
    )
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_typeerror(
    state: *mut lua_State,
    argument: c_int,
    expected: *const c_char,
) -> c_int {
    let expected = if expected.is_null() {
        "value".into()
    } else {
        unsafe { CStr::from_ptr(expected) }
            .to_string_lossy()
            .into_owned()
    };
    let actual = unsafe { lua_typename(state, lua_type(state, argument)) };
    let actual = unsafe { CStr::from_ptr(actual) }.to_string_lossy();
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    api_jump(
        state,
        LuaError::new(format!(
            "bad argument #{} ({} expected, got {})",
            argument, expected, actual
        )),
    )
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_checkoption(
    state: *mut lua_State,
    argument: c_int,
    default: *const c_char,
    options: *const *const c_char,
) -> c_int {
    let string = unsafe { luaL_optlstring(state, argument, default, ptr::null_mut()) };
    if string.is_null() || options.is_null() {
        return unsafe { luaL_argerror(state, argument, c"invalid option".as_ptr()) };
    }
    let wanted = unsafe { CStr::from_ptr(string) }.to_bytes();
    let mut index = 0;
    loop {
        let option = unsafe { *options.add(index) };
        if option.is_null() {
            return unsafe { luaL_argerror(state, argument, c"invalid option".as_ptr()) };
        }
        if unsafe { CStr::from_ptr(option) }.to_bytes() == wanted {
            return index as c_int;
        }
        index += 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn luaL_where(state: *mut lua_State, _level: c_int) {
    unsafe { lua_pushlstring(state, ptr::null(), 0) };
}

#[no_mangle]
pub unsafe extern "C" fn luaL_tolstring(
    state: *mut lua_State,
    index: c_int,
    len: *mut usize,
) -> *const c_char {
    let Some(state_ref) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let Some(value) = state_ref.value(index).cloned() else {
        return ptr::null();
    };
    let bytes = value.display_bytes();
    unsafe { lua_pushlstring(state, bytes.as_ptr().cast(), bytes.len()) };
    unsafe { lua_tolstring(state, -1, len) }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_getmetafield(
    state: *mut lua_State,
    object: c_int,
    field: *const c_char,
) -> c_int {
    let object = unsafe { lua_absindex(state, object) };
    if unsafe { lua_getmetatable(state, object) } == 0 {
        return LUA_TNIL;
    }
    let tag = unsafe { lua_getfield(state, -1, field) };
    if tag == LUA_TNIL {
        unsafe { lua_settop(state, -3) };
        return LUA_TNIL;
    }
    unsafe { lua_rotate(state, -2, 1) };
    unsafe { lua_settop(state, -2) };
    tag
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_callmeta(
    state: *mut lua_State,
    object: c_int,
    field: *const c_char,
) -> c_int {
    let object = unsafe { lua_absindex(state, object) };
    if unsafe { luaL_getmetafield(state, object, field) } == LUA_TNIL {
        return 0;
    }
    unsafe { lua_pushvalue(state, object) };
    unsafe { lua_callk(state, 1, 1, 0, None) };
    1
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_getsubtable(
    state: *mut lua_State,
    index: c_int,
    field: *const c_char,
) -> c_int {
    let index = unsafe { lua_absindex(state, index) };
    if unsafe { lua_getfield(state, index, field) } == LUA_TTABLE {
        return 1;
    }
    unsafe { lua_settop(state, -2) };
    unsafe { lua_createtable(state, 0, 0) };
    unsafe { lua_pushvalue(state, -1) };
    unsafe { lua_setfield(state, index, field) };
    0
}

#[no_mangle]
pub unsafe extern "C-unwind" fn luaL_requiref(
    state: *mut lua_State,
    module: *const c_char,
    open: Option<LuaCFunction>,
    global: c_int,
) {
    unsafe { lua_pushcclosure(state, open, 0) };
    unsafe { lua_pushstring(state, module) };
    unsafe { lua_callk(state, 1, 1, 0, None) };
    if global != 0 {
        unsafe { lua_pushvalue(state, -1) };
        unsafe { lua_setglobal(state, module) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn luaL_makeseed(state: *mut lua_State) -> u32 {
    let address = state as usize as u64;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (address ^ time ^ (time >> 32)) as u32
}

#[no_mangle]
pub unsafe extern "C" fn luaL_traceback(
    state: *mut lua_State,
    _source: *mut lua_State,
    message: *const c_char,
    _level: c_int,
) {
    let message = if message.is_null() {
        b"stack traceback:".as_slice()
    } else {
        unsafe { CStr::from_ptr(message) }.to_bytes()
    };
    let mut traceback = message.to_vec();
    traceback.extend_from_slice(b"\nstack traceback:");
    unsafe { lua_pushlstring(state, traceback.as_ptr().cast(), traceback.len()) };
}

#[no_mangle]
pub unsafe extern "C" fn luaL_fileresult(
    state: *mut lua_State,
    status: c_int,
    filename: *const c_char,
) -> c_int {
    if status != 0 {
        unsafe { lua_pushboolean(state, 1) };
        return 1;
    }
    unsafe { lua_pushnil(state) };
    let name = if filename.is_null() {
        "operation".into()
    } else {
        unsafe { CStr::from_ptr(filename) }
            .to_string_lossy()
            .into_owned()
    };
    let message = format!("{} failed", name);
    unsafe { lua_pushlstring(state, message.as_ptr().cast(), message.len()) };
    unsafe { lua_pushinteger(state, 0) };
    3
}

#[no_mangle]
pub unsafe extern "C" fn luaL_execresult(state: *mut lua_State, status: c_int) -> c_int {
    if status == 0 {
        unsafe { lua_pushboolean(state, 1) };
        unsafe { lua_pushliteral_bytes_for_exec(state, b"exit") };
        unsafe { lua_pushinteger(state, 0) };
    } else {
        unsafe { lua_pushnil(state) };
        unsafe { lua_pushliteral_bytes_for_exec(state, b"exit") };
        unsafe { lua_pushinteger(state, status as i64) };
    }
    3
}

unsafe fn lua_pushliteral_bytes_for_exec(state: *mut lua_State, bytes: &[u8]) {
    unsafe { lua_pushlstring(state, bytes.as_ptr().cast(), bytes.len()) };
}

#[no_mangle]
pub unsafe extern "C" fn luaL_ref(state: *mut lua_State, table: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return -2;
    };
    let Some(value) = state.stack.last() else {
        return -2;
    };
    if *value == LuaValue::Nil {
        state.stack.pop();
        return -1;
    }
    let reference = unsafe { state.runtime() }.c_next_reference;
    unsafe { state.runtime() }.c_next_reference += 1;
    unsafe { lua_rawseti(state, table, reference) };
    reference as c_int
}

#[no_mangle]
pub unsafe extern "C" fn luaL_unref(state: *mut lua_State, table: c_int, reference: c_int) {
    if reference < 0 {
        return;
    }
    unsafe { lua_pushnil(state) };
    unsafe { lua_rawseti(state, table, reference as LuaInteger) };
}

pub(super) unsafe extern "C" fn default_alloc(
    _user_data: *mut c_void,
    pointer: *mut c_void,
    _old_size: usize,
    new_size: usize,
) -> *mut c_void {
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut c_void;
        fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void;
        fn free(pointer: *mut c_void);
    }
    if new_size == 0 {
        if !pointer.is_null() {
            unsafe { free(pointer) };
        }
        ptr::null_mut()
    } else if pointer.is_null() {
        unsafe { malloc(new_size) }
    } else {
        unsafe { realloc(pointer, new_size) }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_getallocf(
    state: *mut lua_State,
    user_data: *mut *mut c_void,
) -> LuaAlloc {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return default_alloc;
    };
    if !user_data.is_null() {
        unsafe { *user_data = state.allocator_data };
    }
    state.allocator
}

#[no_mangle]
pub unsafe extern "C" fn lua_setallocf(
    state: *mut lua_State,
    allocator: Option<LuaAlloc>,
    user_data: *mut c_void,
) {
    let (Some(state), Some(allocator)) = (unsafe { state_mut(state) }, allocator) else {
        return;
    };
    state.allocator = allocator;
    state.allocator_data = user_data;
}
