// Execution control (docs/debug-protocol.md#stepping-algorithms) and the
// private fuel-stepped driving engine underneath it. `drive`/
// `drive_with_budget`/`check_stop` are the heart of this whole crate's
// debugger: see `drive_with_budget`'s doc comment for the granularity-1
// stepping/dedup design this all rests on.

use vm::Fuel;
use wasm_bindgen::prelude::*;

use crate::{strip_chunk_prefix, MAX_INSTRUCTIONS};

use super::types::{DriveOutcome, FrameSnapshot, StepMode};
use super::{BurstResult, DebugSession, StopEvent, ThreadInfo};

#[wasm_bindgen]
impl DebugSession {
    pub fn continue_(&mut self) -> StopEvent {
        let snap = self.snapshot_top();
        self.drive(StepMode::Continue(snap))
    }

    /// `pause()`'s engine half (docs/phase-4-8-implementation.md's `pause()`
    /// design): runs `Continue` semantics for at most `max_instructions`
    /// opcodes instead of until a real stop, so a caller can interleave
    /// "are we paused yet?" checks between calls - `continue_()` has no
    /// in-flight state to interrupt otherwise, since it's one synchronous
    /// Rust call from launch to stop. Safe to call repeatedly to resume a
    /// burst that ran out of budget without stopping: `StepMode::Continue`'s
    /// `check_stop` doesn't depend on the snapshot `drive_with_budget` takes
    /// at the start of each call, only on the *current* line/breakpoints, so
    /// re-snapshotting every burst is harmless.
    pub fn continue_burst(&mut self, max_instructions: u32) -> BurstResult {
        let snap = self.snapshot_top();
        match self.drive_with_budget(StepMode::Continue(snap), max_instructions as i64) {
            DriveOutcome::Stopped(stop) => BurstResult {
                stopped: true,
                stop: Some(stop),
                source: None,
                line: None,
            },
            DriveOutcome::BudgetExhausted { source, line } => BurstResult {
                stopped: false,
                stop: None,
                source: Some(source),
                line,
            },
        }
    }

    pub fn step_over(&mut self) -> StopEvent {
        let snap = self.snapshot_top();
        self.drive(StepMode::Over(snap))
    }

    pub fn step_into(&mut self) -> StopEvent {
        let snap = self.snapshot_top();
        self.drive(StepMode::Into(snap))
    }

    pub fn step_out(&mut self) -> StopEvent {
        let snap = self.snapshot_top();
        self.drive(StepMode::Out(snap))
    }

    // -- Threads (docs/debug-protocol.md#advanced-coroutines-phase-8) --

    /// The active thread nesting at this pause point (see
    /// `Executor::debug_thread_stack`'s doc comment for exactly what this
    /// does and doesn't cover): `id: 0` is always the main thread; higher
    /// ids are coroutines currently on the resume chain, id `len - 1` (the
    /// last one) has `status: "running"`, everything below it `"normal"`
    /// (Lua's own term for "resumed something and is waiting on it").
    /// `id` is what `get_stack_trace`/`get_locals`/`evaluate`/
    /// `set_variable`'s `thread_id` parameter expects.
    pub fn get_threads(&mut self) -> Vec<ThreadInfo> {
        self.lua.enter(|ctx| {
            let Some(stack) = ctx.fetch(&self.executor).debug_thread_stack() else {
                return Vec::new();
            };
            let len = stack.len();
            (0..len)
                .map(|i| ThreadInfo {
                    id: i as u32,
                    status: if i == len - 1 { "running" } else { "normal" }.to_string(),
                })
                .collect()
        })
    }
}

impl DebugSession {
    /// The `thread_id` (per `get_threads`) of whichever thread is currently
    /// active - the top of `Executor::debug_thread_stack`. Used to evaluate
    /// breakpoint conditions/log messages against the thread that actually
    /// hit the breakpoint, which is a coroutine rather than the main thread
    /// whenever the breakpoint sits inside one.
    pub(super) fn current_thread_id(&mut self) -> u32 {
        self.lua.enter(|ctx| {
            ctx.fetch(&self.executor)
                .debug_thread_stack()
                .map(|stack| stack.len().saturating_sub(1) as u32)
                .unwrap_or(0)
        })
    }

    fn snapshot_top(&mut self) -> FrameSnapshot {
        self.lua.enter(|ctx| {
            let Some(thread) = ctx.fetch(&self.executor).current_running_thread() else {
                return FrameSnapshot {
                    lua_depth: 0,
                    line: None,
                    source: std::string::String::new(),
                };
            };
            let lua_depth = thread.debug_lua_frame_depth().unwrap_or(0);
            let snap = thread.debug_snapshot();
            let line = snap.as_ref().and_then(|s| s.line).map(|l| l.0 as u32 + 1); // LineNumber is 0-indexed
            let source = snap
                .map(|s| {
                    strip_chunk_prefix(&std::string::String::from_utf8_lossy(
                        s.chunk_name.as_bytes(),
                    ))
                    .to_string()
                })
                .unwrap_or_default();
            FrameSnapshot {
                lua_depth,
                line,
                source,
            }
        })
    }

    fn drive(&mut self, mode: StepMode) -> StopEvent {
        match self.drive_with_budget(mode, MAX_INSTRUCTIONS) {
            DriveOutcome::Stopped(stop) => stop,
            // `drive_with_budget` is given the same `MAX_INSTRUCTIONS` cap
            // `drive()` always enforced before `continue_burst` existed, so
            // this arm is exactly that pre-existing runaway-loop guard, not
            // a new behavior - see `MAX_INSTRUCTIONS`'s doc comment in lib.rs.
            DriveOutcome::BudgetExhausted { line, .. } => {
                self.terminated = true;
                self.has_stopped_before = true;
                StopEvent::exception("Execution exceeded instruction limit".to_string(), line)
            }
        }
    }

    /// A source line is usually several opcodes wide, and granularity-1
    /// stepping observes every one of them - so `check_stop` must only run
    /// once per *logical arrival* at a position, not on every one of those
    /// sub-steps, or a single visit double-counts as several (a breakpoint's
    /// hit-count would advance too fast; a `continue()` resuming from a
    /// breakpoint would immediately re-trigger the same one before making
    /// any progress). `last_checked` tracks the (source, line) `check_stop`
    /// last ran against; it only runs again once the observed position
    /// changes away from that. Comparing the full `(source, line)` pair, not
    /// just the line number, matters: two different chunks can legitimately
    /// share a line number (e.g. both happen to have their interesting
    /// statement on line 3), and treating that as "no change" is a real bug
    /// this fixed (see
    /// `breakpoints_are_scoped_to_their_own_file_in_a_multi_file_project`).
    ///
    /// Seeding it with `None` on a fresh launch (rather than `start`) is
    /// deliberate: `start` here is the position we're *about to run*, and a
    /// breakpoint on that very first line must still be able to fire on this
    /// `continue()` - only a *resume from a prior stop* should suppress
    /// re-triggering the position it resumed from, which `has_stopped_before`
    /// distinguishes.
    fn drive_with_budget(&mut self, mode: StepMode, max_instructions: i64) -> DriveOutcome {
        self.registry.reset();
        if self.terminated {
            return DriveOutcome::Stopped(StopEvent::new("terminated", None));
        }
        let start = mode.start();
        let mut last_checked: Option<(std::string::String, Option<u32>)> =
            if self.has_stopped_before {
                Some((start.source.clone(), start.line))
            } else {
                None
            };
        let mut consumed: i64 = 0;
        loop {
            let mut fuel = Fuel::with(1);
            let finished = self.lua.enter(|ctx| {
                ctx.fetch(&self.executor)
                    .step_with_granularity(ctx, &mut fuel, 1)
            });
            consumed += 1;

            let (lua_depth, line, chunk_name) = self.lua.enter(|ctx| {
                let Some(thread) = ctx.fetch(&self.executor).current_running_thread() else {
                    return (0, None, std::string::String::new());
                };
                let depth = thread.debug_lua_frame_depth().unwrap_or(0);
                match thread.debug_snapshot() {
                    Some(snap) => (
                        depth,
                        snap.line.map(|l| l.0 as u32 + 1), // LineNumber is 0-indexed
                        strip_chunk_prefix(&std::string::String::from_utf8_lossy(
                            snap.chunk_name.as_bytes(),
                        ))
                        .to_string(),
                    ),
                    None => (depth, None, std::string::String::new()),
                }
            });

            if finished {
                self.terminated = true;
                self.has_stopped_before = true;
                return DriveOutcome::Stopped(match self.take_result_error() {
                    Some(message) => StopEvent::exception(message, line),
                    None => StopEvent::new("terminated", line),
                });
            }

            let current = (chunk_name.clone(), line);
            if Some(&current) != last_checked.as_ref() {
                last_checked = Some(current);
                if let Some(stop) = self.check_stop(&mode, lua_depth, line, &chunk_name) {
                    self.has_stopped_before = true;
                    return DriveOutcome::Stopped(stop);
                }
            }

            if consumed >= max_instructions {
                return DriveOutcome::BudgetExhausted {
                    source: chunk_name,
                    line,
                };
            }
        }
    }

    fn check_stop(
        &mut self,
        mode: &StepMode,
        lua_depth: usize,
        line: Option<u32>,
        chunk_name: &str,
    ) -> Option<StopEvent> {
        match mode {
            StepMode::Continue(_) => {
                let Some(line) = line else { return None };
                let hit_index = self
                    .breakpoints
                    .iter()
                    .position(|bp| bp.line == line && bp.source_id == chunk_name)?;
                let condition = self.breakpoints[hit_index].condition.clone();
                if let Some(cond) = condition {
                    let thread_id = self.current_thread_id();
                    let result = self.evaluate(thread_id, &cond, 0);
                    if !result.ok || result.display == "false" || result.display == "nil" {
                        return None;
                    }
                }
                let bp = &mut self.breakpoints[hit_index];
                bp.hits += 1;
                if let Some(threshold) = bp.hit_condition {
                    if bp.hits < threshold {
                        return None;
                    }
                }
                if let Some(log_message) = bp.log_message.clone() {
                    let thread_id = self.current_thread_id();
                    let result = self.evaluate(thread_id, &log_message, 0);
                    self.output
                        .borrow_mut()
                        .push_str(&format!("{}\n", result.display));
                    return None;
                }
                Some(StopEvent::new("breakpoint", Some(line)))
            }
            StepMode::Over(start) => {
                if line.is_some() && line != start.line && lua_depth <= start.lua_depth {
                    Some(StopEvent::new("step", line))
                } else {
                    None
                }
            }
            StepMode::Into(start) => {
                if lua_depth > start.lua_depth {
                    Some(StopEvent::new("step", line))
                } else if line.is_some() && line != start.line && lua_depth <= start.lua_depth {
                    Some(StopEvent::new("step", line))
                } else {
                    None
                }
            }
            StepMode::Out(start) => {
                if lua_depth < start.lua_depth {
                    Some(StopEvent::new("step", line))
                } else {
                    None
                }
            }
        }
    }

    fn take_result_error(&mut self) -> Option<std::string::String> {
        self.lua
            .try_enter(|ctx| ctx.fetch(&self.executor).take_result::<()>(ctx)?)
            .err()
            .map(|e| e.to_string())
    }
}
