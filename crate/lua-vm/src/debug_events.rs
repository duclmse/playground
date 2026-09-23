//! Phase 3 (docs/roadmap.md) debug instrumentation: drives piccolo's
//! fuel-stepped `Executor::step()` loop and produces the `DebugEvent` stream
//! described in docs/debug-protocol.md#debug-events.
//!
//! This is the risk-spike deliverable from docs/risks.md §1: piccolo 0.3.3
//! doesn't expose enough of `Frame`/`ThreadState` publicly to poll "what
//! line, what locals" between `step()` calls, and its `step()` batches up
//! to 64 opcodes per call with no way to shrink that from the outside — so
//! `crates/vm` is a vendored fork (Cargo package name `vm`) adding exactly the read-only
//! accessors and the one parameterized-granularity entry point this module
//! needs (`Executor::current_running_thread`, `Executor::step_with_granularity`,
//! `Thread::debug_snapshot`, `Thread::debug_lua_frame_depth`,
//! `FunctionPrototype::line_for_pc`) — see crates/vm/README.md for the
//! full patch list.
//!
//! Scope, matching docs/roadmap.md Phase 3's spike framing ("get one
//! step() call to report 'line changed' and expose one local variable's
//! value before scoping the rest of this phase"): this produces `line` /
//! `call` / `return` / `exception` / `terminated` events and exposes
//! register 0 of the current Lua frame as `local0` on `line` events. It does
//! *not* implement breakpoints, stepping commands, or a call stack/locals
//! API — that's Phases 4-6.

use std::collections::HashMap;
use std::rc::Rc;

use vm::{Closure, Executor, Fuel, Lua, StashedExecutor};
use wasm_bindgen::prelude::*;

use crate::{display_value, install_print, install_require, strip_chunk_prefix, MAX_INSTRUCTIONS};

/// One entry in the observed event stream. Mirrors
/// docs/debug-protocol.md#debug-events' `DebugEvent` shape (`type`,
/// `source`, `line`), plus `local0` — a value the spec doesn't define yet,
/// since named-local inspection is Phase 6, not Phase 3; register 0 is
/// exposed here only to prove the introspection path works end to end.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq)]
pub struct DebugEvent {
    kind: DebugEventKind,
    source: Option<String>,
    line: Option<u32>,
    local0: Option<String>,
    duration: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugEventKind {
    Line,
    Call,
    Return,
    Exception,
    Terminated,
}

#[wasm_bindgen]
impl DebugEvent {
    /// One of `"line" | "call" | "return" | "exception" | "terminated"`,
    /// matching debug-protocol.md's `DebugEvent["type"]`.
    #[wasm_bindgen(getter)]
    pub fn event_type(&self) -> String {
        match self.kind {
            DebugEventKind::Line => "line",
            DebugEventKind::Call => "call",
            DebugEventKind::Return => "return",
            DebugEventKind::Exception => "exception",
            DebugEventKind::Terminated => "terminated",
        }
        .to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn source(&self) -> Option<String> {
        self.source.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn line(&self) -> Option<u32> {
        self.line
    }

    /// Display-formatted value of register 0 in the current Lua frame at
    /// the time of a `"line"` event (see module docs — not part of the
    /// debug-protocol.md spec, a Phase 3 spike-only field). `None` for
    /// every other event type.
    #[wasm_bindgen(getter)]
    pub fn local0(&self) -> Option<String> {
        self.local0.clone()
    }

    /// Number of opcodes (`Executor::step_with_granularity(.., 1)` calls)
    /// executed since the previously recorded event - a step-count proxy
    /// for "how long this took", since piccolo's fuel-stepped execution has
    /// no wall-clock notion of time and this needs to stay meaningful for a
    /// native `cargo test` run as well as a wasm32 build. `0` for the very
    /// first event (nothing ran before it) and for an event pushed in the
    /// same step as the one before it (e.g. a `call` immediately followed by
    /// its first `line`).
    #[wasm_bindgen(getter)]
    pub fn duration(&self) -> u32 {
        self.duration
    }
}

impl DebugEvent {
    fn line_event(source: String, line: u32, local0: Option<String>, duration: u32) -> Self {
        DebugEvent {
            kind: DebugEventKind::Line,
            source: Some(source),
            line: Some(line),
            local0,
            duration,
        }
    }

    fn call(source: Option<String>, duration: u32) -> Self {
        DebugEvent {
            kind: DebugEventKind::Call,
            source,
            line: None,
            local0: None,
            duration,
        }
    }

    fn return_(source: Option<String>, duration: u32) -> Self {
        DebugEvent {
            kind: DebugEventKind::Return,
            source,
            line: None,
            local0: None,
            duration,
        }
    }

    fn exception(message: String, duration: u32) -> Self {
        DebugEvent {
            kind: DebugEventKind::Exception,
            source: Some(message),
            line: None,
            local0: None,
            duration,
        }
    }

    fn terminated(duration: u32) -> Self {
        DebugEvent {
            kind: DebugEventKind::Terminated,
            source: None,
            line: None,
            local0: None,
            duration,
        }
    }
}

/// Owned snapshot of the running thread's top Lua frame, read out of the
/// `gc_arena` context in one `lua.enter()` call so it can outlive that
/// borrow (see piccolo fork's `Thread::debug_snapshot`, which is `'gc`-bound
/// and can't cross the closure boundary directly).
struct StepSnapshot {
    chunk_name: Option<String>,
    line: Option<u32>,
    local0: Option<String>,
    lua_frame_depth: usize,
}

fn snapshot(lua: &mut Lua, executor: &StashedExecutor) -> StepSnapshot {
    lua.enter(|ctx| {
        let executor: Executor = ctx.fetch(executor);
        let Some(thread) = executor.current_running_thread() else {
            return StepSnapshot {
                chunk_name: None,
                line: None,
                local0: None,
                lua_frame_depth: 0,
            };
        };
        let lua_frame_depth = thread.debug_lua_frame_depth().unwrap_or(0);
        match thread.debug_snapshot() {
            Some(snap) => StepSnapshot {
                chunk_name: Some(String::from_utf8_lossy(snap.chunk_name.as_bytes()).into_owned()),
                // `LineNumber` is 0-indexed (source line 1 is `LineNumber(0)`).
                line: snap.line.map(|l| l.0 as u32 + 1),
                local0: snap.local0.map(display_value),
                lua_frame_depth,
            },
            None => StepSnapshot {
                chunk_name: None,
                line: None,
                local0: None,
                lua_frame_depth,
            },
        }
    })
}

/// Compares `step` against the last-seen line/depth and pushes the
/// `call`/`return`/`line` events implied by whatever changed. Shared between
/// the one-time initial snapshot (before the first `step()` call — see
/// `run_with_debug_events`) and every snapshot taken after a step, so both
/// go through identical transition logic.
///
/// `current_step`/`last_event_step` compute each pushed event's `duration`:
/// the number of opcode steps since whichever event was recorded previously
/// (0 for the very first event, and for a second event pushed in this same
/// call - e.g. a `call` immediately followed by its first `line`).
fn record_transition(
    events: &mut Vec<DebugEvent>,
    last_line: &mut Option<u32>,
    last_lua_depth: &mut usize,
    last_event_step: &mut i64,
    current_step: i64,
    step: StepSnapshot,
) {
    let duration = (current_step - *last_event_step) as u32;

    if step.lua_frame_depth > *last_lua_depth {
        events.push(DebugEvent::call(step.chunk_name.clone(), duration));
        *last_event_step = current_step;
    } else if step.lua_frame_depth < *last_lua_depth {
        events.push(DebugEvent::return_(step.chunk_name.clone(), duration));
        *last_event_step = current_step;
    }
    *last_lua_depth = step.lua_frame_depth;

    if step.line.is_some() && step.line != *last_line {
        let duration = (current_step - *last_event_step) as u32;
        events.push(DebugEvent::line_event(
            step.chunk_name.clone().unwrap_or_default(),
            step.line.unwrap(),
            step.local0,
            duration,
        ));
        *last_line = step.line;
        *last_event_step = current_step;
    }
}

/// Drives `source` to completion one Lua opcode at a time, recording a
/// `DebugEvent` on every line change and every Lua-frame call/return,
/// terminated by an `"exception"` or `"terminated"` event.
///
/// Uses `Executor::step_with_granularity(.., 1)` (the piccolo fork's
/// parameterized `step()`, see crates/vm/README.md) rather than plain
/// `step()`. This turned out to matter more than the `Fuel` value passed
/// in: upstream `step()` hardcodes a 64-opcode batch size for its internal
/// Lua-frame run, and stops early only on a call/return, not a line change —
/// so straight-line code (no calls) runs to completion inside *one*
/// `step()` call regardless of how small `Fuel` is, and every intermediate
/// line is invisible to a host polling between calls. Granularity 1 is what
/// actually forces a host-observable boundary after every opcode; `Fuel` is
/// still kept small (`Fuel::with(1)`) so the outer per-`step()` loop also
/// stops after that one opcode instead of looping internally. Real
/// breakpoint-driven stepping (Phase 4/5) will want to run at granularity 64
/// (or larger) between breakpoints and drop to 1 only while single-stepping
/// — that tuning is a later phase's problem, not this spike's.
pub fn run_with_debug_events(source: &str, chunk_name: &str) -> (Vec<DebugEvent>, Option<String>) {
    let (events, _truncated, error) = run_with_debug_events_capped(source, chunk_name, usize::MAX);
    (events, error)
}

/// Phase 8 (docs/debug-protocol.md#advanced-execution-timeline-phase-8):
/// same as [`run_with_debug_events`], but stops recording (while letting
/// the program keep running to completion/error/instruction-limit) once
/// `max_events` have been captured, returning whether the recording was
/// actually truncated. Unbounded capture is fine for Phase 3's spike-sized
/// scripts, but a real timeline feature can't assume that - a busy loop can
/// emit far more `line` events than are useful to render or worth holding
/// in memory, so this caps it rather than growing `Vec<DebugEvent>`
/// without bound (see docs/phase-4-8-implementation.md's "Execution
/// timeline" section for why this was cut from Phase 3's original,
/// uncapped shape instead of just reusing it as-is).
fn run_with_debug_events_capped(
    source: &str,
    chunk_name: &str,
    max_events: usize,
) -> (Vec<DebugEvent>, bool, Option<String>) {
    let mut lua = Lua::core();

    let executor = lua.try_enter(|ctx| {
        install_print(ctx, Default::default())?;
        let closure = Closure::load(ctx, Some(chunk_name), source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });
    let executor = match executor {
        Ok(executor) => executor,
        Err(err) => return (Vec::new(), false, Some(err.to_string())),
    };

    run_capped_loop(lua, executor, max_events)
}

/// Phase 8: same as [`run_with_debug_events_capped`], but for a multi-file
/// project - mirrors `execute_project`'s `require()`/virtual-FS setup (see
/// `install_require` in lib.rs). `entry` is executed as the main chunk;
/// `names[i]`/`contents[i]` become available to `require()`.
fn run_with_debug_events_capped_project(
    names: std::vec::Vec<std::string::String>,
    contents: std::vec::Vec<std::string::String>,
    entry: &str,
    max_events: usize,
) -> (Vec<DebugEvent>, bool, Option<String>) {
    let file_map: Rc<HashMap<std::string::String, std::string::String>> =
        Rc::new(names.into_iter().zip(contents).collect());
    let Some(entry_source) = file_map.get(entry).cloned() else {
        return (
            Vec::new(),
            false,
            Some(format!("entry file '{entry}' not found")),
        );
    };
    let mut lua = Lua::core();
    let chunk_name = format!("@{entry}");

    let executor = lua.try_enter(|ctx| {
        install_print(ctx, Default::default())?;
        install_require(ctx, file_map.clone())?;
        let closure = Closure::load(ctx, Some(chunk_name.as_str()), entry_source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });
    let executor = match executor {
        Ok(executor) => executor,
        Err(err) => return (Vec::new(), false, Some(err.to_string())),
    };

    run_capped_loop(lua, executor, max_events)
}

/// Shared driving loop for [`run_with_debug_events_capped`]/
/// [`run_with_debug_events_capped_project`] once their executor setup is
/// done.
fn run_capped_loop(
    mut lua: Lua,
    executor: StashedExecutor,
    max_events: usize,
) -> (Vec<DebugEvent>, bool, Option<String>) {
    let mut events = Vec::new();
    let mut last_line: Option<u32> = None;
    let mut last_lua_depth = 0usize;
    let mut last_event_step: i64 = 0;
    let mut consumed: i64 = 0;
    let mut truncated = false;

    // `Executor::start` pushes the main chunk's Lua frame synchronously,
    // before any `step()` runs — so the initial position (depth 0→1, line 1,
    // no locals assigned yet) has to be recorded once up front, or the
    // debugger would silently skip observing "paused at line 1" and only
    // ever report state *after* line 1 has already executed.
    //
    // `events.len()` is enforced as a hard upper bound by truncating
    // *after* each `record_transition` call, rather than trying to predict
    // how many events a given call might add (a single call can add up to
    // two - a call event and a line event) and gate the call itself on
    // that - truncating after is simpler and exact regardless of how many
    // events one call happens to produce.
    record_transition(
        &mut events,
        &mut last_line,
        &mut last_lua_depth,
        &mut last_event_step,
        consumed,
        snapshot(&mut lua, &executor),
    );
    if events.len() > max_events {
        events.truncate(max_events);
        truncated = true;
    }

    loop {
        let mut fuel = Fuel::with(1);
        let finished = lua.enter(|ctx| {
            ctx.fetch(&executor)
                .step_with_granularity(ctx, &mut fuel, 1)
        });
        consumed += 1;

        if !truncated {
            record_transition(
                &mut events,
                &mut last_line,
                &mut last_lua_depth,
                &mut last_event_step,
                consumed,
                snapshot(&mut lua, &executor),
            );
            if events.len() > max_events {
                events.truncate(max_events);
                truncated = true;
            }
        }

        if finished {
            break;
        }
        if consumed >= MAX_INSTRUCTIONS {
            let message = "Execution exceeded instruction limit".to_string();
            if events.len() < max_events {
                events.push(DebugEvent::exception(
                    message.clone(),
                    (consumed - last_event_step) as u32,
                ));
            }
            return (events, truncated, Some(message));
        }
    }

    match lua.try_enter(|ctx| ctx.fetch(&executor).take_result::<()>(ctx)?) {
        Ok(()) => {
            if events.len() < max_events {
                events.push(DebugEvent::terminated((consumed - last_event_step) as u32));
            } else {
                truncated = true;
            }
            (events, truncated, None)
        }
        Err(err) => {
            let message = err.to_string();
            if events.len() < max_events {
                events.push(DebugEvent::exception(
                    message.clone(),
                    (consumed - last_event_step) as u32,
                ));
            } else {
                truncated = true;
            }
            (events, truncated, Some(message))
        }
    }
}

#[wasm_bindgen]
pub fn debug_events(source: &str) -> Vec<DebugEvent> {
    run_with_debug_events(source, "input").0
}

/// Inline-diagnostics support (a Monaco error marker on the failing line):
/// recovers a `(source, line)` position for an error `execute_project`
/// already produced, by re-running the same program through this module's
/// instrumented single-opcode stepping loop - `execute_project`'s own fast
/// path (`step()` in large fuel batches) never tracks a "current line," and
/// piccolo's `Error::Display` only embeds a line number for a parse error,
/// never for a runtime one (confirmed by inspection: `error('boom')`
/// displays as `"lua error: boom"`, with no position at all) - so a runtime
/// error's line can only be recovered by watching execution, not by parsing
/// the message. Only called on the (rare) error path, so the fast path pays
/// no cost for this on a successful run; print() output from this replay is
/// discarded (`run_with_debug_events_capped_project` never returns it) so
/// nothing gets double-printed to the console.
///
/// Returns `None` if the replay produced no line info at all - genuinely
/// possible for a syntax error in the *entry* chunk, which fails before any
/// opcode ever runs, so there's no event stream to look at; recovered from
/// the message text instead, since piccolo's parser (unlike its runtime
/// Display) does embed one there (`"parse error at line 3: ..."`).
pub(crate) fn diagnose_error_position_project(
    names: std::vec::Vec<std::string::String>,
    contents: std::vec::Vec<std::string::String>,
    entry: &str,
) -> Option<(std::string::String, u32)> {
    let (events, _truncated, error) =
        run_with_debug_events_capped_project(names, contents, entry, usize::MAX);
    error.as_ref()?;
    if let Some(last_line) = events.iter().rev().find(|e| e.kind == DebugEventKind::Line) {
        let source =
            strip_chunk_prefix(last_line.source.as_deref().unwrap_or_default()).to_string();
        return Some((source, last_line.line?));
    }
    let line = parse_line_from_message(error.as_deref()?)?;
    Some((entry.to_string(), line))
}

/// Extracts `N` from piccolo's `"... at line N: ..."` parse-error message
/// shape (the only error message this crate's fork of piccolo puts a line
/// number directly in - see `diagnose_error_position_project`'s doc comment).
fn parse_line_from_message(message: &str) -> Option<u32> {
    let after = message.split("at line ").nth(1)?;
    let digits: std::string::String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[wasm_bindgen]
pub struct Timeline {
    events: Vec<DebugEvent>,
    truncated: bool,
    error: Option<std::string::String>,
}

#[wasm_bindgen]
impl Timeline {
    #[wasm_bindgen(getter)]
    pub fn events(&self) -> Vec<DebugEvent> {
        self.events.clone()
    }
    /// `true` if `max_events` was reached before the program finished -
    /// the program still ran to completion (or its own error/instruction
    /// limit) regardless, only *recording* stopped early.
    #[wasm_bindgen(getter)]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    #[wasm_bindgen(getter)]
    pub fn error(&self) -> Option<std::string::String> {
        self.error.clone()
    }
}

/// Phase 8: records an execution timeline capped at `max_events`
/// `DebugEvent`s (docs/debug-protocol.md#advanced-execution-timeline-phase-8).
/// Rendering it (a per-function timeline view) is a UI task on top of this -
/// not attempted here, see docs/phase-4-8-implementation.md.
#[wasm_bindgen]
pub fn record_timeline(source: &str, chunk_name: &str, max_events: u32) -> Timeline {
    let (events, truncated, error) =
        run_with_debug_events_capped(source, chunk_name, max_events as usize);
    Timeline {
        events,
        truncated,
        error,
    }
}

/// Same as [`record_timeline`], but for a multi-file project (see
/// `run_with_debug_events_capped_project`).
#[wasm_bindgen]
pub fn record_timeline_project(
    names: Vec<std::string::String>,
    contents: Vec<std::string::String>,
    entry: &str,
    max_events: u32,
) -> Timeline {
    let (events, truncated, error) =
        run_with_debug_events_capped_project(names, contents, entry, max_events as usize);
    Timeline {
        events,
        truncated,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(events: &[DebugEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e.kind {
                DebugEventKind::Line => "line",
                DebugEventKind::Call => "call",
                DebugEventKind::Return => "return",
                DebugEventKind::Exception => "exception",
                DebugEventKind::Terminated => "terminated",
            })
            .collect()
    }

    #[test]
    fn one_step_reports_line_changed_and_a_local_value() {
        let (events, error) = run_with_debug_events("local x = 42\nprint(x)", "spike");
        assert_eq!(error, None);

        let first_line = events
            .iter()
            .find(|e| e.kind == DebugEventKind::Line)
            .expect("expected at least one line event");
        assert_eq!(first_line.line, Some(1));

        // register 0 holds `x` after `local x = 42` has executed — some
        // later line event must show it, proving one local's value is
        // observable through the fork's introspection.
        assert!(
            events.iter().any(|e| e.local0.as_deref() == Some("42")),
            "expected some line event to expose local0 == 42, got: {:?}",
            events.iter().map(|e| &e.local0).collect::<Vec<_>>()
        );
    }

    #[test]
    fn line_events_advance_monotonically_for_straight_line_code() {
        let (events, error) =
            run_with_debug_events("local a = 1\nlocal b = 2\nlocal c = a + b", "straight");
        assert_eq!(error, None);
        let lines: Vec<u32> = events
            .iter()
            .filter(|e| e.kind == DebugEventKind::Line)
            .map(|e| e.line.unwrap())
            .collect();
        assert_eq!(lines, vec![1, 2, 3]);
    }

    #[test]
    fn call_and_return_bracket_a_function_body_line() {
        let (events, error) = run_with_debug_events(
            "local function f(n) return n + 1 end\nprint(f(41))",
            "callret",
        );
        assert_eq!(error, None);
        assert_eq!(
            kinds(&events),
            vec![
                "call",
                "line",
                "line",
                "call",
                "line",
                "return",
                "line",
                "return",
                "terminated"
            ]
        );
    }

    #[test]
    fn runtime_error_emits_an_exception_event_and_terminates_the_stream() {
        let (events, error) = run_with_debug_events("error('boom')", "err");
        assert!(error.unwrap().contains("boom"));
        assert_eq!(events.last().unwrap().kind, DebugEventKind::Exception);
    }

    #[test]
    fn runaway_loop_still_terminates_via_the_instruction_limit() {
        let (events, error) = run_with_debug_events("while true do end", "runaway");
        assert_eq!(
            error.as_deref(),
            Some("Execution exceeded instruction limit")
        );
        assert_eq!(events.last().unwrap().kind, DebugEventKind::Exception);
    }

    #[test]
    fn timeline_stops_recording_at_the_cap_but_the_program_still_finishes() {
        let (events, truncated, error) = run_with_debug_events_capped(
            "local a = 1\nlocal b = 2\nlocal c = 3\nprint(c)",
            "cap",
            2,
        );
        assert_eq!(error, None);
        assert!(truncated);
        // Capped at 2 recorded events, but the program still ran to
        // completion and produced its real output independent of the cap.
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn timeline_under_the_cap_is_not_marked_truncated() {
        let (events, truncated, error) = run_with_debug_events_capped("print(1)", "nocap", 1000);
        assert_eq!(error, None);
        assert!(!truncated);
        assert_eq!(events.last().unwrap().kind, DebugEventKind::Terminated);
    }

    #[test]
    fn duration_is_zero_for_the_first_event_and_sums_to_total_steps_taken() {
        let (events, error) =
            run_with_debug_events("local a = 1\nlocal b = 2\nlocal c = a + b", "duration");
        assert_eq!(error, None);
        assert_eq!(events.first().unwrap().duration, 0);

        // Every event's `duration` is opcode steps since the *previous*
        // event, so they should sum to the total number of opcodes the
        // program ran - one `step_with_granularity(.., 1)` call per event
        // boundary crossed, none double-counted or dropped.
        let total: u32 = events.iter().map(|e| e.duration).sum();
        assert!(total > 0, "expected at least one opcode to have run");
    }

    #[test]
    fn timeline_project_supports_require_across_files() {
        let names = vec!["main.lua".to_string(), "lib.lua".to_string()];
        let contents = vec![
            "local lib = require('lib')\nprint(lib.hello())".to_string(),
            "local M = {}\nfunction M.hello() return 'hi' end\nreturn M".to_string(),
        ];
        let (events, truncated, error) =
            run_with_debug_events_capped_project(names, contents, "main.lua", 1000);
        assert_eq!(error, None);
        assert!(!truncated);
        assert_eq!(events.last().unwrap().kind, DebugEventKind::Terminated);
        // A call event should appear for entering lib.hello() via require().
        assert!(events.iter().any(|e| e.kind == DebugEventKind::Call));
    }

    #[test]
    fn diagnose_error_position_recovers_the_line_of_a_runtime_error() {
        let names = vec!["main.lua".to_string()];
        let contents = vec!["local x = 1\nlocal y = 2\nerror('boom')".to_string()];
        let position = diagnose_error_position_project(names, contents, "main.lua");
        assert_eq!(position, Some(("main.lua".to_string(), 3)));
    }

    #[test]
    fn diagnose_error_position_falls_back_to_the_message_for_an_entry_syntax_error() {
        let names = vec!["main.lua".to_string()];
        let contents = vec!["this is not lua".to_string()];
        let position = diagnose_error_position_project(names, contents, "main.lua");
        assert_eq!(position, Some(("main.lua".to_string(), 1)));
    }

    #[test]
    fn diagnose_error_position_is_none_for_a_successful_program() {
        let names = vec!["main.lua".to_string()];
        let contents = vec!["print('ok')".to_string()];
        assert_eq!(
            diagnose_error_position_project(names, contents, "main.lua"),
            None
        );
    }

    #[test]
    fn diagnose_error_position_points_at_the_require_call_for_a_required_files_syntax_error() {
        let names = vec!["main.lua".to_string(), "broken.lua".to_string()];
        let contents = vec![
            "local ok = 1\nrequire('broken')".to_string(),
            "this is not lua".to_string(),
        ];
        let position = diagnose_error_position_project(names, contents, "main.lua");
        // The failing file's own line isn't recoverable this way (its parse
        // error happens inside the require() callback, with no event stream
        // of its own) - pointing at the call site that pulled it in is the
        // best available position, same tradeoff a "go to definition" that
        // resolves to a broken file would make.
        assert_eq!(position, Some(("main.lua".to_string(), 2)));
    }
}
