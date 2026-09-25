//! the coroutine library.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::*;

impl LuaRuntime {
    pub(super) fn call_native_coroutine(
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
            NativeFunction::CoroutineCreate => {
                let f = required(0)?;
                // Real Lua's `coroutine.create` requires exactly
                // `LUA_TFUNCTION` (`luaL_checktype`), which every callable
                // `LuaValue` variant reports as via `type_name` - including
                // `GMatchIterator`/`CoroutineWrapper`, which this match had
                // been missing (a table with a `__call` metamethod is
                // "table", not "function", and real Lua rejects it here too,
                // so this deliberately checks the variant set rather than
                // generic callability).
                if f.type_name() != "function" {
                    return Err(LuaError::new(format!(
                        "bad argument #1 to 'create' (function expected, got {})",
                        f.type_name()
                    )));
                }
                let co = self.new_coroutine(f)?;
                Ok(vec![LuaValue::Thread(co)])
            }
            NativeFunction::CoroutineResume => {
                let co = Self::expect_coroutine(&required(0)?)?;
                let resume_args = args.into_iter().skip(1).collect();
                match self.resume_coroutine(&co, resume_args) {
                    Ok(mut values) => {
                        let mut results = vec![LuaValue::Bool(true)];
                        results.append(&mut values);
                        Ok(results)
                    }
                    Err(error) => Ok(vec![
                        LuaValue::Bool(false),
                        error.into_lua_value(&self.canonical_heap),
                    ]),
                }
            }
            NativeFunction::CoroutineYield => {
                // `coroutine.yield` is intercepted by `step_result_for_call`
                // (`StepResult::Yield`) whenever it is reached through the
                // trampoline - i.e. from an `Instr::Call`, or via
                // `resolve_call`/`push_native_call` from inside `pcall`,
                // `xpcall`, `table.sort`'s comparator, or `string.gsub`'s
                // function-replacement. It only reaches this blocking
                // `call_native` arm if invoked directly through the
                // `LuaRuntime::call` bridge from native Rust code outside the
                // trampoline (e.g. a `__tostring`/`__gc` metamethod call),
                // which is exactly "attempt to yield across a C-call
                // boundary" in real Lua - the same error `call_closure`
                // raises when a `DriveOutcome::Yielded` would otherwise
                // escape across that same boundary.
                Err(LuaError::new("attempt to yield across a C-call boundary"))
            }
            NativeFunction::CoroutineStatus => {
                let co = Self::expect_coroutine(&required(0)?)?;
                Ok(vec![LuaValue::String(self.intern_str(
                    co.status.get().as_str().as_bytes().to_vec(),
                ))])
            }
            NativeFunction::CoroutineWrap => {
                let f = required(0)?;
                // See the matching comment in `CoroutineCreate` above.
                if f.type_name() != "function" {
                    return Err(LuaError::new(format!(
                        "bad argument #1 to 'wrap' (function expected, got {})",
                        f.type_name()
                    )));
                }
                let co = self.new_coroutine(f)?;
                Ok(vec![LuaValue::CoroutineWrapper(co)])
            }
            NativeFunction::CoroutineRunning => match self.coroutine_stack.last() {
                Some(co) => Ok(vec![LuaValue::Thread(co.clone()), LuaValue::Bool(false)]),
                None => Ok(vec![
                    LuaValue::Thread(self.main_coroutine.clone()),
                    LuaValue::Bool(true),
                ]),
            },
            NativeFunction::CoroutineIsYieldable => {
                // Real Lua's `coroutine.isyieldable([co])` (`lua_isyieldable`,
                // `L->nny == 0`): a coroutine is yieldable throughout its
                // whole lifetime (fresh/suspended/running/dead) simply by
                // virtue of *not* being the main thread - the main thread
                // alone can never yield - *unless* it is also the coroutine
                // currently executing this very call, and that execution is
                // nested inside a native library call real Lua doesn't let a
                // yield cross (`string.gsub`'s replacement function,
                // `table.sort`'s comparator - see `in_non_yieldable_call`).
                // `co` defaults to the currently running coroutine when
                // omitted, matching `coroutine.running()`'s own
                // default-to-main fallback.
                let current = self
                    .coroutine_stack
                    .last()
                    .cloned()
                    .unwrap_or_else(|| self.main_coroutine.clone());
                let co = match args.first() {
                    Some(value) => Self::expect_coroutine(value)?,
                    None => current.clone(),
                };
                let yieldable = !Rc::ptr_eq(&co, &self.main_coroutine)
                    && !(Rc::ptr_eq(&co, &current) && self.in_non_yieldable_call());
                Ok(vec![LuaValue::Bool(yieldable)])
            }
            NativeFunction::CoroutineClose => {
                // Real Lua's `coroutine.close` (`lcorolib.c`'s `luaB_close`):
                // the main thread can never be closed. A `normal` coroutine
                // (suspended because it resumed another coroutine, and so
                // still on the call stack) can't be closed either. A
                // `running` coroutine can only ever mean closing yourself
                // (see below) - anything else succeeds: `suspended` (never
                // resumed, or yielded) and `dead` (idempotently).
                // `co` defaults to the currently running coroutine when
                // omitted (real Lua's `getoptco`), matching
                // `coroutine.isyieldable`'s own default-argument pattern.
                let co = match args.first() {
                    Some(value) => Self::expect_coroutine(value)?,
                    None => self
                        .coroutine_stack
                        .last()
                        .cloned()
                        .unwrap_or_else(|| self.main_coroutine.clone()),
                };
                match co.status.get() {
                    // `Running` can only ever describe the coroutine/main
                    // that is *currently executing* (see `resume_coroutine`:
                    // exactly one participant holds this status at a time,
                    // matching real Lua's `COS_RUN`, which is defined as
                    // `L == co` - the calling state closing itself). Real
                    // Lua's "cannot close main thread" only fires for that
                    // exact self-close on main; closing main from a
                    // coroutine it resumed instead sees main's status as
                    // `Normal`, reported by the generic "normal coroutine"
                    // message below.
                    CoroutineStatus::Running if Rc::ptr_eq(&co, &self.main_coroutine) => {
                        return Err(LuaError::new("cannot close main thread"));
                    }
                    CoroutineStatus::Running => {
                        // Self-close: real Lua's `luaB_close` calls
                        // `lua_closethread` on the very state that's making
                        // this call, which longjmps straight past every
                        // intervening `pcall`/`xpcall` protection level back
                        // to the `resume` boundary rather than returning an
                        // ordinary catchable error - see the `uncatchable`
                        // field doc on `LuaError` and its handling in
                        // `resume_coroutine`.
                        return Err(LuaError::new(
                            "coroutine.close: self-close (internal control-flow signal, must never reach user code)",
                        )
                        .make_uncatchable());
                    }
                    CoroutineStatus::Normal => {
                        return Err(LuaError::new("cannot close a normal coroutine"));
                    }
                    CoroutineStatus::Dead => {
                        // A coroutine that died from an uncaught error
                        // surfaces that error exactly once via `close` (real
                        // Lua's `lua_resetthread`/`lua_closethread` reports
                        // the thread's terminating status); closing it again
                        // afterward reports success like any other
                        // already-dead coroutine.
                        match co.dead_error.borrow_mut().take() {
                            Some(value) => Ok(vec![LuaValue::Bool(false), value]),
                            None => Ok(vec![LuaValue::Bool(true)]),
                        }
                    }
                    CoroutineStatus::Suspended => {
                        // Take the frame stack out of the `RefCell` first so
                        // no borrow is held while `close_pending` (which
                        // needs `&mut self`) runs `__close` metamethods.
                        let mut frames = std::mem::take(&mut *co.frames.borrow_mut());
                        let mut error: Option<LuaError> = None;
                        while let Some(frame) = frames.pop() {
                            if let Frame::Lua(mut lua_frame) = frame {
                                let count = lua_frame.to_close.len() as u16;
                                error = self.close_pending(&mut lua_frame, count, error);
                            }
                        }
                        self.call_depth -= co.depth_charged.take();
                        co.status.set(CoroutineStatus::Dead);
                        match error {
                            Some(e) => Ok(vec![
                                LuaValue::Bool(false),
                                e.into_lua_value(&self.canonical_heap),
                            ]),
                            None => Ok(vec![LuaValue::Bool(true)]),
                        }
                    }
                }
            }
            _ => unreachable!("call_native_coroutine received a non-coroutine NativeFunction"),
        }
    }
}
