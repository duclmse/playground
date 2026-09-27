//! Lua 5.5 source-compatible embedding surface.
//!
//! The exported state is intentionally opaque to C. Each state owns one
//! `LuaRuntime`; callback invocations borrow that runtime through a temporary
//! stack view, so C functions called from Lua observe ordinary Lua stack
//! semantics without placing process pointers in the managed value graph.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void, CStr};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::ptr;
use std::rc::Rc;

use super::frame::LuaFrame;
use super::*;
use crate::ast::BinaryOp;

#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), link(name = "dl"))]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

pub(super) struct NativeLibrary {
    #[cfg(unix)]
    handle: *mut c_void,
}

impl Drop for NativeLibrary {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.handle.is_null() {
            unsafe { dlclose(self.handle) };
        }
    }
}

pub(super) fn load_native_callable(
    path: &[u8],
    symbol: &[u8],
) -> Result<(NativeLibrary, LuaCFunction), String> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let path = CString::new(path).map_err(|_| "native module path contains NUL".to_string())?;
        let symbol =
            CString::new(symbol).map_err(|_| "native module symbol contains NUL".to_string())?;
        let handle = unsafe { dlopen(path.as_ptr(), dlopen_flags()) };
        if handle.is_null() {
            return Err(dl_error("cannot open shared library"));
        }
        let address = unsafe { dlsym(handle, symbol.as_ptr()) };
        if address.is_null() {
            let error = dl_error("symbol not found");
            unsafe { dlclose(handle) };
            return Err(error);
        }
        let function = unsafe { std::mem::transmute::<*mut c_void, LuaCFunction>(address) };
        Ok((NativeLibrary { handle }, function))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, symbol);
        Err("dynamic libraries are not implemented on this platform".to_string())
    }
}

pub(super) fn load_native_library(path: &[u8]) -> Result<NativeLibrary, String> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let path = CString::new(path).map_err(|_| "native module path contains NUL".to_string())?;
        let handle = unsafe { dlopen(path.as_ptr(), dlopen_flags()) };
        if handle.is_null() {
            Err(dl_error("cannot open shared library"))
        } else {
            Ok(NativeLibrary { handle })
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("dynamic libraries are not implemented on this platform".to_string())
    }
}

fn active_c_state(runtime: &LuaRuntime) -> Option<*mut lua_State> {
    let thread = runtime
        .coroutine_stack
        .last()
        .copied()
        .unwrap_or(runtime.main_coroutine);
    let identity = thread.object_id().raw();
    runtime.c_thread_states.borrow().get(&identity).copied()
}

fn invoke_c_hook(runtime: &mut LuaRuntime, event: c_int, current_line: c_int) -> LuaResult<()> {
    let Some(state_pointer) = active_c_state(runtime) else {
        return Ok(());
    };
    let state = unsafe { &mut *state_pointer };
    let Some(hook) = state.hook else {
        return Ok(());
    };
    if state.hook_running {
        return Ok(());
    }
    let mut debug: LuaDebug = unsafe { std::mem::zeroed() };
    debug.event = event;
    debug.currentline = current_line;
    debug.i_ci = std::ptr::dangling_mut::<c_void>();
    state.hook_running = true;
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        hook(state_pointer, &mut debug)
    }));
    state.hook_running = false;
    match result {
        Ok(()) => Ok(()),
        Err(payload) => match payload.downcast::<CApiJump>() {
            Ok(_) => Err(state
                .pending_error
                .take()
                .unwrap_or_else(|| LuaError::new("C hook error"))),
            Err(payload) => std::panic::resume_unwind(payload),
        },
    }
}

pub(super) fn fire_c_hook(
    runtime: &mut LuaRuntime,
    event: &'static str,
    line: Option<i64>,
) -> LuaResult<()> {
    let (event, mask) = match event {
        "call" => (0, 1 << 0),
        "return" => (1, 1 << 1),
        "line" => (2, 1 << 2),
        "count" => (3, 1 << 3),
        "tail call" => (4, 1 << 0),
        _ => return Ok(()),
    };
    let interested =
        active_c_state(runtime).is_some_and(|state| unsafe { (&*state).hook_mask & mask != 0 });
    if interested {
        invoke_c_hook(runtime, event, line.unwrap_or(-1) as c_int)?;
    }
    Ok(())
}

pub(super) fn fire_c_instruction_hooks(
    runtime: &mut LuaRuntime,
    frame: &mut LuaFrame,
    pc: usize,
) -> LuaResult<()> {
    let Some(state_pointer) = active_c_state(runtime) else {
        return Ok(());
    };
    let mut fire_count = false;
    let mut line = None;
    unsafe {
        let state = &mut *state_pointer;
        if state.hook_mask & (1 << 3) != 0 && state.hook_count > 0 {
            state.hook_remaining -= 1;
            if state.hook_remaining <= 0 {
                state.hook_remaining = state.hook_count;
                fire_count = true;
            }
        }
        if state.hook_mask & (1 << 2) != 0 {
            if let Some(location) = frame.proto.source_map.location(pc as u32) {
                let current_line = location.line as i64;
                let current_pc = pc as i64;
                if current_line != frame.c_hook_last_line || current_pc <= frame.c_hook_last_pc {
                    frame.c_hook_last_line = current_line;
                    frame.c_hook_last_pc = current_pc;
                    line = Some(current_line);
                }
            }
        }
    }
    if fire_count {
        invoke_c_hook(runtime, 3, -1)?;
    }
    if let Some(line) = line {
        invoke_c_hook(runtime, 2, line as c_int)?;
    }
    Ok(())
}

#[cfg(unix)]
const fn dlopen_flags() -> c_int {
    // RTLD_NOW plus RTLD_GLOBAL. Lua exposes symbols from previously loaded
    // C modules to later modules (the upstream lib1/lib11 and lib2/lib21
    // fixtures depend on this), so local visibility is not compatible.
    #[cfg(target_os = "macos")]
    {
        0x2 | 0x8
    }
    #[cfg(not(target_os = "macos"))]
    {
        0x2 | 0x100
    }
}

#[cfg(unix)]
fn dl_error(fallback: &str) -> String {
    let error = unsafe { dlerror() };
    if error.is_null() {
        fallback.to_string()
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

pub const LUA_OK: c_int = 0;
pub const LUA_YIELD: c_int = 1;
pub const LUA_ERRRUN: c_int = 2;
pub const LUA_ERRSYNTAX: c_int = 3;
pub const LUA_ERRMEM: c_int = 4;
pub const LUA_ERRERR: c_int = 5;
pub const LUA_MULTRET: c_int = -1;
pub const LUA_REGISTRYINDEX: c_int = -1_001_000;

pub const LUA_TNONE: c_int = -1;
pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TLIGHTUSERDATA: c_int = 2;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;
pub const LUA_TFUNCTION: c_int = 6;
pub const LUA_TUSERDATA: c_int = 7;
pub const LUA_TTHREAD: c_int = 8;

pub type LuaInteger = i64;
pub type LuaNumber = f64;
pub type LuaUnsigned = u64;
pub type LuaKContext = isize;
pub type LuaCFunction = unsafe extern "C-unwind" fn(*mut lua_State) -> c_int;
pub type LuaKFunction = unsafe extern "C-unwind" fn(*mut lua_State, c_int, LuaKContext) -> c_int;
pub type LuaAlloc = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void;
pub type LuaReader = unsafe extern "C" fn(*mut lua_State, *mut c_void, *mut usize) -> *const c_char;
pub type LuaWriter =
    unsafe extern "C" fn(*mut lua_State, *const c_void, usize, *mut c_void) -> c_int;
pub type LuaWarnFunction = unsafe extern "C" fn(*mut c_void, *const c_char, c_int);
pub type LuaHook = unsafe extern "C-unwind" fn(*mut lua_State, *mut LuaDebug);

#[repr(C)]
#[allow(non_snake_case)]
pub struct LuaDebug {
    pub event: c_int,
    pub name: *const c_char,
    pub namewhat: *const c_char,
    pub what_: *const c_char,
    pub source: *const c_char,
    pub srclen: usize,
    pub currentline: c_int,
    pub linedefined: c_int,
    pub lastlinedefined: c_int,
    pub nups: u8,
    pub nparams: u8,
    pub isvararg: c_char,
    pub extraargs: u8,
    pub istailcall: c_char,
    pub ftransfer: c_int,
    pub ntransfer: c_int,
    pub short_src: [c_char; 60],
    pub i_ci: *mut c_void,
}

/// Opaque in the public C header. Rust keeps the concrete layout private to
/// this module even though the symbol name must match Lua's API.
#[allow(non_camel_case_types)]
pub struct lua_State {
    runtime: *mut LuaRuntime,
    _owned_runtime: Option<Box<LuaRuntime>>,
    stack: Vec<LuaValue>,
    upvalues: Vec<LuaValue>,
    /// NUL-terminated views returned to C. Lua strings remain byte-exact;
    /// these buffers only satisfy the C API's pointer contract.
    c_strings: Vec<Box<[u8]>>,
    pending_error: Option<LuaError>,
    allocator: LuaAlloc,
    allocator_data: *mut c_void,
    panic_function: Option<LuaCFunction>,
    thread: ThreadRef,
    status: c_int,
    to_close: Vec<usize>,
    hook: Option<LuaHook>,
    hook_mask: c_int,
    hook_count: c_int,
    hook_remaining: c_int,
    hook_running: bool,
}

#[derive(Debug)]
struct CApiJump;

#[derive(Debug)]
struct CApiYield {
    result_count: usize,
    continuation: Option<CContinuation>,
}

pub(super) enum CApiOutcome {
    Returned(Vec<LuaValue>),
    Yielded(Vec<LuaValue>),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CContinuation {
    function: LuaKFunction,
    context: LuaKContext,
}

impl lua_State {
    fn owned(allocator: LuaAlloc, allocator_data: *mut c_void) -> Box<Self> {
        let mut runtime = Box::new(LuaRuntime::new());
        let runtime_ptr = runtime.as_mut() as *mut LuaRuntime;
        let thread = runtime.main_coroutine;
        Box::new(Self {
            runtime: runtime_ptr,
            _owned_runtime: Some(runtime),
            stack: Vec::new(),
            upvalues: Vec::new(),
            c_strings: Vec::new(),
            pending_error: None,
            allocator,
            allocator_data,
            panic_function: None,
            thread,
            status: LUA_OK,
            to_close: Vec::new(),
            hook: None,
            hook_mask: 0,
            hook_count: 0,
            hook_remaining: 0,
            hook_running: false,
        })
    }

    fn callback(runtime: &mut LuaRuntime, stack: Vec<LuaValue>, upvalues: Vec<LuaValue>) -> Self {
        let thread = runtime
            .coroutine_stack
            .last()
            .cloned()
            .unwrap_or(runtime.main_coroutine);
        Self {
            runtime,
            _owned_runtime: None,
            stack,
            upvalues,
            c_strings: Vec::new(),
            pending_error: None,
            allocator: default_alloc,
            allocator_data: ptr::null_mut(),
            panic_function: None,
            thread,
            status: LUA_OK,
            to_close: Vec::new(),
            hook: None,
            hook_mask: 0,
            hook_count: 0,
            hook_remaining: 0,
            hook_running: false,
        }
    }

    fn child(parent: &lua_State, thread: ThreadRef) -> Box<Self> {
        Box::new(Self {
            runtime: parent.runtime,
            _owned_runtime: None,
            stack: Vec::new(),
            upvalues: Vec::new(),
            c_strings: Vec::new(),
            pending_error: None,
            allocator: parent.allocator,
            allocator_data: parent.allocator_data,
            panic_function: parent.panic_function,
            thread,
            status: LUA_OK,
            to_close: Vec::new(),
            hook: None,
            hook_mask: 0,
            hook_count: 0,
            hook_remaining: 0,
            hook_running: false,
        })
    }

    unsafe fn runtime(&mut self) -> &mut LuaRuntime {
        unsafe { &mut *self.runtime }
    }

    fn absolute(&self, index: c_int) -> Option<usize> {
        if index > 0 {
            let offset = index as usize - 1;
            (offset < self.stack.len()).then_some(offset)
        } else if index < 0 && index > LUA_REGISTRYINDEX {
            let distance = (-index) as usize;
            self.stack.len().checked_sub(distance)
        } else {
            None
        }
    }

    fn value(&self, index: c_int) -> Option<&LuaValue> {
        if index < LUA_REGISTRYINDEX {
            return self.upvalues.get((LUA_REGISTRYINDEX - index - 1) as usize);
        }
        self.absolute(index).and_then(|index| self.stack.get(index))
    }

    fn push_error(&mut self, error: LuaError) -> c_int {
        let heap = unsafe { self.runtime() }.canonical_heap.clone();
        self.stack.push(error.clone().into_lua_value(&heap));
        self.pending_error = Some(error);
        LUA_ERRRUN
    }

    // Named to mirror `lua_to_canonical`/`canonical_to_lua` below, not the
    // Rust `to_*`/`from_*` self-type conventions clippy expects here.
    #[allow(clippy::wrong_self_convention)]
    fn to_canonical(&mut self, value: &LuaValue) -> LuaResult<sol_core::Value> {
        lua_to_canonical(unsafe { self.runtime() }, value)
    }

    #[allow(clippy::wrong_self_convention)]
    fn from_canonical(&mut self, value: sol_core::Value) -> LuaResult<LuaValue> {
        canonical_to_lua(unsafe { self.runtime() }, value)
    }

    fn registry_get(&mut self, key: sol_core::Value) -> LuaResult<LuaValue> {
        let runtime = unsafe { self.runtime() };
        let value = runtime
            .canonical_heap
            .borrow()
            .table_get(runtime.c_registry, key)
            .map_err(|error| LuaError::new(error.to_string()))?;
        self.from_canonical(value)
    }

    fn registry_set(&mut self, key: sol_core::Value, value: &LuaValue) -> LuaResult<()> {
        let value = self.to_canonical(value)?;
        let runtime = unsafe { self.runtime() };
        runtime
            .canonical_heap
            .borrow_mut()
            .table_set(runtime.c_registry, key, value)
            .map_err(|error| LuaError::new(error.to_string()))
    }
}

pub(super) fn invoke(
    runtime: &mut LuaRuntime,
    function: LuaCFunction,
    args: Vec<LuaValue>,
    callable: sol_core::ObjectId,
) -> LuaResult<CApiOutcome> {
    let captures = {
        let heap = runtime.canonical_heap.borrow();
        let sol_core::HeapObject::NativeCallable(callable) = heap
            .object(callable)
            .map_err(|error| LuaError::new(error.to_string()))?
        else {
            return Err(LuaError::new(
                "C function handle has the wrong canonical kind",
            ));
        };
        callable.captures.clone()
    };
    let upvalues = captures
        .into_iter()
        .map(|value| canonical_to_lua(runtime, value))
        .collect::<LuaResult<Vec<_>>>()?;
    let mut state = lua_State::callback(runtime, args, upvalues);
    let result_count = match catch_unwind(AssertUnwindSafe(|| unsafe { function(&mut state) })) {
        Ok(result_count) => result_count,
        Err(payload) => match payload.downcast::<CApiJump>() {
            Ok(_) => {
                return Err(state
                    .pending_error
                    .take()
                    .unwrap_or_else(|| LuaError::new("C API error")))
            }
            Err(payload) => match payload.downcast::<CApiYield>() {
                Ok(payload) => {
                    if payload.result_count > state.stack.len() {
                        return Err(LuaError::new("invalid C yield result count"));
                    }
                    if let Some(continuation) = payload.continuation {
                        let identity = state.thread.object_id().raw();
                        unsafe { state.runtime() }
                            .c_continuations
                            .borrow_mut()
                            .insert(identity, continuation);
                    }
                    let values = state
                        .stack
                        .split_off(state.stack.len() - payload.result_count);
                    return Ok(CApiOutcome::Yielded(values));
                }
                Err(payload) => std::panic::resume_unwind(payload),
            },
        },
    };
    if let Some(error) = state.pending_error {
        return Err(error);
    }
    if result_count < 0 || result_count as usize > state.stack.len() {
        return Err(LuaError::new("C function returned an invalid result count"));
    }
    Ok(CApiOutcome::Returned(
        state
            .stack
            .split_off(state.stack.len() - result_count as usize),
    ))
}

fn invoke_continuation(
    runtime: &mut LuaRuntime,
    thread: ThreadRef,
    continuation: CContinuation,
    args: Vec<LuaValue>,
) -> LuaResult<CApiOutcome> {
    let mut state = lua_State::callback(runtime, args, Vec::new());
    state.thread = thread;
    let result_count = match catch_unwind(AssertUnwindSafe(|| unsafe {
        (continuation.function)(&mut state, LUA_YIELD, continuation.context)
    })) {
        Ok(result_count) => result_count,
        Err(payload) => match payload.downcast::<CApiJump>() {
            Ok(_) => {
                return Err(state
                    .pending_error
                    .take()
                    .unwrap_or_else(|| LuaError::new("C continuation error")))
            }
            Err(payload) => match payload.downcast::<CApiYield>() {
                Ok(payload) => {
                    if payload.result_count > state.stack.len() {
                        return Err(LuaError::new("invalid C yield result count"));
                    }
                    if let Some(next) = payload.continuation {
                        let identity = state.thread.object_id().raw();
                        unsafe { state.runtime() }
                            .c_continuations
                            .borrow_mut()
                            .insert(identity, next);
                    }
                    let values = state
                        .stack
                        .split_off(state.stack.len() - payload.result_count);
                    return Ok(CApiOutcome::Yielded(values));
                }
                Err(payload) => std::panic::resume_unwind(payload),
            },
        },
    };
    if result_count < 0 || result_count as usize > state.stack.len() {
        return Err(LuaError::new(
            "C continuation returned an invalid result count",
        ));
    }
    Ok(CApiOutcome::Returned(
        state
            .stack
            .split_off(state.stack.len() - result_count as usize),
    ))
}

pub(super) fn canonical_to_lua(
    runtime: &LuaRuntime,
    value: sol_core::Value,
) -> LuaResult<LuaValue> {
    if value == sol_core::Value::NIL {
        return Ok(LuaValue::Nil);
    }
    if let Some(value) = value.as_bool() {
        return Ok(LuaValue::Bool(value));
    }
    if let Some(value) = value.as_integer() {
        return Ok(LuaValue::Integer(value));
    }
    if let Some(value) = value.as_float() {
        return Ok(LuaValue::Float(value));
    }
    let object = value
        .as_object()
        .ok_or_else(|| LuaError::new("invalid canonical value"))?;
    if let Some(pointer) = runtime.c_light_userdata_reverse.borrow().get(&object) {
        return Ok(LuaValue::LightUserdata(*pointer));
    }
    let heap_ref = runtime.canonical_heap.clone();
    let heap = heap_ref.borrow();
    match heap
        .object(object)
        .map_err(|error| LuaError::new(error.to_string()))?
    {
        sol_core::HeapObject::String(_) => {
            drop(heap);
            Ok(LuaValue::String(CanonicalString::root_existing(
                heap_ref, object,
            )))
        }
        sol_core::HeapObject::Table(_) => {
            drop(heap);
            Ok(LuaValue::CanonicalTable(CanonicalTable::root_existing(
                heap_ref, object,
            )))
        }
        sol_core::HeapObject::Userdata(_) => {
            drop(heap);
            Ok(LuaValue::Userdata(CanonicalUserdata::root_existing(
                heap_ref, object,
            )))
        }
        sol_core::HeapObject::NativeCallable(_) => {
            drop(heap);
            Ok(LuaValue::CFunction(CanonicalCFunction::root_existing(
                heap_ref, object,
            )))
        }
        _ => Err(LuaError::new("canonical object is not exposed to Lua yet")),
    }
}

pub(super) fn lua_to_canonical(
    runtime: &LuaRuntime,
    value: &LuaValue,
) -> LuaResult<sol_core::Value> {
    if let LuaValue::LightUserdata(pointer) = value {
        if let Some(object) = runtime.c_light_userdata.borrow().get(pointer).copied() {
            return Ok(sol_core::Value::object(object));
        }
        let object = runtime
            .canonical_heap
            .borrow_mut()
            .alloc_userdata(*pointer as u64);
        runtime
            .c_light_userdata
            .borrow_mut()
            .insert(*pointer, object);
        runtime
            .c_light_userdata_reverse
            .borrow_mut()
            .insert(object, *pointer);
        return Ok(sol_core::Value::object(object));
    }
    Ok(match value {
        LuaValue::Nil => sol_core::Value::NIL,
        LuaValue::Bool(value) => sol_core::Value::boolean(*value),
        LuaValue::Integer(value) => sol_core::Value::integer(*value),
        LuaValue::Float(value) => sol_core::Value::float(*value),
        LuaValue::String(value) => sol_core::Value::object(value.object_id()),
        LuaValue::CanonicalTable(value) => sol_core::Value::object(value.object_id()),
        LuaValue::Userdata(value) => sol_core::Value::object(value.object_id()),
        LuaValue::CFunction(value) => sol_core::Value::object(value.object_id()),
        _ => {
            return Err(LuaError::new(
                "value is not represented on the canonical heap yet",
            ))
        }
    })
}

pub(super) fn canonical_table_get(
    runtime: &LuaRuntime,
    table: sol_core::ObjectId,
    key: &LuaValue,
) -> LuaResult<LuaValue> {
    let key = lua_to_canonical(runtime, key)?;
    let value = runtime
        .canonical_heap
        .borrow()
        .table_get(table, key)
        .map_err(|error| LuaError::new(error.to_string()))?;
    canonical_to_lua(runtime, value)
}

pub(super) fn canonical_metamethod(
    runtime: &LuaRuntime,
    object: sol_core::ObjectId,
    name: &[u8],
) -> LuaResult<Option<LuaValue>> {
    let metatable = {
        let heap = runtime.canonical_heap.borrow();
        match heap
            .object(object)
            .map_err(|error| LuaError::new(error.to_string()))?
        {
            sol_core::HeapObject::Table(value) => value.metatable,
            sol_core::HeapObject::Userdata(value) => value.metatable,
            _ => None,
        }
    };
    let Some(metatable) = metatable else {
        return Ok(None);
    };
    let key = LuaValue::String(runtime.intern_str(name));
    let value = canonical_table_get(runtime, metatable, &key)?;
    Ok((value != LuaValue::Nil).then_some(value))
}

unsafe fn state_mut<'a>(state: *mut lua_State) -> Option<&'a mut lua_State> {
    unsafe { state.as_mut() }
}

unsafe extern "C" {
    fn sol_c_api_shim_anchor();
}

fn register_owned_state(state: Box<lua_State>) -> *mut lua_State {
    let state = Box::into_raw(state);
    unsafe {
        let state_ref = &mut *state;
        let identity = state_ref.thread.object_id().raw();
        (&mut *state_ref.runtime)
            .c_thread_states
            .borrow_mut()
            .insert(identity, state);
    }
    state
}

#[no_mangle]
pub unsafe extern "C" fn luaL_newstate() -> *mut lua_State {
    unsafe { sol_c_api_shim_anchor() };
    register_owned_state(lua_State::owned(default_alloc, ptr::null_mut()))
}

#[no_mangle]
pub unsafe extern "C" fn lua_newstate(
    allocator: Option<LuaAlloc>,
    allocator_data: *mut c_void,
    _seed: u32,
) -> *mut lua_State {
    let Some(allocator) = allocator else {
        return ptr::null_mut();
    };
    unsafe { sol_c_api_shim_anchor() };
    register_owned_state(lua_State::owned(allocator, allocator_data))
}

#[no_mangle]
pub unsafe extern "C" fn lua_version(_state: *mut lua_State) -> LuaNumber {
    505.0
}

#[no_mangle]
pub unsafe extern "C" fn lua_atpanic(
    state: *mut lua_State,
    panic_function: Option<LuaCFunction>,
) -> Option<LuaCFunction> {
    let state = unsafe { state_mut(state) }?;
    std::mem::replace(&mut state.panic_function, panic_function)
}

#[no_mangle]
pub unsafe extern "C" fn lua_setwarnf(
    state: *mut lua_State,
    warning_function: Option<LuaWarnFunction>,
    user_data: *mut c_void,
) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let runtime = unsafe { state.runtime() };
    runtime.c_warning_function = warning_function;
    runtime.c_warning_data = user_data;
}

#[no_mangle]
pub unsafe extern "C" fn lua_warning(
    state: *mut lua_State,
    message: *const c_char,
    to_continue: c_int,
) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let runtime = unsafe { state.runtime() };
    if let Some(warning_function) = runtime.c_warning_function {
        unsafe { warning_function(runtime.c_warning_data, message, to_continue) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_close(state: *mut lua_State) {
    let Some(state_ref) = (unsafe { state.as_mut() }) else {
        return;
    };
    if state_ref._owned_runtime.is_none() {
        return;
    }
    let children = unsafe { &mut *state_ref.runtime }
        .c_thread_states
        .borrow_mut()
        .drain()
        .map(|(_, child)| child)
        .filter(|child| *child != state)
        .collect::<Vec<_>>();
    for child in children {
        unsafe { drop(Box::from_raw(child)) };
    }
    unsafe { drop(Box::from_raw(state)) };
}

#[no_mangle]
pub unsafe extern "C" fn luaL_openlibs(state: *mut lua_State) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    // Creating a state is deny-by-default. Calling `luaL_openlibs` is the
    // embedder's explicit opt-in to the native standard-library profile.
    let runtime = unsafe { state.runtime() };
    runtime.capabilities = Capabilities::NATIVE_CLI;
    runtime.canonical_heap.borrow_mut().capabilities = Capabilities::NATIVE_CLI;
}

unsafe fn push_library(state: *mut lua_State, name: &str) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let runtime = unsafe { state.runtime() };
    let value = if name.is_empty() {
        runtime.globals.as_value()
    } else {
        runtime.globals.get(runtime, name)
    };
    state.stack.push(value);
    1
}

#[no_mangle]
pub unsafe extern "C" fn luaopen_base(state: *mut lua_State) -> c_int {
    unsafe { push_library(state, "") }
}

macro_rules! library_opener {
    ($rust:ident, $name:literal) => {
        #[no_mangle]
        pub unsafe extern "C" fn $rust(state: *mut lua_State) -> c_int {
            unsafe { push_library(state, $name) }
        }
    };
}

library_opener!(luaopen_coroutine, "coroutine");
library_opener!(luaopen_table, "table");
library_opener!(luaopen_io, "io");
library_opener!(luaopen_os, "os");
library_opener!(luaopen_string, "string");
library_opener!(luaopen_math, "math");
library_opener!(luaopen_utf8, "utf8");
library_opener!(luaopen_debug, "debug");
library_opener!(luaopen_package, "package");

#[no_mangle]
pub unsafe extern "C" fn luaL_openselectedlibs(
    state: *mut lua_State,
    _load: c_int,
    _preload: c_int,
) {
    unsafe { luaL_openlibs(state) };
}

#[no_mangle]
pub unsafe extern "C" fn lua_gettop(state: *mut lua_State) -> c_int {
    unsafe { state_mut(state) }
        .map(|state| state.stack.len() as c_int)
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn lua_absindex(state: *mut lua_State, index: c_int) -> c_int {
    if index > 0 || index <= LUA_REGISTRYINDEX {
        index
    } else {
        (unsafe { lua_gettop(state) }) + index + 1
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_checkstack(_state: *mut lua_State, _extra: c_int) -> c_int {
    1
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_settop(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let new_len = if index >= 0 {
        index as usize
    } else {
        state.stack.len().saturating_sub((-index - 1) as usize)
    };
    let mut closing = state
        .to_close
        .iter()
        .copied()
        .filter(|slot| *slot >= new_len)
        .collect::<Vec<_>>();
    closing.sort_unstable_by(|left, right| right.cmp(left));
    for slot in closing {
        if let Err(error) = close_c_stack_slot(state, slot) {
            api_jump(state, error);
        }
    }
    state.to_close.retain(|slot| *slot < new_len);
    state.stack.resize(new_len, LuaValue::Nil);
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_toclose(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let Some(slot) = state.absolute(index) else {
        api_jump(state, LuaError::new("invalid stack index for lua_toclose"));
    };
    let value = state.stack[slot].clone();
    if !matches!(value, LuaValue::Nil | LuaValue::Bool(false)) {
        let method = match unsafe { state.runtime() }.metamethod(&value, b"__close") {
            Ok(method) => method,
            Err(error) => api_jump(state, error),
        };
        if method.is_none() {
            api_jump(state, LuaError::new("value is not closable"));
        }
    }
    if !state.to_close.contains(&slot) {
        state.to_close.push(slot);
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_closeslot(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let Some(slot) = state.absolute(index) else {
        return;
    };
    let Some(position) = state.to_close.iter().position(|marked| *marked == slot) else {
        return;
    };
    state.to_close.remove(position);
    if let Err(error) = close_c_stack_slot(state, slot) {
        api_jump(state, error);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushvalue(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    if let Some(value) = state.value(index).cloned() {
        state.stack.push(value);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_rotate(state: *mut lua_State, index: c_int, n: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let Some(start) = state.absolute(index) else {
        return;
    };
    let len = state.stack.len() - start;
    if len == 0 {
        return;
    }
    let right = n.rem_euclid(len as c_int) as usize;
    state.stack[start..].rotate_right(right);
}

#[no_mangle]
pub unsafe extern "C" fn lua_copy(state: *mut lua_State, from: c_int, to: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let (Some(from), Some(to)) = (state.absolute(from), state.absolute(to)) else {
        return;
    };
    state.stack[to] = state.stack[from].clone();
}

#[no_mangle]
pub unsafe extern "C" fn lua_type(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_TNONE;
    };
    match state.value(index) {
        None => LUA_TNONE,
        Some(LuaValue::Nil) => LUA_TNIL,
        Some(LuaValue::Bool(_)) => LUA_TBOOLEAN,
        Some(LuaValue::LightUserdata(_)) => LUA_TLIGHTUSERDATA,
        Some(LuaValue::Integer(_) | LuaValue::Float(_)) => LUA_TNUMBER,
        Some(LuaValue::String(_)) => LUA_TSTRING,
        Some(LuaValue::Table(_) | LuaValue::CanonicalTable(_)) => LUA_TTABLE,
        Some(LuaValue::Userdata(_)) => LUA_TUSERDATA,
        Some(LuaValue::Thread(_)) => LUA_TTHREAD,
        Some(_) => LUA_TFUNCTION,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_typename(_state: *mut lua_State, tag: c_int) -> *const c_char {
    match tag {
        LUA_TNIL => c"nil".as_ptr(),
        LUA_TBOOLEAN => c"boolean".as_ptr(),
        LUA_TLIGHTUSERDATA | LUA_TUSERDATA => c"userdata".as_ptr(),
        LUA_TNUMBER => c"number".as_ptr(),
        LUA_TSTRING => c"string".as_ptr(),
        LUA_TTABLE => c"table".as_ptr(),
        LUA_TFUNCTION => c"function".as_ptr(),
        LUA_TTHREAD => c"thread".as_ptr(),
        _ => c"no value".as_ptr(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushnil(state: *mut lua_State) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.stack.push(LuaValue::Nil);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushboolean(state: *mut lua_State, value: c_int) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.stack.push(LuaValue::Bool(value != 0));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushinteger(state: *mut lua_State, value: LuaInteger) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.stack.push(LuaValue::Integer(value));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushnumber(state: *mut lua_State, value: LuaNumber) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.stack.push(LuaValue::Float(value));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushlightuserdata(state: *mut lua_State, value: *mut c_void) {
    if let Some(state) = unsafe { state_mut(state) } {
        state.stack.push(LuaValue::LightUserdata(value as usize));
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushlstring(
    state: *mut lua_State,
    bytes: *const c_char,
    len: usize,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    if bytes.is_null() && len != 0 {
        return ptr::null();
    }
    let slice = if len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), len) }
    };
    let value = unsafe { state.runtime() }.fresh_str(slice);
    state.stack.push(LuaValue::String(value));
    let mut terminated = slice.to_vec();
    terminated.push(0);
    state.c_strings.push(terminated.into_boxed_slice());
    state.c_strings.last().unwrap().as_ptr().cast()
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushstring(
    state: *mut lua_State,
    bytes: *const c_char,
) -> *const c_char {
    if bytes.is_null() {
        unsafe { lua_pushnil(state) };
        return ptr::null();
    }
    let len = unsafe { CStr::from_ptr(bytes) }.to_bytes().len();
    unsafe { lua_pushlstring(state, bytes, len) }
}

#[no_mangle]
pub unsafe extern "C" fn lua_toboolean(state: *mut lua_State, index: c_int) -> c_int {
    unsafe { state_mut(state) }
        .and_then(|state| state.value(index))
        .is_some_and(LuaValue::truthy) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn lua_tointegerx(
    state: *mut lua_State,
    index: c_int,
    isnum: *mut c_int,
) -> LuaInteger {
    let value = unsafe { state_mut(state) }
        .and_then(|state| state.value(index))
        .and_then(|value| super::util::coerce_integer(value).ok());
    if !isnum.is_null() {
        unsafe { *isnum = value.is_some() as c_int };
    }
    value.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn lua_tonumberx(
    state: *mut lua_State,
    index: c_int,
    isnum: *mut c_int,
) -> LuaNumber {
    let value = unsafe { state_mut(state) }
        .and_then(|state| state.value(index))
        .and_then(|value| super::util::coerce_number(value).ok())
        .map(|value| match value {
            Number::Integer(value) => value as f64,
            Number::Float(value) => value,
        });
    if !isnum.is_null() {
        unsafe { *isnum = value.is_some() as c_int };
    }
    value.unwrap_or(0.0)
}

#[no_mangle]
pub unsafe extern "C" fn lua_tolstring(
    state: *mut lua_State,
    index: c_int,
    len: *mut usize,
) -> *const c_char {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null();
    };
    let Some(slot) = state.absolute(index) else {
        return ptr::null();
    };
    if !matches!(state.stack[slot], LuaValue::String(_)) {
        let bytes = match &state.stack[slot] {
            LuaValue::Integer(_) | LuaValue::Float(_) => state.stack[slot].display_bytes(),
            _ => return ptr::null(),
        };
        let interned = unsafe { state.runtime() }.fresh_str(bytes);
        state.stack[slot] = LuaValue::String(interned);
    }
    let LuaValue::String(bytes) = &state.stack[slot] else {
        unreachable!()
    };
    if !len.is_null() {
        unsafe { *len = bytes.len() };
    }
    let mut terminated = bytes.as_bytes().to_vec();
    terminated.push(0);
    state.c_strings.push(terminated.into_boxed_slice());
    state.c_strings.last().unwrap().as_ptr().cast()
}

#[no_mangle]
pub unsafe extern "C" fn lua_touserdata(state: *mut lua_State, index: c_int) -> *mut c_void {
    match unsafe { state_mut(state) }.and_then(|state| state.value(index)) {
        Some(LuaValue::Userdata(value)) => value.bytes_ptr(),
        Some(LuaValue::LightUserdata(value)) => *value as *mut c_void,
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_newuserdatauv(
    state: *mut lua_State,
    size: usize,
    nuvalue: c_int,
) -> *mut c_void {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null_mut();
    };
    let value =
        unsafe { state.runtime() }.new_userdata_with_uservalues(size, nuvalue.max(0) as usize);
    let LuaValue::Userdata(userdata) = &value else {
        unreachable!()
    };
    let pointer = userdata.bytes_ptr();
    state.stack.push(value);
    pointer
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushcclosure(
    state: *mut lua_State,
    function: Option<LuaCFunction>,
    n: c_int,
) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let capture_start = state.stack.len().saturating_sub(n.max(0) as usize);
    let captures = state.stack[capture_start..].to_vec();
    state.stack.truncate(capture_start);
    let Some(function) = function else {
        state.stack.push(LuaValue::Nil);
        return;
    };
    match unsafe { state.runtime() }.register_c_function(function) {
        Ok(id) => {
            let canonical = captures
                .iter()
                .map(|value| state.to_canonical(value))
                .collect::<LuaResult<Vec<_>>>();
            match canonical {
                Ok(captures) => {
                    let heap = unsafe { state.runtime() }.canonical_heap.clone();
                    state
                        .stack
                        .push(LuaValue::CFunction(CanonicalCFunction::allocate(
                            heap, id, captures,
                        )));
                }
                Err(error) => {
                    state.push_error(error);
                }
            }
        }
        Err(error) => {
            state.push_error(error);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_iscfunction(state: *mut lua_State, index: c_int) -> c_int {
    matches!(
        unsafe { state_mut(state) }.and_then(|state| state.value(index)),
        Some(LuaValue::CFunction(_))
    ) as c_int
}

mod debug;
pub use debug::*;

#[no_mangle]
pub unsafe extern "C" fn lua_isinteger(state: *mut lua_State, index: c_int) -> c_int {
    matches!(
        unsafe { state_mut(state) }.and_then(|state| state.value(index)),
        Some(LuaValue::Integer(_))
    ) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn lua_isnumber(state: *mut lua_State, index: c_int) -> c_int {
    let mut valid = 0;
    unsafe { lua_tonumberx(state, index, &mut valid) };
    valid
}

#[no_mangle]
pub unsafe extern "C" fn lua_isstring(state: *mut lua_State, index: c_int) -> c_int {
    matches!(unsafe { lua_type(state, index) }, LUA_TSTRING | LUA_TNUMBER) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn lua_isuserdata(state: *mut lua_State, index: c_int) -> c_int {
    matches!(
        unsafe { lua_type(state, index) },
        LUA_TUSERDATA | LUA_TLIGHTUSERDATA
    ) as c_int
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawequal(state: *mut lua_State, left: c_int, right: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    match (state.value(left), state.value(right)) {
        (Some(left), Some(right)) => (left == right) as c_int,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_compare(
    state: *mut lua_State,
    left: c_int,
    right: c_int,
    operation: c_int,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let (Some(left), Some(right)) = (state.value(left).cloned(), state.value(right).cloned())
    else {
        return 0;
    };
    let operation = match operation {
        0 => BinaryOp::Eq,
        1 => BinaryOp::Lt,
        2 => BinaryOp::Le,
        _ => return 0,
    };
    match unsafe { state.runtime() }.binary(operation, left, right) {
        Ok(value) => value.truthy() as c_int,
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_arith(state: *mut lua_State, operation: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let unary = operation >= 12;
    let Some(right) = state.stack.pop() else {
        return;
    };
    let left = if unary {
        match operation {
            12 => LuaValue::Integer(0),
            13 => LuaValue::Integer(-1),
            _ => return,
        }
    } else {
        let Some(left) = state.stack.pop() else {
            return;
        };
        left
    };
    let operation = match operation {
        0 => BinaryOp::Add,
        1 | 12 => BinaryOp::Sub,
        2 => BinaryOp::Mul,
        3 => BinaryOp::Mod,
        4 => BinaryOp::Pow,
        5 => BinaryOp::Div,
        6 => BinaryOp::FloorDiv,
        7 => BinaryOp::BitAnd,
        8 => BinaryOp::BitOr,
        9 | 13 => BinaryOp::BitXor,
        10 => BinaryOp::Shl,
        11 => BinaryOp::Shr,
        _ => return,
    };
    match unsafe { state.runtime() }.binary(operation, left, right) {
        Ok(value) => state.stack.push(value),
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_tocfunction(
    state: *mut lua_State,
    index: c_int,
) -> Option<LuaCFunction> {
    let state = unsafe { state_mut(state) }?;
    let LuaValue::CFunction(callable) = state.value(index)? else {
        return None;
    };
    let id = callable.callable_id();
    unsafe { state.runtime() }.c_functions.get(&id).copied()
}

#[no_mangle]
pub unsafe extern "C" fn lua_topointer(state: *mut lua_State, index: c_int) -> *const c_void {
    unsafe { state_mut(state) }
        .and_then(|state| state.value(index))
        .and_then(LuaValue::identity_address)
        .map_or(ptr::null(), |address| address as *const c_void)
}

#[no_mangle]
pub unsafe extern "C" fn lua_rawlen(state: *mut lua_State, index: c_int) -> usize {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let value = state.value(index).cloned();
    match value {
        Some(LuaValue::String(value)) => value.len(),
        Some(LuaValue::Table(table)) => unsafe { state.runtime() }.table_len(table),
        Some(LuaValue::CanonicalTable(value)) => value.len(),
        Some(LuaValue::Userdata(value)) => value.len(),
        _ => 0,
    }
}

mod table;
pub use table::*;

#[no_mangle]
pub unsafe extern "C" fn luaL_loadbufferx(
    state: *mut lua_State,
    source: *const c_char,
    size: usize,
    name: *const c_char,
    mode: *const c_char,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    if source.is_null() && size != 0 {
        return state.push_error(LuaError::new("null chunk buffer"));
    }
    let source = if size == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(source.cast::<u8>(), size) }
    };
    let chunkname = if name.is_null() {
        b"=(load)".to_vec()
    } else {
        unsafe { CStr::from_ptr(name) }.to_bytes().to_vec()
    };
    let mode = if mode.is_null() {
        LuaValue::Nil
    } else {
        LuaValue::String(
            unsafe { state.runtime() }.intern_str(unsafe { CStr::from_ptr(mode) }.to_bytes()),
        )
    };
    let args = vec![
        LuaValue::String(unsafe { state.runtime() }.fresh_str(source)),
        LuaValue::String(unsafe { state.runtime() }.fresh_str(chunkname)),
        mode,
    ];
    match unsafe { state.runtime() }.call_native_load(NativeFunction::Load, args) {
        Ok(values) if !matches!(values.first(), Some(LuaValue::Nil) | None) => {
            state.stack.push(values[0].clone());
            LUA_OK
        }
        Ok(values) => {
            let message = match values.get(1).cloned() {
                Some(value) => value,
                None => LuaValue::String(unsafe { state.runtime() }.intern_str(b"load failed")),
            };
            state.stack.push(message);
            LUA_ERRSYNTAX
        }
        Err(error) => state.push_error(error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn luaL_loadstring(state: *mut lua_State, source: *const c_char) -> c_int {
    if source.is_null() {
        return LUA_ERRSYNTAX;
    }
    let size = unsafe { CStr::from_ptr(source) }.to_bytes().len();
    unsafe { luaL_loadbufferx(state, source, size, c"=(string)".as_ptr(), ptr::null()) }
}

#[no_mangle]
pub unsafe extern "C" fn luaL_loadfilex(
    state: *mut lua_State,
    filename: *const c_char,
    mode: *const c_char,
) -> c_int {
    if filename.is_null() {
        if let Some(state) = unsafe { state_mut(state) } {
            let message =
                unsafe { state.runtime() }.intern_str(b"stdin loading is not supported");
            state.stack.push(LuaValue::String(message));
        }
        return LUA_ERRRUN;
    }
    let path = unsafe { CStr::from_ptr(filename) }.to_string_lossy();
    match std::fs::read(path.as_ref()) {
        Ok(source) => unsafe {
            luaL_loadbufferx(state, source.as_ptr().cast(), source.len(), filename, mode)
        },
        Err(error) => {
            if let Some(state) = unsafe { state_mut(state) } {
                let message = unsafe { state.runtime() }
                    .fresh_str(format!("cannot open {}: {error}", path));
                state.stack.push(LuaValue::String(message));
            }
            LUA_ERRRUN
        }
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_load(
    state: *mut lua_State,
    reader: Option<LuaReader>,
    data: *mut c_void,
    name: *const c_char,
    mode: *const c_char,
) -> c_int {
    let Some(reader) = reader else {
        return LUA_ERRSYNTAX;
    };
    let mut source = Vec::new();
    loop {
        let mut size = 0;
        let piece = unsafe { reader(state, data, &mut size) };
        if piece.is_null() || size == 0 {
            break;
        }
        source.extend_from_slice(unsafe { std::slice::from_raw_parts(piece.cast::<u8>(), size) });
    }
    unsafe { luaL_loadbufferx(state, source.as_ptr().cast(), source.len(), name, mode) }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_dump(
    state: *mut lua_State,
    writer: Option<LuaWriter>,
    data: *mut c_void,
    _strip: c_int,
) -> c_int {
    let Some(writer) = writer else { return 1 };
    let Some(state_ref) = (unsafe { state_mut(state) }) else {
        return 1;
    };
    let Some(function) = state_ref.stack.last().cloned() else {
        return 1;
    };
    let bytes = match unsafe { state_ref.runtime() }.call(
        LuaValue::NativeFunction(NativeFunction::StringDump),
        vec![function],
    ) {
        Ok(values) => match values.first() {
            Some(LuaValue::String(bytes)) => bytes.clone(),
            _ => return 1,
        },
        Err(_) => return 1,
    };
    unsafe { writer(state, bytes.as_ptr().cast(), bytes.len(), data) }
}

#[no_mangle]
pub unsafe extern "C" fn lua_pcallk(
    state: *mut lua_State,
    nargs: c_int,
    nresults: c_int,
    _errfunc: c_int,
    _ctx: LuaKContext,
    _continuation: Option<LuaKFunction>,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    if nargs < 0 || state.stack.len() < nargs as usize + 1 {
        return state.push_error(LuaError::new("invalid C API call frame"));
    }
    let function_index = state.stack.len() - nargs as usize - 1;
    let function = state.stack[function_index].clone();
    let args = state.stack.split_off(function_index + 1);
    state.stack.pop();
    match unsafe { state.runtime() }.call(function, args) {
        Ok(mut results) => {
            if nresults != LUA_MULTRET {
                results.resize(nresults.max(0) as usize, LuaValue::Nil);
                results.truncate(nresults.max(0) as usize);
            }
            state.stack.extend(results);
            LUA_OK
        }
        Err(error) => state.push_error(error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_callk(
    state: *mut lua_State,
    nargs: c_int,
    nresults: c_int,
    ctx: LuaKContext,
    continuation: Option<LuaKFunction>,
) {
    let _ = unsafe { lua_pcallk(state, nargs, nresults, 0, ctx, continuation) };
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_error(state: *mut lua_State) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    let value = state.stack.last().cloned().unwrap_or(LuaValue::Nil);
    let mut error = LuaError::new(String::from_utf8_lossy(&value.display_bytes()).into_owned());
    error.value = Some(value);
    state.pending_error = Some(error);
    resume_unwind(Box::new(CApiJump))
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_len(state: *mut lua_State, index: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    let Some(value) = state.value(index).cloned() else {
        api_jump(state, LuaError::new("invalid value index"));
    };
    match unsafe { state.runtime() }.length_of(value) {
        Ok(length) => state.stack.push(LuaValue::Integer(length)),
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_concat(state: *mut lua_State, count: c_int) {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return;
    };
    if count <= 0 {
        let empty = unsafe { state.runtime() }.intern_str(b"");
        state.stack.push(LuaValue::String(empty));
        return;
    }
    let count = count as usize;
    if count > state.stack.len() {
        api_jump(state, LuaError::new("not enough values to concatenate"));
    }
    let start = state.stack.len() - count;
    let values = state.stack.split_off(start);
    let mut values = values.into_iter();
    let mut result = values.next().unwrap();
    for value in values {
        result = match unsafe { state.runtime() }.binary(BinaryOp::Concat, result, value) {
            Ok(value) => value,
            Err(error) => api_jump(state, error),
        };
    }
    state.stack.push(result);
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_next(state: *mut lua_State, index: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let table = state.value(index).cloned();
    let key = state.stack.pop().unwrap_or(LuaValue::Nil);
    let result = (|| -> LuaResult<Vec<LuaValue>> {
        match table {
            Some(LuaValue::Table(table)) => {
                unsafe { state.runtime() }.next(LuaValue::Table(table), key)
            }
            Some(LuaValue::CanonicalTable(table)) => {
                let canonical_key = state.to_canonical(&key)?;
                let runtime = unsafe { state.runtime() };
                let entries = runtime
                    .canonical_heap
                    .borrow_mut()
                    .table_entries(table.object_id())
                    .map_err(|error| LuaError::new(error.to_string()))?;
                let start = if canonical_key == sol_core::Value::NIL {
                    0
                } else {
                    entries
                        .iter()
                        .position(|(entry, _)| *entry == canonical_key)
                        .ok_or_else(|| LuaError::new("invalid key to 'next'"))?
                        + 1
                };
                match entries.get(start) {
                    Some((key, value)) => Ok(vec![
                        canonical_to_lua(runtime, *key)?,
                        canonical_to_lua(runtime, *value)?,
                    ]),
                    None => Ok(vec![LuaValue::Nil]),
                }
            }
            Some(value) => Err(LuaError::new(format!(
                "table expected, got {}",
                value.type_name()
            ))),
            None => Err(LuaError::new("invalid table index")),
        }
    })();
    match result {
        Ok(values) if !matches!(values.first(), Some(LuaValue::Nil) | None) => {
            state.stack.extend(values.into_iter().take(2));
            1
        }
        Ok(_) => 0,
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_newthread(state: *mut lua_State) -> *mut lua_State {
    let Some(parent) = (unsafe { state_mut(state) }) else {
        return ptr::null_mut();
    };
    let thread = match unsafe { parent.runtime() }.new_coroutine(LuaValue::Nil) {
        Ok(thread) => thread,
        Err(error) => api_jump(parent, error),
    };
    let identity = thread.object_id().raw();
    let child = Box::into_raw(lua_State::child(parent, thread));
    unsafe { parent.runtime() }
        .c_thread_states
        .borrow_mut()
        .insert(identity, child);
    parent.stack.push(LuaValue::Thread(thread));
    child
}

#[no_mangle]
pub unsafe extern "C" fn lua_pushthread(state: *mut lua_State) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let thread = state.thread;
    let is_main = thread == unsafe { state.runtime() }.main_coroutine;
    state.stack.push(LuaValue::Thread(thread));
    is_main as c_int
}

#[no_mangle]
pub unsafe extern "C" fn lua_tothread(state: *mut lua_State, index: c_int) -> *mut lua_State {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return ptr::null_mut();
    };
    let Some(LuaValue::Thread(thread)) = state.value(index) else {
        return ptr::null_mut();
    };
    let identity = thread.object_id().raw();
    unsafe { state.runtime() }
        .c_thread_states
        .borrow()
        .get(&identity)
        .copied()
        .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn lua_xmove(from: *mut lua_State, to: *mut lua_State, count: c_int) {
    if from == to || count <= 0 {
        return;
    }
    let (Some(from), Some(to)) = (unsafe { state_mut(from) }, unsafe { state_mut(to) }) else {
        return;
    };
    if from.runtime != to.runtime || count as usize > from.stack.len() {
        return;
    }
    let values = from.stack.split_off(from.stack.len() - count as usize);
    to.stack.extend(values);
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_yieldk(
    state: *mut lua_State,
    result_count: c_int,
    context: LuaKContext,
    continuation: Option<LuaKFunction>,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    let can_yield = {
        let runtime = unsafe { state.runtime() };
        !runtime.coroutine_stack.is_empty() && !runtime.in_non_yieldable_call()
    };
    if !can_yield {
        api_jump(
            state,
            LuaError::new("attempt to yield across a C-call boundary"),
        );
    }
    if result_count < 0 || result_count as usize > state.stack.len() {
        api_jump(state, LuaError::new("invalid C yield result count"));
    }
    resume_unwind(Box::new(CApiYield {
        result_count: result_count as usize,
        continuation: continuation.map(|function| CContinuation { function, context }),
    }))
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_resume(
    state: *mut lua_State,
    from: *mut lua_State,
    argument_count: c_int,
    result_count: *mut c_int,
) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    if !from.is_null() && unsafe { (*from).runtime } != state.runtime {
        return state.push_error(LuaError::new("cannot resume across independent states"));
    }
    let argument_count = argument_count.max(0) as usize;
    if argument_count > state.stack.len() {
        return state.push_error(LuaError::new("not enough arguments to resume"));
    }
    let thread = state.thread;
    let coroutine = unsafe { state.runtime() }.coroutine(thread);
    let first_resume = coroutine.frames.borrow().is_empty();
    if first_resume && state.stack.len() <= argument_count {
        return state.push_error(LuaError::new("cannot resume a thread without a function"));
    }
    let mut args = state.stack.split_off(state.stack.len() - argument_count);
    if first_resume {
        let function = state.stack.pop().unwrap();
        *coroutine.body.borrow_mut() = Some(function);
    }
    state.stack.clear();
    let identity = thread.object_id().raw();
    let continuation = unsafe { state.runtime() }
        .c_continuations
        .borrow_mut()
        .remove(&identity);
    if let Some(continuation) = continuation {
        match invoke_continuation(unsafe { state.runtime() }, thread, continuation, args) {
            Ok(CApiOutcome::Returned(values)) => args = values,
            Ok(CApiOutcome::Yielded(values)) => {
                state.stack = values;
                state.status = LUA_YIELD;
                if !result_count.is_null() {
                    unsafe { *result_count = state.stack.len() as c_int };
                }
                return LUA_YIELD;
            }
            Err(error) => {
                coroutine.status.set(CoroutineStatus::Dead);
                let heap = unsafe { state.runtime() }.canonical_heap.clone();
                state.stack.push(error.into_lua_value(&heap));
                state.status = LUA_ERRRUN;
                if !result_count.is_null() {
                    unsafe { *result_count = 1 };
                }
                return LUA_ERRRUN;
            }
        }
    }
    let runtime = unsafe { state.runtime() };
    let outcome = runtime.resume_coroutine_outcome(thread, args);
    let heap = runtime.canonical_heap.clone();
    let (status, values) = match outcome {
        sol_core::CallOutcome::Returned(values) => (LUA_OK, values),
        sol_core::CallOutcome::Yielded(values) => (LUA_YIELD, values),
        sol_core::CallOutcome::Raised(error) => (LUA_ERRRUN, vec![error.into_lua_value(&heap)]),
        sol_core::CallOutcome::TailCall(_) => {
            unreachable!("the coroutine trampoline consumes tail calls")
        }
    };
    state.stack = values;
    state.status = status;
    if !result_count.is_null() {
        unsafe { *result_count = state.stack.len() as c_int };
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn lua_closethread(state: *mut lua_State, _from: *mut lua_State) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return LUA_ERRRUN;
    };
    let thread = state.thread;
    let runtime = unsafe { state.runtime() };
    let main_thread = runtime.main_coroutine;
    let coroutine = runtime.coroutine(thread);
    if coroutine.status.get() == CoroutineStatus::Running && thread != main_thread {
        return LUA_ERRRUN;
    }
    state.stack.clear();
    coroutine.frames.borrow_mut().clear();
    *coroutine.body.borrow_mut() = None;
    coroutine.status.set(CoroutineStatus::Dead);
    state.status = LUA_OK;
    LUA_OK
}

#[no_mangle]
pub unsafe extern "C" fn lua_status(state: *mut lua_State) -> c_int {
    unsafe { state.as_ref() }
        .map(|state| state.status)
        .unwrap_or(LUA_ERRRUN)
}

#[no_mangle]
pub unsafe extern "C" fn lua_isyieldable(state: *mut lua_State) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return 0;
    };
    let main_thread = unsafe { state.runtime() }.main_coroutine;
    (state.thread != main_thread) as c_int
}

#[no_mangle]
pub unsafe extern "C-unwind" fn lua_gc(state: *mut lua_State, what: c_int) -> c_int {
    let Some(state) = (unsafe { state_mut(state) }) else {
        return -1;
    };
    let option: &[u8] = match what {
        0 => b"stop",
        1 => b"restart",
        2 => b"collect",
        3 | 4 => b"count",
        5 => b"step",
        6 => b"isrunning",
        7 => b"generational",
        8 => b"incremental",
        _ => return -1,
    };
    let option = unsafe { state.runtime() }.intern_str(option);
    match unsafe { state.runtime() }.call_native(
        NativeFunction::CollectGarbage,
        vec![LuaValue::String(option)],
    ) {
        Ok(values) => match values.first() {
            Some(LuaValue::Integer(value)) => *value as c_int,
            Some(LuaValue::Float(value)) => {
                if what == 4 {
                    ((*value * 1024.0) as usize % 1024) as c_int
                } else {
                    *value as c_int
                }
            }
            Some(LuaValue::Bool(value)) => *value as c_int,
            _ => 0,
        },
        Err(error) => api_jump(state, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lua_numbertocstring(
    state: *mut lua_State,
    index: c_int,
    buffer: *mut c_char,
) -> u32 {
    if buffer.is_null() {
        return 0;
    }
    let mut len = 0;
    let string = unsafe { lua_tolstring(state, index, &mut len) };
    if string.is_null() {
        return 0;
    }
    unsafe { ptr::copy_nonoverlapping(string, buffer, len + 1) };
    len as u32
}

#[no_mangle]
pub unsafe extern "C" fn lua_stringtonumber(state: *mut lua_State, string: *const c_char) -> usize {
    if string.is_null() {
        return 0;
    }
    let bytes = unsafe { CStr::from_ptr(string) }.to_bytes();
    let Some(number) = super::util::parse_lua_number(bytes) else {
        return 0;
    };
    match number {
        Number::Integer(value) => unsafe { lua_pushinteger(state, value) },
        Number::Float(value) => unsafe { lua_pushnumber(state, value) },
    }
    bytes.len() + 1
}

mod auxlib;
pub use auxlib::*;

fn value_tag(value: &LuaValue) -> c_int {
    match value {
        LuaValue::Nil => LUA_TNIL,
        LuaValue::Bool(_) => LUA_TBOOLEAN,
        LuaValue::LightUserdata(_) => LUA_TLIGHTUSERDATA,
        LuaValue::Integer(_) | LuaValue::Float(_) => LUA_TNUMBER,
        LuaValue::String(_) => LUA_TSTRING,
        LuaValue::Table(_) | LuaValue::CanonicalTable(_) => LUA_TTABLE,
        LuaValue::Userdata(_) => LUA_TUSERDATA,
        LuaValue::Thread(_) => LUA_TTHREAD,
        _ => LUA_TFUNCTION,
    }
}

fn api_jump(state: &mut lua_State, error: LuaError) -> ! {
    state.pending_error = Some(error);
    resume_unwind(Box::new(CApiJump))
}

fn store_c_bytes(state: &mut lua_State, bytes: &[u8]) -> *const c_char {
    let mut terminated = bytes.to_vec();
    terminated.push(0);
    state.c_strings.push(terminated.into_boxed_slice());
    state.c_strings.last().unwrap().as_ptr().cast()
}

fn close_c_stack_slot(state: &mut lua_State, slot: usize) -> LuaResult<()> {
    let Some(value) = state.stack.get(slot).cloned() else {
        return Ok(());
    };
    state.stack[slot] = LuaValue::Nil;
    if matches!(value, LuaValue::Nil | LuaValue::Bool(false)) {
        return Ok(());
    }
    let runtime = unsafe { state.runtime() };
    let Some(method) = runtime.metamethod(&value, b"__close")? else {
        return Err(LuaError::new("value is not closable"));
    };
    runtime.call(method, vec![value, LuaValue::Nil]).map(|_| ())
}

fn raw_table_get(state: &mut lua_State, table: LuaValue, key: &LuaValue) -> LuaResult<LuaValue> {
    match table {
        LuaValue::Table(table) => unsafe { state.runtime() }.table_get(table, key),
        LuaValue::CanonicalTable(table) => {
            canonical_table_get(unsafe { state.runtime() }, table.object_id(), key)
        }
        value => Err(LuaError::new(format!(
            "table expected, got {}",
            value.type_name()
        ))),
    }
}

fn raw_table_set(
    state: &mut lua_State,
    table: LuaValue,
    key: LuaValue,
    value: LuaValue,
) -> LuaResult<()> {
    match table {
        LuaValue::Table(table) => unsafe { state.runtime() }.table_set(table, key, value),
        LuaValue::CanonicalTable(table) => {
            let key = state.to_canonical(&key)?;
            let value = state.to_canonical(&value)?;
            let runtime = unsafe { state.runtime() };
            runtime
                .canonical_heap
                .borrow_mut()
                .table_set(table.object_id(), key, value)
                .map_err(|error| LuaError::new(error.to_string()))
        }
        value => Err(LuaError::new(format!(
            "table expected, got {}",
            value.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests;
