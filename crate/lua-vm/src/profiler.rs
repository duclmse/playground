//! Phase 8 (docs/roadmap.md, docs/debug-protocol.md#advanced-profiler-phase-8):
//! runs a program to completion, tracking `FunctionStats` per function.
//! "Falls out of CALL/RETURN/COUNT-equivalent events almost for free" per
//! the spec - built directly on the same `debug_frames`/
//! `step_with_granularity` primitives `session.rs` uses, no new fork
//! surface needed.
//!
//! `totalTime`/`selfTime` are instruction counts, not wall-clock time -
//! this codebase already treats "instructions executed" as its timing
//! proxy everywhere else (`MAX_INSTRUCTIONS`, the runaway-loop guard), and
//! wall-clock time would vary run to run for reasons that have nothing to
//! do with the Lua program (JIT warmup, host machine load), which would
//! make a profile misleading in exactly the way instruction counts don't.

use std::collections::HashMap;
use std::rc::Rc;

use vm::compiler::FunctionRef;
use vm::{Closure, DebugFrame, Executor, Fuel, Lua, StashedExecutor};
use wasm_bindgen::prelude::*;

use crate::{
    extend_table_library, install_print, install_require, install_xpcall, strip_chunk_prefix,
    MAX_INSTRUCTIONS,
};

#[wasm_bindgen]
#[derive(Debug, Clone)]
pub struct FunctionStats {
    function_id: std::string::String,
    calls: u32,
    total_instructions: u64,
    self_instructions: u64,
}

#[wasm_bindgen]
impl FunctionStats {
    #[wasm_bindgen(getter)]
    pub fn function_id(&self) -> std::string::String {
        self.function_id.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn calls(&self) -> u32 {
        self.calls
    }
    /// Instructions executed while this function was anywhere on the call
    /// stack (i.e. including time spent in callees it made).
    #[wasm_bindgen(getter)]
    pub fn total_instructions(&self) -> f64 {
        self.total_instructions as f64
    }
    /// Instructions executed while this function was the *innermost*
    /// active frame (excluding time spent in callees).
    #[wasm_bindgen(getter)]
    pub fn self_instructions(&self) -> f64 {
        self.self_instructions as f64
    }
}

#[derive(Default)]
struct Accum {
    calls: u32,
    total_instructions: u64,
    self_instructions: u64,
}

struct ActiveCall {
    function_id: std::string::String,
    start: u64,
    child_instructions: u64,
}

/// Builds a `function_id` from `function`'s *own* prototype - deliberately
/// not from whatever chunk started the run. `require()`d files each
/// compile to their own top-level `FunctionRef::Chunk` prototype (with
/// their own `proto.chunk_name`), so a function from `lib.lua` must not be
/// attributed to `main.lua` just because `main.lua` was the entry point -
/// an earlier version of this function took the entry chunk's name as a
/// parameter and used it unconditionally, which silently merged every
/// required file's top-level chunk into the entry chunk's own stats
/// (caught by `profile_project_supports_require_across_files`, which
/// failed with the entry chunk showing 2 calls instead of 1 - one for
/// itself, one for the required file's chunk that got misattributed to it).
fn function_id(function: Closure) -> std::string::String {
    let proto = function.prototype();
    let chunk_name = strip_chunk_prefix(&std::string::String::from_utf8_lossy(
        proto.chunk_name.as_bytes(),
    ))
    .to_string();
    match proto.reference {
        FunctionRef::Named(name, line) => format!(
            "{chunk_name}:{} {}",
            line.0 + 1,
            std::string::String::from_utf8_lossy(name.as_bytes())
        ),
        FunctionRef::Expression(line) => format!("{chunk_name}:{} <anonymous>", line.0 + 1),
        FunctionRef::Chunk => chunk_name,
    }
}

/// Runs `source` to completion (capped at [`MAX_INSTRUCTIONS`], same as
/// `execute()`) and returns per-function call/instruction stats. Errors
/// during the run are silently reflected as an incomplete profile (whatever
/// ran before the error) rather than a `Result`, matching how a profiler
/// is normally used - "show me what happened," not "did it succeed."
///
/// Single-file only - no `require()` (see `profile_project` for a
/// multi-file virtual-FS project, mirroring `execute`/`execute_project`'s
/// split in lib.rs).
#[wasm_bindgen]
pub fn profile(source: &str, chunk_name: &str) -> Vec<FunctionStats> {
    let mut lua = Lua::core();
    let executor = lua.try_enter(|ctx| {
        install_print(ctx, new_output_buffer())?;
        extend_table_library(ctx)?;
        install_xpcall(ctx);
        let closure = vm::Closure::load(ctx, Some(chunk_name), source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });
    let Ok(executor) = executor else {
        return Vec::new();
    };
    run_profile(lua, executor, chunk_name)
}

/// Same as [`profile`], but for a multi-file project - mirrors
/// `execute_project`'s `require()`/virtual-FS setup (see `install_require`
/// in lib.rs). `entry` is executed as the main chunk; `names[i]`/
/// `contents[i]` become available to `require()`.
#[wasm_bindgen]
pub fn profile_project(
    names: Vec<std::string::String>,
    contents: Vec<std::string::String>,
    entry: &str,
) -> Vec<FunctionStats> {
    let file_map: Rc<HashMap<std::string::String, std::string::String>> =
        Rc::new(names.into_iter().zip(contents).collect());
    let Some(entry_source) = file_map.get(entry).cloned() else {
        return Vec::new();
    };
    let mut lua = Lua::core();
    let chunk_name = format!("@{entry}");
    let executor = lua.try_enter(|ctx| {
        install_print(ctx, new_output_buffer())?;
        extend_table_library(ctx)?;
        install_xpcall(ctx);
        install_require(ctx, file_map.clone())?;
        let closure = vm::Closure::load(ctx, Some(chunk_name.as_str()), entry_source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });
    let Ok(executor) = executor else {
        return Vec::new();
    };
    run_profile(lua, executor, &chunk_name)
}

/// Shared driving loop for [`profile`]/[`profile_project`] once their
/// (possibly quite different) executor setup is done.
fn run_profile(mut lua: Lua, executor: StashedExecutor, chunk_name: &str) -> Vec<FunctionStats> {
    let main_id = strip_chunk_prefix(chunk_name).to_string();
    let mut stack = vec![ActiveCall {
        function_id: main_id.clone(),
        start: 0,
        child_instructions: 0,
    }];
    let mut stats: HashMap<std::string::String, Accum> = HashMap::new();
    stats.entry(main_id).or_default().calls += 1;

    let mut total: u64 = 0;
    let mut last_depth = 1usize;

    loop {
        let mut fuel = Fuel::with(1);
        let finished = lua.enter(|ctx| {
            ctx.fetch(&executor)
                .step_with_granularity(ctx, &mut fuel, 1)
        });
        total += 1;

        let (depth, top_id) = lua.enter(|ctx| {
            let Some(thread) = ctx.fetch(&executor).current_running_thread() else {
                return (0, None);
            };
            let Some(frames) = thread.debug_frames() else {
                return (0, None);
            };
            let lua_frames: Vec<_> = frames
                .iter()
                .filter_map(|f| match *f {
                    DebugFrame::Lua { function, .. } => Some(function),
                    DebugFrame::Callback => None,
                })
                .collect();
            let depth = lua_frames.len();
            let top_id = lua_frames.last().map(|f| function_id(*f));
            (depth, top_id)
        });

        if depth > last_depth {
            if let Some(id) = top_id {
                stats.entry(id.clone()).or_default().calls += 1;
                stack.push(ActiveCall {
                    function_id: id,
                    start: total,
                    child_instructions: 0,
                });
            }
        } else if depth < last_depth {
            while stack.len() > depth.max(1) {
                let done = stack.pop().unwrap();
                let duration = total - done.start;
                let self_time = duration.saturating_sub(done.child_instructions);
                let entry = stats.entry(done.function_id).or_default();
                entry.total_instructions += duration;
                entry.self_instructions += self_time;
                if let Some(parent) = stack.last_mut() {
                    parent.child_instructions += duration;
                }
            }
        }
        last_depth = depth.max(1);

        if finished || total >= MAX_INSTRUCTIONS as u64 {
            // Close out whatever is still on the stack (including "main")
            // as if it returned right now, so every call that started gets
            // counted even if the program never formally returns from it
            // (e.g. it errored, or hit the instruction limit).
            while let Some(done) = stack.pop() {
                let duration = total - done.start;
                let self_time = duration.saturating_sub(done.child_instructions);
                let entry = stats.entry(done.function_id).or_default();
                entry.total_instructions += duration;
                entry.self_instructions += self_time;
                if let Some(parent) = stack.last_mut() {
                    parent.child_instructions += duration;
                }
            }
            break;
        }
    }

    stats
        .into_iter()
        .map(|(function_id, acc)| FunctionStats {
            function_id,
            calls: acc.calls,
            total_instructions: acc.total_instructions,
            self_instructions: acc.self_instructions,
        })
        .collect()
}

/// A print-buffer sink `install_print` needs but this module has no use
/// for - a profiler doesn't report `print()` output, just call/instruction
/// stats.
fn new_output_buffer() -> std::rc::Rc<std::cell::RefCell<std::string::String>> {
    std::rc::Rc::new(std::cell::RefCell::new(std::string::String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_calls_and_attributes_self_vs_total_time() {
        let stats = profile(
            "local function inner()\n  local x = 0\n  for i = 1, 100 do x = x + i end\n  return x\nend\nlocal function outer()\n  return inner() + 1\nend\nprint(outer())",
            "prof",
        );
        let inner = stats
            .iter()
            .find(|s| s.function_id.contains("inner"))
            .expect("inner should be profiled");
        assert_eq!(inner.calls, 1);
        assert!(inner.self_instructions > 0);

        let outer = stats
            .iter()
            .find(|s| s.function_id.contains("outer"))
            .expect("outer should be profiled");
        assert_eq!(outer.calls, 1);
        // outer's total time includes the time it spent inside inner();
        // its own self time (just the `+ 1` and return) should be much
        // smaller than inner's loop-heavy self time.
        assert!(outer.total_instructions >= inner.total_instructions);
        assert!(outer.self_instructions < inner.self_instructions);

        let main = stats
            .iter()
            .find(|s| s.function_id == "prof")
            .expect("main chunk");
        assert_eq!(main.calls, 1);
    }

    #[test]
    fn a_function_called_multiple_times_accumulates_across_calls() {
        let stats = profile(
            "local function f(n) return n * 2 end\nfor i = 1, 5 do f(i) end",
            "prof2",
        );
        let f = stats.iter().find(|s| s.function_id.contains(" f")).unwrap();
        assert_eq!(f.calls, 5);
    }

    #[test]
    fn profile_project_supports_require_across_files() {
        let names = vec!["main.lua".to_string(), "lib.lua".to_string()];
        let contents = vec![
            "local lib = require('lib')\nfor i = 1, 3 do lib.go() end".to_string(),
            "local M = {}\nfunction M.go()\n  local x = 1\nend\nreturn M".to_string(),
        ];
        let stats = profile_project(names, contents, "main.lua");
        let go = stats
            .iter()
            .find(|s| s.function_id.contains(" go"))
            .expect("lib.go should be profiled across the require() boundary");
        assert_eq!(go.calls, 3);
        let main = stats
            .iter()
            .find(|s| s.function_id == "main.lua")
            .expect("main chunk");
        assert_eq!(main.calls, 1);
    }
}
