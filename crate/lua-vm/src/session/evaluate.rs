// Evaluation (docs/debug-protocol.md#evaluation): running an arbitrary Lua
// expression with a paused frame's named locals in scope, via a compiled
// wrapper chunk rather than resuming into the live frame (piccolo's
// `Executor` has no API for that) - see `compile_and_run_eval`'s doc
// comment for the full design and its consequences.

use vm::{
    DebugFrame, Executor, Fuel, IntoValue, StashedExecutor, Table, Thread, UpValue, UpValueState,
    Value,
};
use wasm_bindgen::prelude::*;

use crate::{display_value, MAX_INSTRUCTIONS};

use super::types::thread_by_id;
use super::{DebugSession, EvalResult};

#[wasm_bindgen]
impl DebugSession {
    /// Evaluates `expression` with `frame_index`'s locals in scope (falling
    /// back to globals for anything not shadowed by a local), per
    /// debug-protocol.md's requirement that evaluation run in the selected
    /// frame's environment, not global scope. Implementation note in
    /// docs/phase-4-8-implementation.md: this works by compiling `expression`
    /// as a wrapper chunk that unpacks a snapshot of the frame's *named*
    /// locals from a table argument, rather than splicing into the live
    /// paused call - piccolo's `Executor` has no API for resuming into the
    /// middle of an already-running frame, so a real closure/upvalue-sharing
    /// implementation isn't available without deeper VM changes than this
    /// pass scoped. Consequence: assignments inside `expression` to an
    /// existing local do not write back to the paused frame (use
    /// `set_variable` for that); assignments to a new/global name behave
    /// like a normal Lua statement, since globals are the same table the
    /// paused program itself sees.
    pub fn evaluate(&mut self, thread_id: u32, expression: &str, frame_index: u32) -> EvalResult {
        let executor = match self.compile_and_run_eval(thread_id, expression, frame_index) {
            Ok(executor) => executor,
            Err(result) => return result,
        };
        // `display_value` converts the result to an owned `String` *inside*
        // this closure - a `Value<'gc>` can't be the closure's return type
        // (see `compile_and_run_eval`'s doc comment on why), only data
        // that's independent of this particular call's `'gc` brand can.
        let result: Result<std::string::String, vm::StaticError> =
            self.lua.try_enter(
                |ctx| match ctx.fetch(&executor).take_result::<Value>(ctx)? {
                    Ok(value) => Ok(display_value(value)),
                    Err(err) => Err(err),
                },
            );
        match result {
            Ok(display) => EvalResult { ok: true, display },
            Err(err) => EvalResult {
                ok: false,
                display: err.to_string(),
            },
        }
    }

    /// Phase 7: writes `value_expr` (evaluated the same way as `evaluate`,
    /// including seeing `frame_index`'s locals) directly into
    /// `frame_index`'s local named `name`. Unlike `evaluate`, this *does*
    /// mutate the paused frame - the write happens via
    /// `Thread::debug_write_register` inside the same `try_enter` call that
    /// produces the evaluated `Value`, since a `Value<'gc>` can't cross
    /// between separate `enter`/`try_enter` calls (each has its own,
    /// independent `'gc` brand - see `compile_and_run_eval`'s doc comment).
    ///
    /// Falls back to a named *upvalue* of the frame's closure if no local
    /// matches (locals win on a name collision, matching `find_named_register`
    /// being tried first) - written via `UpValue`/`OpenUpValue`'s public
    /// `set` rather than `debug_write_register`, since upvalues aren't
    /// backed by a register slot at all (see `find_named_upvalue`'s doc
    /// comment).
    pub fn set_variable(
        &mut self,
        thread_id: u32,
        frame_index: u32,
        name: &str,
        value_expr: &str,
    ) -> EvalResult {
        let executor = match self.compile_and_run_eval(thread_id, value_expr, frame_index) {
            Ok(executor) => executor,
            Err(result) => return result,
        };
        let name_owned = name.to_string();
        self.lua
            .try_enter(|ctx| {
                let value = match ctx.fetch(&executor).take_result::<Value>(ctx)? {
                    Ok(value) => value,
                    Err(err) => {
                        return Ok(EvalResult {
                            ok: false,
                            display: err.to_string(),
                        })
                    }
                };
                let Some(thread) = thread_by_id(ctx, &self.executor, thread_id) else {
                    return Ok(EvalResult {
                        ok: false,
                        display: "no running thread".to_string(),
                    });
                };
                let display = display_value(value);
                if let Some(register) = find_named_register(thread, frame_index, &name_owned) {
                    thread.debug_write_register(&ctx, frame_index as usize, register, value);
                    return Ok(EvalResult { ok: true, display });
                }
                if let Some(upvalue) = find_named_upvalue(thread, frame_index, &name_owned) {
                    match upvalue.get() {
                        UpValueState::Open(open) => open.set(&ctx, value),
                        UpValueState::Closed(_) => upvalue.set(&ctx, UpValueState::Closed(value)),
                    }
                    return Ok(EvalResult { ok: true, display });
                }
                Ok(EvalResult {
                    ok: false,
                    display: format!("no local or upvalue named '{name_owned}' in this frame"),
                })
            })
            .unwrap_or_else(|err| EvalResult {
                ok: false,
                display: err.to_string(),
            })
    }
}

impl DebugSession {
    /// Locals live at `frame_index`'s current pc, as owned `(name, Value)`
    /// pairs used to build the wrapper chunk's argument table in
    /// `compile_and_run_eval`. Only *named* locals are included (temporary
    /// registers without a source-level name aren't meaningfully
    /// referenceable by an expression anyway).
    /// Returns `(register, name)` for every *named* local live at
    /// `frame_index`'s current pc - keeping the actual register number
    /// attached to each name (rather than just a `Vec<String>`) matters:
    /// unnamed registers get filtered out, so their positions in a plain
    /// name list would no longer line up with real register numbers, and
    /// reading back the wrong register for a name silently returns some
    /// other variable's value instead of erroring.
    fn frame_named_locals(
        &mut self,
        thread_id: u32,
        frame_index: u32,
    ) -> std::vec::Vec<(usize, std::string::String)> {
        self.lua.enter(|ctx| {
            let Some(thread) = thread_by_id(ctx, &self.executor, thread_id) else {
                return Vec::new();
            };
            let Some(frames) = thread.debug_frames() else {
                return Vec::new();
            };
            let Some(DebugFrame::Lua {
                function,
                pc,
                stack_size,
                ..
            }) = frames
                .iter()
                .rev()
                .filter(|f| matches!(f, DebugFrame::Lua { .. }))
                .nth(frame_index as usize)
                .copied()
            else {
                return Vec::new();
            };
            let proto = function.prototype();
            (0..stack_size)
                .filter_map(|reg| {
                    let name = proto.local_name_at(pc, vm::types::RegisterIndex(reg as u8))?;
                    Some((
                        reg,
                        std::string::String::from_utf8_lossy(name.as_bytes()).into_owned(),
                    ))
                })
                .collect()
        })
    }

    /// Compiles `expression` as a wrapper chunk with `frame_index`'s named
    /// locals in scope, and steps it to completion. Returns the finished
    /// (but not yet `take_result`ed) executor so callers decide what to do
    /// with the resulting `Value` themselves, inside their own `try_enter`
    /// call - `evaluate()` just displays it, `set_variable()` also writes it
    /// into a register. Both need this: a `Value<'gc>` produced by
    /// `take_result` cannot be returned out of the `enter`/`try_enter` call
    /// that produced it (each call has its own, independently-branded `'gc`
    /// lifetime - see `Lua::enter`'s `for<'gc> FnOnce(Context<'gc>) -> T`
    /// signature, where `T` is fixed before the closure runs and so cannot
    /// mention that particular `'gc`), so gathering locals, compiling, and
    /// consuming the result cannot be split across separate calls the way
    /// an earlier draft of this file tried to.
    ///
    /// Implementation note (docs/phase-4-8-implementation.md has the full
    /// writeup): this works by snapshotting the frame's named locals into a
    /// table argument rather than splicing into the live paused call -
    /// piccolo's `Executor` has no API for resuming into the middle of an
    /// already-running frame. Consequence: assignments inside `expression`
    /// to an existing local do not write back to the paused frame (that's
    /// what `set_variable` is for); assignments to a new/global name behave
    /// like a normal Lua statement, since globals are the same table the
    /// paused program itself sees.
    fn compile_and_run_eval(
        &mut self,
        thread_id: u32,
        expression: &str,
        frame_index: u32,
    ) -> Result<StashedExecutor, EvalResult> {
        let locals = self.frame_named_locals(thread_id, frame_index);

        let mut wrapper = std::string::String::new();
        wrapper.push_str("local __locals = ...\n");
        for (_, name) in &locals {
            wrapper.push_str(&format!("local {name} = __locals[{name:?}]\n"));
        }
        wrapper.push_str(&format!("return ({expression})"));

        let result = self.lua.try_enter(|ctx| {
            let Some(thread) = thread_by_id(ctx, &self.executor, thread_id) else {
                return Err(vm::Error::from(
                    "evaluate() requires a running thread".into_value(ctx),
                ));
            };

            let locals_table = Table::new(&ctx);
            for (reg, name) in &locals {
                if let Some(value) = thread.debug_read_register(frame_index as usize, *reg) {
                    locals_table.set(ctx, ctx.intern(name.as_bytes()), value)?;
                }
            }

            let closure = vm::Closure::load(ctx, Some("=(evaluate)"), wrapper.as_bytes())?;
            let executor = Executor::start(ctx, closure.into(), (locals_table,));
            Ok(ctx.stash(executor))
        });
        let executor = result.map_err(|err| EvalResult {
            ok: false,
            display: err.to_string(),
        })?;

        let mut consumed: i64 = 0;
        loop {
            let mut fuel = Fuel::with(4096);
            let finished = self
                .lua
                .enter(|ctx| ctx.fetch(&executor).step(ctx, &mut fuel));
            consumed += (4096 - fuel.remaining()) as i64;
            if finished {
                break;
            }
            if consumed >= MAX_INSTRUCTIONS {
                return Err(EvalResult {
                    ok: false,
                    display: "evaluation exceeded instruction limit".to_string(),
                });
            }
        }
        Ok(executor)
    }
}

/// Finds the register currently holding the named local `name` in
/// `frame_index` (used by `set_variable`), scoped to the same `Thread`/`ctx`
/// as the caller so the returned index is safe to pass straight to
/// `debug_write_register` without re-deriving anything.
fn find_named_register(thread: Thread, frame_index: u32, name: &str) -> Option<usize> {
    let frames = thread.debug_frames()?;
    let DebugFrame::Lua {
        function,
        pc,
        stack_size,
        ..
    } = frames
        .iter()
        .rev()
        .filter(|f| matches!(f, DebugFrame::Lua { .. }))
        .nth(frame_index as usize)
        .copied()?
    else {
        return None;
    };
    let proto = function.prototype();
    (0..stack_size).find(|&reg| {
        proto
            .local_name_at(pc, vm::types::RegisterIndex(reg as u8))
            .is_some_and(|n| n.as_bytes() == name.as_bytes())
    })
}

/// Finds the upvalue named `name` captured by `frame_index`'s closure (used
/// by `set_variable`'s locals-then-upvalues fallback), mirroring
/// `get_upvalues`'s lookup (`inspector.rs`). Returns the `UpValue` handle
/// itself rather than an index: unlike a local (a plain register slot in the
/// frame's own stack window), an open upvalue can point into a *different*
/// thread's stack (a parent coroutine's), so there is no single
/// `(thread_id, frame_index, index)` triple that alone identifies where to
/// write - the `UpValue`/`UpValueState` handle already knows.
fn find_named_upvalue<'gc>(
    thread: Thread<'gc>,
    frame_index: u32,
    name: &str,
) -> Option<UpValue<'gc>> {
    let frames = thread.debug_frames()?;
    let DebugFrame::Lua { function, .. } = frames
        .iter()
        .rev()
        .filter(|f| matches!(f, DebugFrame::Lua { .. }))
        .nth(frame_index as usize)
        .copied()?
    else {
        return None;
    };
    let proto = function.prototype();
    function
        .upvalues()
        .iter()
        .enumerate()
        .find_map(|(i, upvalue)| {
            proto
                .upvalue_name_at(i)
                .filter(|n| n.as_bytes() == name.as_bytes())
                .map(|_| *upvalue)
        })
}
