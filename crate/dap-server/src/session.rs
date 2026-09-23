// The adapter's own session state: one `lua_vm::DebugSession` (constructed
// by `launch`, per DAP's launch sequence - not before), plus the
// bookkeeping DAP needs that `lua_vm` doesn't track itself: a per-source
// breakpoint list (for `setBreakpoints`' bulk-replace reconciliation
// against `lua_vm`'s imperative add-one/remove-one API) and the launched
// program's chunk name (breakpoints' `source_id` must match it exactly -
// see `lua_vm::session::breakpoints`'s doc comment).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use lua_vm::DebugSession;
use serde_json::{json, Value};

use crate::convert::{
    breakpoint_to_dap, decode_frame_id, decode_variables_ref, encode_scope_ref,
    eval_result_ok_body, globals_ref, memory_stats_to_dap, set_variable_ok_body,
    stack_frame_to_dap, thread_to_dap, variable_to_dap, DecodedRef, ScopeKind,
};

pub struct AdapterSession {
    debug: Option<DebugSession>,
    /// The absolute path `launch` was given - `lua_vm`'s own chunk name
    /// (used for breakpoint matching) is only the basename, which isn't
    /// enough for a DAP client to navigate to the file from a stack frame.
    program_path: Option<String>,
    /// `source path -> [(dap_line, internal_breakpoint_id)]`, tracked so a
    /// bulk `setBreakpoints` call can be diffed against what's currently
    /// applied instead of blindly re-adding everything.
    breakpoints: HashMap<String, Vec<(u32, u32)>>,
}

impl AdapterSession {
    pub fn new() -> Self {
        AdapterSession {
            debug: None,
            program_path: None,
            breakpoints: HashMap::new(),
        }
    }

    fn debug_mut(&mut self) -> Result<&mut DebugSession, String> {
        self.debug
            .as_mut()
            .ok_or_else(|| "no active session - launch must run first".to_string())
    }

    pub fn is_launched(&self) -> bool {
        self.debug.is_some()
    }

    pub fn take_output(&mut self) -> String {
        self.debug
            .as_mut()
            .map(|d| d.take_output())
            .unwrap_or_default()
    }

    // -- lifecycle ---------------------------------------------------

    pub fn handle_initialize(&self, _args: Value) -> Result<Value, String> {
        Ok(json!({
            "supportsConfigurationDoneRequest": true,
            "supportsConditionalBreakpoints": true,
            "supportsHitConditionalBreakpoints": true,
            "supportsLogPoints": true,
            "supportsEvaluateForHovers": true,
        }))
    }

    /// `launch` args: `{"program": "<path to a .lua file>"}` - v1 supports a
    /// single local file (no `require()`/multi-file project), matching this
    /// pass's explicitly scoped MVP. The chunk name (and every breakpoint's
    /// `source_id`) is the file's basename, mirroring how the browser client
    /// names its entry chunk after the file name it was given.
    pub fn handle_launch(&mut self, args: Value) -> Result<Value, String> {
        let program = args
            .get("program")
            .and_then(Value::as_str)
            .ok_or("launch requires a string \"program\" argument (path to a .lua file)")?;
        let path = Path::new(program);
        let source =
            fs::read_to_string(path).map_err(|e| format!("failed to read '{program}': {e}"))?;
        let chunk_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("launch \"program\" path has no file name")?
            .to_string();
        self.debug = Some(DebugSession::launch(&source, &chunk_name));
        self.program_path = Some(program.to_string());
        self.breakpoints.clear();
        Ok(json!({}))
    }

    // -- breakpoints ---------------------------------------------------

    /// Bulk-reconciles one source's breakpoint list against what's
    /// currently applied (DAP's `setBreakpoints` always sends the *entire*
    /// desired set for that source, not a delta) - added since `lua_vm`
    /// only has imperative `set_breakpoint`/`remove_breakpoint`. Always
    /// re-applies condition/hitCondition/logMessage rather than diffing
    /// those too, since `None` clears them cheaply either way.
    pub fn handle_set_breakpoints(&mut self, args: Value) -> Result<Value, String> {
        let source_path = args
            .get("source")
            .and_then(|s| s.get("path"))
            .and_then(Value::as_str)
            .or_else(|| {
                args.get("source")
                    .and_then(|s| s.get("name"))
                    .and_then(Value::as_str)
            })
            .ok_or("setBreakpoints requires source.path or source.name")?
            .to_string();
        // DAP addresses a source by path; `lua_vm` addresses it by chunk
        // name (basename) - reduce to the same basename lua_vm expects.
        let source_id = Path::new(&source_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&source_path)
            .to_string();

        let requested: Vec<Value> = args
            .get("breakpoints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let existing = self.breakpoints.remove(&source_id).unwrap_or_default();
        let debug = self.debug_mut()?;
        for (_, id) in &existing {
            debug.remove_breakpoint(*id);
        }

        let mut applied = Vec::with_capacity(requested.len());
        let mut tracked = Vec::with_capacity(requested.len());
        for bp_arg in &requested {
            let line = bp_arg.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let bp = debug.set_breakpoint(&source_id, line);
            let condition = bp_arg
                .get("condition")
                .and_then(Value::as_str)
                .map(str::to_string);
            let hit_condition = bp_arg
                .get("hitCondition")
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse::<u32>().ok());
            let log_message = bp_arg
                .get("logMessage")
                .and_then(Value::as_str)
                .map(str::to_string);
            debug.set_breakpoint_condition(bp.id(), condition);
            debug.set_breakpoint_hit_condition(bp.id(), hit_condition);
            debug.set_breakpoint_log_message(bp.id(), log_message);
            tracked.push((line, bp.id()));
            applied.push(breakpoint_to_dap(&bp, &source_path));
        }
        self.breakpoints.insert(source_id, tracked);
        Ok(json!({ "breakpoints": applied }))
    }

    // -- introspection ---------------------------------------------------

    pub fn handle_threads(&mut self) -> Result<Value, String> {
        let threads: Vec<Value> = self
            .debug_mut()?
            .get_threads()
            .iter()
            .map(thread_to_dap)
            .collect();
        Ok(json!({ "threads": threads }))
    }

    pub fn handle_stack_trace(&mut self, args: Value) -> Result<Value, String> {
        let thread_id = args.get("threadId").and_then(Value::as_u64).unwrap_or(0) as u32;
        let source_path = self.program_path.clone().unwrap_or_default();
        let frames: Vec<Value> = self
            .debug_mut()?
            .get_stack_trace(thread_id)
            .iter()
            .map(|f| stack_frame_to_dap(f, thread_id, &source_path))
            .collect();
        let total = frames.len();
        Ok(json!({ "stackFrames": frames, "totalFrames": total }))
    }

    pub fn handle_scopes(&mut self, args: Value) -> Result<Value, String> {
        let frame_id = args
            .get("frameId")
            .and_then(Value::as_i64)
            .ok_or("scopes requires frameId")?;
        let (thread_id, frame_index) = decode_frame_id(frame_id);
        Ok(json!({ "scopes": [
            {
                "name": "Locals",
                "variablesReference": encode_scope_ref(ScopeKind::Locals, thread_id, frame_index),
                "expensive": false,
            },
            {
                "name": "Upvalues",
                "variablesReference": encode_scope_ref(ScopeKind::Upvalues, thread_id, frame_index),
                "expensive": false,
            },
            {
                "name": "Globals",
                "variablesReference": globals_ref(),
                "expensive": true,
            },
        ] }))
    }

    pub fn handle_variables(&mut self, args: Value) -> Result<Value, String> {
        let reference = args
            .get("variablesReference")
            .and_then(Value::as_i64)
            .ok_or("variables requires variablesReference")?;
        let start = args.get("start").and_then(Value::as_u64).unwrap_or(0) as u32;
        let count = args
            .get("count")
            .and_then(Value::as_u64)
            .filter(|&c| c > 0)
            .unwrap_or(100) as u32;
        let debug = self.debug_mut()?;
        let vars = match decode_variables_ref(reference) {
            DecodedRef::Scope {
                kind: ScopeKind::Locals,
                thread_id,
                frame_index,
            } => debug.get_locals(thread_id, frame_index),
            DecodedRef::Scope {
                kind: ScopeKind::Upvalues,
                thread_id,
                frame_index,
            } => debug.get_upvalues(thread_id, frame_index),
            DecodedRef::Globals => debug.get_globals(),
            DecodedRef::Table { reference } => debug.get_table_entries(reference, start, count),
        };
        let variables: Vec<Value> = vars.iter().map(variable_to_dap).collect();
        Ok(json!({ "variables": variables }))
    }

    pub fn handle_evaluate(&mut self, args: Value) -> Result<Value, String> {
        let expression = args
            .get("expression")
            .and_then(Value::as_str)
            .ok_or("evaluate requires expression")?;
        let (thread_id, frame_index) = args
            .get("frameId")
            .and_then(Value::as_i64)
            .map(decode_frame_id)
            .unwrap_or((0, 0));
        let result = self
            .debug_mut()?
            .evaluate(thread_id, expression, frame_index);
        eval_result_ok_body(&result)
    }

    pub fn handle_set_variable(&mut self, args: Value) -> Result<Value, String> {
        let reference = args
            .get("variablesReference")
            .and_then(Value::as_i64)
            .ok_or("setVariable requires variablesReference")?;
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or("setVariable requires name")?;
        let value_expr = args
            .get("value")
            .and_then(Value::as_str)
            .ok_or("setVariable requires value")?;
        let (thread_id, frame_index) = match decode_variables_ref(reference) {
            DecodedRef::Scope {
                thread_id,
                frame_index,
                ..
            } => (thread_id, frame_index),
            _ => return Err("setVariable is only supported on a Locals/Upvalues scope".to_string()),
        };
        let result = self
            .debug_mut()?
            .set_variable(thread_id, frame_index, name, value_expr);
        set_variable_ok_body(&result)
    }

    /// Not a standard DAP request - a small custom extension (DAP allows
    /// adapter-specific requests) exposing the memory-inspection feature
    /// this project's web debugger also has, for a DAP client willing to
    /// send it.
    pub fn handle_memory_stats(&mut self) -> Result<Value, String> {
        Ok(memory_stats_to_dap(&self.debug_mut()?.get_memory_stats()))
    }

    pub fn handle_force_gc(&mut self) -> Result<Value, String> {
        self.debug_mut()?.force_gc();
        Ok(memory_stats_to_dap(&self.debug_mut()?.get_memory_stats()))
    }

    // -- execution (single, synchronous step - see main.rs for the
    // burst-looped, interruptible `continue` handling) ------------------

    pub fn step_over(&mut self) -> Result<lua_vm::StopEvent, String> {
        Ok(self.debug_mut()?.step_over())
    }
    pub fn step_into(&mut self) -> Result<lua_vm::StopEvent, String> {
        Ok(self.debug_mut()?.step_into())
    }
    pub fn step_out(&mut self) -> Result<lua_vm::StopEvent, String> {
        Ok(self.debug_mut()?.step_out())
    }
    pub fn continue_burst(&mut self, max_instructions: u32) -> Result<lua_vm::BurstResult, String> {
        Ok(self.debug_mut()?.continue_burst(max_instructions))
    }

    pub fn current_thread_id(&mut self) -> u32 {
        self.debug
            .as_mut()
            .map(|d| {
                d.get_threads()
                    .into_iter()
                    .find(|t| t.status() == "running")
                    .map(|t| t.id())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }
}

/// Dispatches a synchronous (non-execution) DAP request to its handler.
/// Execution requests (`continue`/`next`/`stepIn`/`stepOut`/`pause`) and
/// lifecycle requests that need to emit events (`configurationDone`,
/// `disconnect`) are handled directly in `main.rs`'s dispatch loop instead,
/// since they need access to the output writer this function doesn't have.
pub fn dispatch_query(
    session: &mut AdapterSession,
    command: &str,
    args: Value,
) -> Option<Result<Value, String>> {
    Some(match command {
        "initialize" => session.handle_initialize(args),
        "launch" => session.handle_launch(args),
        "setBreakpoints" => session.handle_set_breakpoints(args),
        "threads" => session.handle_threads(),
        "stackTrace" => session.handle_stack_trace(args),
        "scopes" => session.handle_scopes(args),
        "variables" => session.handle_variables(args),
        "evaluate" => session.handle_evaluate(args),
        "setVariable" => session.handle_set_variable(args),
        "memoryStats" => session.handle_memory_stats(),
        "forceGc" => session.handle_force_gc(),
        _ => return None,
    })
}
