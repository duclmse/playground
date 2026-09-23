// Mapping from `lua_vm::session` types (JS-facing in the browser, plain
// Rust here) to DAP JSON shapes, plus the two small ID-encoding schemes DAP
// needs that `lua_vm` has no equivalent of:
//
// - DAP `frameId` (used in `stackTrace`'s frame ids, and as `scopes`'/
//   `evaluate`'s `frameId` argument) must be unique across *all* threads,
//   but `lua_vm::StackFrame::index` is only unique *within* one thread's
//   stack (frame 0 exists in every thread). `encode_frame_id`/
//   `decode_frame_id` pack `(thread_id, frame_index)` into one id.
// - DAP `variablesReference` is one shared id space covering both "a
//   table/userdata you can expand" (already numbered by `lua_vm`'s
//   `ObjectRegistry`, small sequential u32s starting at 0) and "a scope
//   container" (Locals/Upvalues/Globals for some frame, which `lua_vm` has
//   no id for at all - `get_locals`/`get_upvalues` take `(thread_id,
//   frame_index)` directly, not a reference). `encode_scope_ref`/
//   `decode_scope_ref` reserve the top bit for synthetic scope references
//   so the two kinds can never collide.

use lua_vm::{Breakpoint, EvalResult, MemoryStats, StackFrame, StopEvent, ThreadInfo, Variable};
use serde_json::{json, Value};

pub fn encode_frame_id(thread_id: u32, frame_index: u32) -> i64 {
    (((thread_id as u64) << 20) | (frame_index as u64 & 0xF_FFFF)) as i64
}

pub fn decode_frame_id(frame_id: i64) -> (u32, u32) {
    let bits = frame_id as u64;
    ((bits >> 20) as u32, (bits & 0xF_FFFF) as u32)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Locals,
    Upvalues,
}

const SCOPE_REF_TAG: u32 = 0x8000_0000;
const GLOBALS_REF: i64 = 0xFFFF_FFFF;

pub fn encode_scope_ref(kind: ScopeKind, thread_id: u32, frame_index: u32) -> i64 {
    let kind_bit: u32 = if kind == ScopeKind::Upvalues {
        0x4000_0000
    } else {
        0
    };
    let thread = (thread_id & 0xFFF) << 12;
    let frame = frame_index & 0xFFF;
    (SCOPE_REF_TAG | kind_bit | thread | frame) as i64
}

pub fn globals_ref() -> i64 {
    GLOBALS_REF
}

pub enum DecodedRef {
    Scope {
        kind: ScopeKind,
        thread_id: u32,
        frame_index: u32,
    },
    Globals,
    Table {
        reference: u32,
    },
}

pub fn decode_variables_ref(reference: i64) -> DecodedRef {
    if reference == GLOBALS_REF {
        return DecodedRef::Globals;
    }
    let bits = reference as u32;
    if bits & SCOPE_REF_TAG == 0 {
        return DecodedRef::Table { reference: bits };
    }
    let kind = if bits & 0x4000_0000 != 0 {
        ScopeKind::Upvalues
    } else {
        ScopeKind::Locals
    };
    let thread_id = (bits >> 12) & 0xFFF;
    let frame_index = bits & 0xFFF;
    DecodedRef::Scope {
        kind,
        thread_id,
        frame_index,
    }
}

pub fn variable_to_dap(v: &Variable) -> Value {
    json!({
        "name": v.name(),
        "value": v.display(),
        "type": v.value_type(),
        "variablesReference": if v.expandable() { v.reference().map(|r| r as i64).unwrap_or(0) } else { 0 },
    })
}

/// `source_path`: the *absolute* path the program was launched from -
/// `f.source()` is only the chunk name (a basename, `lua_vm`'s own
/// breakpoint-matching key), which isn't enough for VS Code to navigate to
/// the file when a user clicks a stack frame.
pub fn stack_frame_to_dap(f: &StackFrame, thread_id: u32, source_path: &str) -> Value {
    json!({
        "id": encode_frame_id(thread_id, f.index()),
        "name": f.name(),
        "line": f.line().unwrap_or(0),
        "column": 0,
        "source": { "name": f.source(), "path": source_path },
    })
}

pub fn thread_to_dap(t: &ThreadInfo) -> Value {
    json!({
        "id": t.id(),
        "name": if t.id() == 0 { "main".to_string() } else { format!("coroutine #{}", t.id()) },
    })
}

pub fn breakpoint_to_dap(bp: &Breakpoint, source_path: &str) -> Value {
    json!({
        "id": bp.id(),
        "verified": bp.verified(),
        "line": bp.line(),
        "source": { "name": bp.source_id(), "path": source_path },
    })
}

pub fn eval_result_ok_body(result: &EvalResult) -> Result<Value, String> {
    if result.ok() {
        Ok(json!({ "result": result.display(), "variablesReference": 0 }))
    } else {
        Err(result.display())
    }
}

pub fn set_variable_ok_body(result: &EvalResult) -> Result<Value, String> {
    if result.ok() {
        Ok(json!({ "value": result.display(), "variablesReference": 0 }))
    } else {
        Err(result.display())
    }
}

pub fn memory_stats_to_dap(stats: &MemoryStats) -> Value {
    json!({
        "totalAllocation": stats.total_allocation(),
        "gcAllocation": stats.gc_allocation(),
        "externalAllocation": stats.external_allocation(),
        "allocationDebt": stats.allocation_debt(),
    })
}

/// Maps a `StopEvent`'s free-text `reason` (`"breakpoint"|"step"|
/// "exception"|"terminated"|"paused"`, per `debug-session.ts`'s `StopEvent`)
/// to DAP's `stopped` event `reason` enum values.
pub fn stop_reason_to_dap(reason: &str) -> &'static str {
    match reason {
        "breakpoint" => "breakpoint",
        "step" => "step",
        "exception" => "exception",
        "paused" => "pause",
        _ => "breakpoint",
    }
}

pub fn stopped_event_body(stop: &StopEvent, thread_id: u32) -> Value {
    let mut body = json!({
        "reason": stop_reason_to_dap(&stop.reason()),
        "threadId": thread_id,
        "allThreadsStopped": true,
    });
    if let Some(message) = stop.message() {
        body["description"] = json!(message);
        body["text"] = json!(message);
    }
    body
}
