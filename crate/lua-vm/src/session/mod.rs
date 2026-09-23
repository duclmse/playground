//! Phases 4-8 (docs/roadmap.md): a real, resumable debug session - set/hit
//! breakpoints (including conditional/hit-count/logpoint variants), step
//! over/into/out, pause/resume a running program, walk the call stack,
//! inspect locals/upvalues/globals/tables, evaluate expressions in a paused
//! frame's scope, and debug coroutines independently of the main thread.
//!
//! This is `docs/debug-protocol.md`'s `DebugSession` interface, implemented
//! on the Rust/WASM side rather than in TypeScript, since the primitives it
//! needs (per-frame register access, a resumable `Executor`, named-local/
//! upvalue debug info) only exist on this side of the `wasm-bindgen`
//! boundary - see `crates/vm/README.md` for the fork additions this builds
//! on (`debug_frames`, `debug_read_register`, `debug_write_register`,
//! `debug_thread_stack`, `FunctionPrototype::local_name_at`/
//! `upvalue_name_at`, `Executor::step_with_granularity`). The TS
//! `LuaDebugger` class (`packages/lua-runtime`) is a thin adapter from this
//! struct's method names to `debug-protocol.md`'s exact `DebugSession`
//! interface and the worker message protocol - it does not reimplement any
//! of this logic.
//!
//! Submodules, split out of what was originally one file as it grew past
//! ~1800 lines:
//! - [`registry`]: the `ObjectRegistry` handing out stable ids for
//!   lazily-expandable values (tables today; see its own doc comment).
//! - [`types`]: the small `wasm_bindgen`-exposed value/result types
//!   (`Variable`, `StackFrame`, `StopEvent`, ...) plus the private stepping
//!   state (`FrameSnapshot`/`StepMode`/`DriveOutcome`) they're built from.
//! - `breakpoints`: `set_breakpoint`/`remove_breakpoint`/the Phase 8
//!   condition/hit-count/log-message setters.
//! - `execution`: `continue_`/`continue_burst`/`step_*`/`get_threads`, and
//!   the private fuel-stepped driving engine (`drive`/`drive_with_budget`/
//!   `check_stop`) underneath all of them.
//! - `inspector`: `get_stack_trace`/`get_locals`/`get_upvalues`/
//!   `get_globals`/`get_table_entries`/`get_metatable`.
//! - `evaluate`: `evaluate`/`set_variable` and the wrapper-chunk machinery
//!   they're both built on (`compile_and_run_eval`'s doc comment has the
//!   full design and why it works this way).
//! - `memory`: `get_memory_stats`/`force_gc`, read straight from
//!   `gc-arena`'s `Metrics` - see `memory.rs`'s doc comment for what is and
//!   isn't available at that layer.
//!
//! Scope cut, documented in full in `docs/phase-4-8-implementation.md`:
//! `pause()`'s only non-mechanical remaining gap is genuinely nothing -
//! everything Phase 4-8 originally scoped is implemented. Smaller polish
//! items (hover-evaluation in the editor, function/thread/userdata
//! expansion in the variables tree, coroutines not on the active resume
//! chain) are tracked there too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vm::{Executor, Lua, StashedExecutor};
use wasm_bindgen::prelude::*;

use crate::{extend_table_library, install_print, install_require, install_xpcall};

mod breakpoints;
mod evaluate;
mod execution;
mod inspector;
mod memory;
mod registry;
mod types;

use registry::ObjectRegistry;
use types::BreakpointState;

pub use types::{
    Breakpoint, BurstResult, EvalResult, MemoryStats, StackFrame, StopEvent, ThreadInfo, Variable,
};

#[wasm_bindgen]
pub struct DebugSession {
    lua: Lua,
    executor: StashedExecutor,
    breakpoints: Vec<BreakpointState>,
    next_breakpoint_id: u32,
    registry: ObjectRegistry,
    output: Rc<RefCell<std::string::String>>,
    terminated: bool,
    // Set once the session has stopped at least once (breakpoint/step/
    // exception). Used by `drive()` to distinguish "fresh launch" from
    // "resuming from a stop" when deciding whether the very first observed
    // line is eligible to trigger a breakpoint - see `drive()`'s doc
    // comment on why that distinction matters.
    has_stopped_before: bool,
}

#[wasm_bindgen]
impl DebugSession {
    /// `launch()` from docs/debug-protocol.md#debugsession-interface.
    /// Installs the same stdlib surface as `execute()`/`run_project()`
    /// (sandboxed `print`, `table.insert`/`concat`/`sort`, `xpcall`), so a
    /// script behaves identically whether it's run or debugged.
    #[wasm_bindgen(constructor)]
    pub fn launch(source: &str, chunk_name: &str) -> DebugSession {
        let output = Rc::new(RefCell::new(std::string::String::new()));
        let mut lua = Lua::core();
        let executor = lua
            .try_enter(|ctx| {
                install_print(ctx, output.clone())?;
                extend_table_library(ctx)?;
                install_xpcall(ctx);
                let closure = vm::Closure::load(ctx, Some(chunk_name), source.as_bytes())?;
                let executor = Executor::start(ctx, closure.into(), ());
                Ok(ctx.stash(executor))
            })
            .expect("compile error surfaced via launch_result instead");
        DebugSession {
            lua,
            executor,
            breakpoints: Vec::new(),
            next_breakpoint_id: 0,
            registry: ObjectRegistry::default(),
            output,
            terminated: false,
            has_stopped_before: false,
        }
    }

    /// Like `launch`, but for a multi-file project - mirrors
    /// `execute_project()`'s `require()`/virtual-FS setup.
    pub fn launch_project(
        names: Vec<std::string::String>,
        contents: Vec<std::string::String>,
        entry: &str,
    ) -> DebugSession {
        let output = Rc::new(RefCell::new(std::string::String::new()));
        let file_map: Rc<HashMap<std::string::String, std::string::String>> =
            Rc::new(names.into_iter().zip(contents).collect());
        let entry_source = file_map
            .get(entry)
            .cloned()
            .unwrap_or_else(|| format!("error({:?})", format!("entry file '{entry}' not found")));
        let mut lua = Lua::core();
        let executor = lua
            .try_enter(|ctx| {
                install_print(ctx, output.clone())?;
                extend_table_library(ctx)?;
                install_xpcall(ctx);
                install_require(ctx, file_map.clone())?;
                let chunk_name = format!("@{entry}");
                let closure =
                    vm::Closure::load(ctx, Some(chunk_name.as_str()), entry_source.as_bytes())?;
                let executor = Executor::start(ctx, closure.into(), ());
                Ok(ctx.stash(executor))
            })
            .expect("compile error surfaced via launch_result instead");
        DebugSession {
            lua,
            executor,
            breakpoints: Vec::new(),
            next_breakpoint_id: 0,
            registry: ObjectRegistry::default(),
            output,
            terminated: false,
            has_stopped_before: false,
        }
    }

    /// Buffered `print()` output since the last call to this method
    /// (drains the buffer, matching how a console panel wants to poll for
    /// new output between stops).
    pub fn take_output(&mut self) -> std::string::String {
        self.output.borrow_mut().split_off(0)
    }

    pub fn is_terminated(&self) -> bool {
        self.terminated
    }
}

#[cfg(test)]
mod tests;
