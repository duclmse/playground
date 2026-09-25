use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static C_HOOK_CALLS: AtomicUsize = AtomicUsize::new(0);
static C_HOOK_LINES: AtomicUsize = AtomicUsize::new(0);
static C_HOOK_RETURNS: AtomicUsize = AtomicUsize::new(0);
static C_HOOK_COUNTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C-unwind" fn count_c_hook(_state: *mut lua_State, debug: *mut LuaDebug) {
    match unsafe { (*debug).event } {
        0 => C_HOOK_CALLS.fetch_add(1, Ordering::Relaxed),
        1 => C_HOOK_RETURNS.fetch_add(1, Ordering::Relaxed),
        2 => C_HOOK_LINES.fetch_add(1, Ordering::Relaxed),
        3 => C_HOOK_COUNTS.fetch_add(1, Ordering::Relaxed),
        _ => 0,
    };
}

unsafe extern "C" fn capture_warning(
    user_data: *mut c_void,
    message: *const c_char,
    to_continue: c_int,
) {
    let warnings = unsafe { &mut *user_data.cast::<Vec<(Vec<u8>, c_int)>>() };
    let message = unsafe { CStr::from_ptr(message) }.to_bytes().to_vec();
    warnings.push((message, to_continue));
}

unsafe extern "C-unwind" fn add(state: *mut lua_State) -> c_int {
    let left = unsafe { lua_tointegerx(state, 1, ptr::null_mut()) };
    let right = unsafe { lua_tointegerx(state, 2, ptr::null_mut()) };
    unsafe { lua_pushinteger(state, left + right) };
    1
}

unsafe extern "C-unwind" fn yield_seven(state: *mut lua_State) -> c_int {
    unsafe { lua_pushinteger(state, 7) };
    unsafe { lua_yieldk(state, 1, 0, None) }
}

unsafe extern "C-unwind" fn resume_with_context(
    state: *mut lua_State,
    status: c_int,
    context: LuaKContext,
) -> c_int {
    assert_eq!(status, LUA_YIELD);
    let resumed = unsafe { lua_tointegerx(state, 1, ptr::null_mut()) };
    unsafe { lua_pushinteger(state, resumed + context as LuaInteger) };
    1
}

unsafe extern "C-unwind" fn yield_with_continuation(state: *mut lua_State) -> c_int {
    unsafe { lua_pushinteger(state, 7) };
    unsafe { lua_yieldk(state, 1, 35, Some(resume_with_context)) }
}

unsafe extern "C-unwind" fn return_first_upvalue(state: *mut lua_State) -> c_int {
    unsafe { lua_pushvalue(state, LUA_REGISTRYINDEX - 1) };
    1
}

#[test]
fn c_callback_is_callable_from_loaded_lua() {
    unsafe {
        let state = luaL_newstate();
        lua_pushcclosure(state, Some(add), 0);
        lua_setglobal(state, c"add".as_ptr());
        let source = c"return add(20, 22)";
        assert_eq!(luaL_loadstring(state, source.as_ptr()), LUA_OK);
        assert_eq!(lua_pcallk(state, 0, 1, 0, 0, None), LUA_OK);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 42);
        lua_close(state);
    }
}

#[test]
fn userdata_storage_is_canonical_and_precisely_rooted() {
    unsafe {
        let state = luaL_newstate();
        let state_ref = &mut *state;
        // Native-library setup interns many rooted strings (function names,
        // table keys) into the same canonical heap, so the live-object count
        // right after `luaL_newstate()` is a large, version-dependent
        // baseline rather than a small fixed number, and it isn't collected
        // yet, so it may still include transient garbage from init. Collect
        // once up front to get a clean baseline, then assert this test's
        // userdata allocation/collection behavior as a *delta* against it.
        (&mut *state_ref.runtime)
            .canonical_heap
            .borrow_mut()
            .collect_major();
        let baseline = (&*state_ref.runtime).canonical_heap.borrow().len();
        let bytes = lua_newuserdatauv(state, 16, 0).cast::<u8>();
        assert!(!bytes.is_null());
        *bytes.add(15) = 99;
        assert_eq!(lua_touserdata(state, -1).cast::<u8>().add(15).read(), 99);
        assert_eq!(
            (&*state_ref.runtime).canonical_heap.borrow().len(),
            baseline + 1
        );
        lua_settop(state, 0);
        assert_eq!(
            (&*state_ref.runtime).canonical_heap.borrow().len(),
            baseline + 1
        );
        (&mut *state_ref.runtime)
            .canonical_heap
            .borrow_mut()
            .collect_major();
        assert_eq!((&*state_ref.runtime).canonical_heap.borrow().len(), baseline);
        lua_close(state);
    }
}

#[test]
fn registry_values_are_traced_by_the_canonical_heap() {
    unsafe {
        let state = luaL_newstate();
        lua_pushinteger(state, 42);
        lua_rawseti(state, LUA_REGISTRYINDEX, 7);
        assert_eq!(lua_rawgeti(state, LUA_REGISTRYINDEX, 7), LUA_TNUMBER);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 42);
        lua_close(state);
    }
}

#[test]
fn raw_pointer_keys_round_trip_as_light_userdata() {
    unsafe {
        let state = luaL_newstate();
        let pointer = 0x1234usize as *const c_void;
        lua_pushinteger(state, 42);
        lua_rawsetp(state, LUA_REGISTRYINDEX, pointer);

        assert_eq!(lua_rawgetp(state, LUA_REGISTRYINDEX, pointer), LUA_TNUMBER);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 42);
        lua_settop(state, 0);

        lua_pushlightuserdata(state, pointer.cast_mut());
        lua_rawget(state, LUA_REGISTRYINDEX);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 42);
        lua_close(state);
    }
}

#[test]
fn warning_callback_is_shared_by_the_runtime_state() {
    unsafe {
        let state = luaL_newstate();
        let mut warnings = Vec::new();
        lua_setwarnf(
            state,
            Some(capture_warning),
            (&mut warnings as *mut Vec<(Vec<u8>, c_int)>).cast(),
        );
        lua_warning(state, c"first ".as_ptr(), 1);
        lua_warning(state, c"second".as_ptr(), 0);
        assert_eq!(
            warnings,
            vec![(b"first ".to_vec(), 1), (b"second".to_vec(), 0)]
        );
        lua_close(state);
    }
}

#[test]
fn child_state_resumes_on_the_shared_runtime_and_moves_results() {
    unsafe {
        let main = luaL_newstate();
        let child = lua_newthread(main);
        assert!(!child.is_null());
        assert_eq!(lua_tothread(main, -1), child);
        assert_eq!(lua_pushthread(main), 1);
        assert_eq!(lua_tothread(main, -1), main);
        lua_settop(main, 0);

        assert_eq!(luaL_loadstring(child, c"return 40 + 2".as_ptr()), LUA_OK);
        let mut result_count = 0;
        assert_eq!(lua_resume(child, main, 0, &mut result_count), LUA_OK);
        assert_eq!(result_count, 1);
        assert_eq!(lua_status(child), LUA_OK);
        lua_xmove(child, main, 1);
        assert_eq!(lua_tointegerx(main, -1, ptr::null_mut()), 42);
        assert_eq!(lua_gettop(child), 0);
        lua_close(main);
    }
}

#[test]
fn child_state_preserves_lua_frames_across_yield() {
    unsafe {
        let main = luaL_newstate();
        let child = lua_newthread(main);
        lua_settop(main, 0);
        assert_eq!(
            luaL_loadstring(
                child,
                c"local value = coroutine.yield(7); return value + 1".as_ptr(),
            ),
            LUA_OK
        );
        let mut result_count = 0;
        assert_eq!(lua_resume(child, main, 0, &mut result_count), LUA_YIELD);
        assert_eq!(result_count, 1);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 7);
        assert_eq!(lua_status(child), LUA_YIELD);

        lua_settop(child, 0);
        lua_pushinteger(child, 41);
        assert_eq!(lua_resume(child, main, 1, &mut result_count), LUA_OK);
        assert_eq!(result_count, 1);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 42);
        lua_close(main);
    }
}

#[test]
fn c_callback_can_yield_from_a_resumed_thread() {
    unsafe {
        let main = luaL_newstate();
        lua_pushcclosure(main, Some(yield_seven), 0);
        lua_setglobal(main, c"cyield".as_ptr());
        let child = lua_newthread(main);
        lua_settop(main, 0);
        assert_eq!(luaL_loadstring(child, c"return cyield()".as_ptr()), LUA_OK);

        let mut result_count = 0;
        assert_eq!(lua_resume(child, main, 0, &mut result_count), LUA_YIELD);
        assert_eq!(result_count, 1);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 7);

        lua_settop(child, 0);
        lua_pushinteger(child, 42);
        assert_eq!(lua_resume(child, main, 1, &mut result_count), LUA_OK);
        assert_eq!(result_count, 1);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 42);
        lua_close(main);
    }
}

#[test]
fn c_yield_continuation_runs_before_lua_frames_resume() {
    unsafe {
        let main = luaL_newstate();
        lua_pushcclosure(main, Some(yield_with_continuation), 0);
        lua_setglobal(main, c"cyield".as_ptr());
        let child = lua_newthread(main);
        lua_settop(main, 0);
        assert_eq!(luaL_loadstring(child, c"return cyield()".as_ptr()), LUA_OK);

        let mut result_count = 0;
        assert_eq!(lua_resume(child, main, 0, &mut result_count), LUA_YIELD);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 7);
        lua_settop(child, 0);
        lua_pushinteger(child, 5);
        assert_eq!(lua_resume(child, main, 1, &mut result_count), LUA_OK);
        assert_eq!(result_count, 1);
        assert_eq!(lua_tointegerx(child, -1, ptr::null_mut()), 40);
        lua_close(main);
    }
}

#[test]
fn c_closure_upvalues_are_canonical_and_mutable_through_the_api() {
    unsafe {
        let state = luaL_newstate();
        lua_pushinteger(state, 10);
        lua_pushcclosure(state, Some(return_first_upvalue), 1);
        let identity = lua_upvalueid(state, -1, 1);
        assert!(!identity.is_null());
        assert_eq!(identity, lua_upvalueid(state, -1, 1));
        let name = lua_getupvalue(state, -1, 1);
        assert!(!name.is_null());
        assert!(CStr::from_ptr(name).to_bytes().is_empty());
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 10);
        lua_settop(state, 1);

        lua_pushinteger(state, 32);
        let name = lua_setupvalue(state, 1, 1);
        assert!(!name.is_null());
        assert!(CStr::from_ptr(name).to_bytes().is_empty());
        assert_eq!(lua_gettop(state), 1);
        lua_pushvalue(state, 1);
        lua_callk(state, 0, 1, 0, None);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 32);
        lua_close(state);
    }
}

#[test]
fn c_debug_hook_observes_resumed_lua_execution() {
    unsafe {
        C_HOOK_CALLS.store(0, Ordering::Relaxed);
        C_HOOK_LINES.store(0, Ordering::Relaxed);
        C_HOOK_RETURNS.store(0, Ordering::Relaxed);
        C_HOOK_COUNTS.store(0, Ordering::Relaxed);
        let main = luaL_newstate();
        let child = lua_newthread(main);
        lua_settop(main, 0);
        lua_sethook(
            child,
            Some(count_c_hook),
            (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3),
            1,
        );
        assert_eq!(
            lua_gethook(child).map(|hook| hook as *const () as usize),
            Some(count_c_hook as *const () as usize)
        );
        assert_eq!(lua_gethookcount(child), 1);
        assert_eq!(
            luaL_loadstring(child, c"local x = 1; x = x + 1; return x".as_ptr()),
            LUA_OK
        );
        let mut results = 0;
        assert_eq!(lua_resume(child, main, 0, &mut results), LUA_OK);
        assert!(C_HOOK_CALLS.load(Ordering::Relaxed) > 0);
        assert!(C_HOOK_LINES.load(Ordering::Relaxed) > 0);
        assert!(C_HOOK_RETURNS.load(Ordering::Relaxed) > 0);
        assert!(C_HOOK_COUNTS.load(Ordering::Relaxed) > 0);
        lua_close(main);
    }
}

#[test]
fn c_to_close_slot_runs_close_metamethod_when_popped() {
    unsafe {
        let state = luaL_newstate();
        assert_eq!(
            luaL_loadstring(
                state,
                c"closed = 0; return setmetatable({}, {__close=function() closed=closed+1 end})"
                    .as_ptr(),
            ),
            LUA_OK
        );
        assert_eq!(lua_pcallk(state, 0, 1, 0, 0, None), LUA_OK);
        lua_toclose(state, -1);
        lua_settop(state, 0);
        assert_eq!(lua_getglobal(state, c"closed".as_ptr()), LUA_TNUMBER);
        assert_eq!(lua_tointegerx(state, -1, ptr::null_mut()), 1);
        lua_close(state);
    }
}
