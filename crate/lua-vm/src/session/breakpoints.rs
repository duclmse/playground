// Breakpoints (docs/debug-protocol.md#breakpoints). `condition`/
// `hit_condition`/`log_message` are Phase 8 fields on the same shape the
// spec already defines for Phase 4 - reused, not bolted on separately (see
// `types::BreakpointState`).

use wasm_bindgen::prelude::*;

use super::types::BreakpointState;
use super::{Breakpoint, DebugSession};

#[wasm_bindgen]
impl DebugSession {
    /// `source_id` must match a chunk name exactly as this session names it
    /// internally - the entry file's own name (as passed to `launch`/
    /// `launch_project`), or (for `require()`d files) the virtual-FS path
    /// used to load them. A mismatched `source_id` is not an error, it's
    /// just a breakpoint that never verifies against anything running.
    pub fn set_breakpoint(&mut self, source_id: &str, line: u32) -> Breakpoint {
        let id = self.next_breakpoint_id;
        self.next_breakpoint_id += 1;
        self.breakpoints.push(BreakpointState {
            id,
            source_id: source_id.to_string(),
            line,
            condition: None,
            hit_condition: None,
            log_message: None,
            hits: 0,
        });
        Breakpoint {
            id,
            source_id: source_id.to_string(),
            line,
            verified: true,
        }
    }

    pub fn remove_breakpoint(&mut self, id: u32) {
        self.breakpoints.retain(|bp| bp.id != id);
    }

    /// Phase 8: conditional breakpoints - only stop when `condition`
    /// evaluates truthy in the paused frame. `None` clears the condition.
    pub fn set_breakpoint_condition(&mut self, id: u32, condition: Option<std::string::String>) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            bp.condition = condition;
        }
    }

    /// Phase 8: hit-count breakpoints - only stop on the Nth hit (and every
    /// hit after, matching common debugger UX - "break after N hits" isn't
    /// usually "break exactly once").
    pub fn set_breakpoint_hit_condition(&mut self, id: u32, hit_condition: Option<u32>) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            bp.hit_condition = hit_condition;
        }
    }

    /// Phase 8: logpoints - log `message` (a Lua expression string,
    /// evaluated and its display appended to `take_output()`) instead of
    /// stopping.
    pub fn set_breakpoint_log_message(
        &mut self,
        id: u32,
        log_message: Option<std::string::String>,
    ) {
        if let Some(bp) = self.breakpoints.iter_mut().find(|bp| bp.id == id) {
            bp.log_message = log_message;
        }
    }
}
