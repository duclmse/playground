// Call stack / inspector (docs/debug-protocol.md#call-stack,
// #value--inspector-model, #scopes--locals).

use vm::{DebugFrame, UpValueState, Value};
use wasm_bindgen::prelude::*;

use crate::{display_value, strip_chunk_prefix};

use super::registry::marshal_value;
use super::types::thread_by_id;
use super::{DebugSession, StackFrame, Variable};

#[wasm_bindgen]
impl DebugSession {
    pub fn get_stack_trace(&mut self, thread_id: u32) -> Vec<StackFrame> {
        self.lua.enter(|ctx| {
            let Some(thread) = thread_by_id(ctx, &self.executor, thread_id) else {
                return Vec::new();
            };
            let Some(frames) = thread.debug_frames() else {
                return Vec::new();
            };
            let lua_frames: Vec<_> = frames
                .iter()
                .filter(|f| matches!(f, DebugFrame::Lua { .. }))
                .collect();
            let lua_count = lua_frames.len();
            let mut out = Vec::new();
            // Walk top-to-bottom (index 0 = current frame), numbering Lua
            // frames the same way `debug_read_register`'s `depth_from_top`
            // does, so `frame_index` here is directly usable in
            // `get_locals`/`evaluate`.
            let mut lua_depth_from_top = 0usize;
            for frame in frames.iter().rev() {
                match *frame {
                    DebugFrame::Lua { function, pc, .. } => {
                        let proto = function.prototype();
                        let line = proto.line_for_pc(pc).map(|l| l.0 as u32 + 1); // LineNumber is 0-indexed
                        let chunk = strip_chunk_prefix(&std::string::String::from_utf8_lossy(
                            proto.chunk_name.as_bytes(),
                        ))
                        .to_string();
                        let is_main = lua_depth_from_top == lua_count - 1;
                        out.push(StackFrame {
                            index: lua_depth_from_top as u32,
                            // piccolo doesn't retain a name for a closure
                            // value itself (functions are anonymous at
                            // runtime - see this file's module docs and
                            // risks.md for the same gap as upvalues); "main"
                            // for the entry chunk, "<chunk:line>" otherwise
                            // is the best available identifier without a
                            // deeper compiler change than this pass scoped.
                            name: if is_main {
                                "main".to_string()
                            } else {
                                format!("{chunk}:{}", line.unwrap_or(0))
                            },
                            source: chunk,
                            line,
                            function_type: if is_main {
                                "main".to_string()
                            } else {
                                "lua".to_string()
                            },
                        });
                        lua_depth_from_top += 1;
                    }
                    DebugFrame::Callback => {
                        out.push(StackFrame {
                            index: u32::MAX, // not addressable via get_locals/evaluate
                            name: "[C function]".to_string(),
                            source: std::string::String::new(),
                            line: None,
                            function_type: "c".to_string(),
                        });
                    }
                }
            }
            out
        })
    }

    /// Locals live at `frame_index`'s current pc (docs/debug-protocol.md's
    /// `Scope { type: "local" }`), named where `local_name_at` resolves a
    /// name and falling back to `R<n>` (the `"register"` scope type the
    /// spec already anticipates) for compiler-internal temporaries that
    /// never had a source-level name.
    pub fn get_locals(&mut self, thread_id: u32, frame_index: u32) -> Vec<Variable> {
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
                    let value = thread.debug_read_register(frame_index as usize, reg)?;
                    let name = proto
                        .local_name_at(pc, vm::types::RegisterIndex(reg as u8))
                        .map(|s| std::string::String::from_utf8_lossy(s.as_bytes()).into_owned())
                        .unwrap_or_else(|| format!("R{reg}"));
                    let mut var = marshal_value(ctx, &mut self.registry, value);
                    var.name = name;
                    Some(var)
                })
                .collect()
        })
    }

    /// Upvalues captured by the closure running at `frame_index` - a scope
    /// docs/debug-protocol.md's model doesn't name separately, but is a
    /// natural sibling of locals/globals in the inspector. Named via
    /// `FunctionPrototype::upvalue_name_at`, the fork patch this crate adds
    /// alongside `local_name_at` for the same reason (piccolo's compiler
    /// knows every upvalue's declared name - `_ENV` included - but discards
    /// it once compilation finishes); falls back to `U<n>` for defensive
    /// symmetry with `get_locals`'s `R<n>`, though that case shouldn't
    /// actually arise (every upvalue the compiler creates has a name).
    pub fn get_upvalues(&mut self, thread_id: u32, frame_index: u32) -> Vec<Variable> {
        self.lua.enter(|ctx| {
            let Some(thread) = thread_by_id(ctx, &self.executor, thread_id) else {
                return Vec::new();
            };
            let Some(frames) = thread.debug_frames() else {
                return Vec::new();
            };
            let Some(DebugFrame::Lua { function, .. }) = frames
                .iter()
                .rev()
                .filter(|f| matches!(f, DebugFrame::Lua { .. }))
                .nth(frame_index as usize)
                .copied()
            else {
                return Vec::new();
            };
            let proto = function.prototype();
            function
                .upvalues()
                .iter()
                .enumerate()
                .map(|(i, upvalue)| {
                    let value = match upvalue.get() {
                        UpValueState::Open(open) => open.get(&ctx),
                        UpValueState::Closed(v) => v,
                    };
                    let name = proto
                        .upvalue_name_at(i)
                        .map(|s| std::string::String::from_utf8_lossy(s.as_bytes()).into_owned())
                        .unwrap_or_else(|| format!("U{i}"));
                    let mut var = marshal_value(ctx, &mut self.registry, value);
                    var.name = name;
                    var
                })
                .collect()
        })
    }

    /// Globals (docs/debug-protocol.md's `Scope { type: "global" }`) -
    /// `ctx.globals()` is a real, publicly-iterable `Table` already, no fork
    /// patch needed for this one.
    pub fn get_globals(&mut self) -> Vec<Variable> {
        self.lua.enter(|ctx| {
            ctx.globals()
                .iter()
                .filter_map(|(k, v)| {
                    if !matches!(k, Value::String(_)) {
                        return None;
                    }
                    let mut var = marshal_value(ctx, &mut self.registry, v);
                    var.name = display_value(k);
                    Some(var)
                })
                .collect()
        })
    }

    /// Lazily expands a table referenced by a previous `Variable.reference`
    /// (docs/debug-protocol.md's "Lazy-load entries" requirement) - `start`/
    /// `count` page through it rather than enumerating everything eagerly.
    pub fn get_table_entries(&mut self, reference: u32, start: u32, count: u32) -> Vec<Variable> {
        self.lua.enter(|ctx| {
            let Some(table) = self.registry.table(ctx, reference) else {
                return Vec::new();
            };
            table
                .iter()
                .skip(start as usize)
                .take(count as usize)
                .map(|(k, v)| {
                    let mut var = marshal_value(ctx, &mut self.registry, v);
                    var.name = display_value(k);
                    var
                })
                .collect()
        })
    }

    /// The metatable of a referenced table, if any (docs/debug-protocol.md's
    /// "Metatables" tree node) - `None` when there isn't one or `reference`
    /// isn't a table.
    pub fn get_metatable(&mut self, reference: u32) -> Option<u32> {
        self.lua.enter(|ctx| {
            let table = self.registry.table(ctx, reference)?;
            let meta = table.metatable()?;
            self.registry.register(ctx, Value::Table(meta))
        })
    }
}
