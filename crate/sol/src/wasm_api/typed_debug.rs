//! Live specialized frames behind the existing browser debug protocol.
use super::*;
use crate::{
    debugger::live::Evaluated,
    interp::live::{Execution, Stop},
    types::{self, TProgram},
};
use std::collections::BTreeMap;

struct Breakpoint {
    source: String,
    line: u32,
    hits: u32,
    condition: Option<String>,
    hit: Option<u32>,
    log: Option<String>,
}
struct Reference {
    ty: Type,
    words: Box<[u64; 1]>,
    _guard: crate::gc::RootGuard,
}

#[wasm_bindgen]
pub struct WasmTypedDebugSession {
    program: TProgram,
    _engine: crate::tier0::Engine,
    execution: Execution<()>,
    entry: String,
    return_type: Type,
    breakpoints: BTreeMap<u32, Breakpoint>,
    next_breakpoint: u32,
    references: BTreeMap<u32, Reference>,
    evaluations: Vec<Evaluated>,
    visited: Vec<Option<(u64, u32, u32)>>,
    skip_once: bool,
    terminal: Option<(String, Option<String>)>,
    output: String,
}

#[wasm_bindgen]
impl WasmTypedDebugSession {
    pub fn launch_project(
        entry: String,
        names: Vec<String>,
        contents: Vec<String>,
    ) -> Result<Self, JsValue> {
        Self::from_project(entry, names, contents).map_err(|e| JsValue::from_str(&e))
    }
}

impl WasmTypedDebugSession {
    fn from_project(
        entry: String,
        names: Vec<String>,
        contents: Vec<String>,
    ) -> Result<Self, String> {
        if names.len() != contents.len() {
            return Err("project names and contents have different lengths".into());
        }
        let files = names
            .into_iter()
            .zip(contents)
            .map(|(name, source)| (name, source.into_bytes()))
            .collect::<Vec<_>>();
        let (program, return_type) =
            crate::modules::compile_debug_project_from_sources(&entry, &files)?;
        let engine: crate::tier0::Engine = crate::tier0::Engine::new(program.clone(), ())?;
        let execution = engine.start_live("main", &[])?;
        Ok(Self {
            program,
            _engine: engine,
            execution,
            entry,
            return_type,
            breakpoints: BTreeMap::new(),
            next_breakpoint: 1,
            references: BTreeMap::new(),
            evaluations: Vec::new(),
            visited: Vec::new(),
            skip_once: false,
            terminal: None,
            output: String::new(),
        })
    }
}

#[wasm_bindgen]
impl WasmTypedDebugSession {
    pub fn continue_burst(&mut self, instructions: u32) -> LuaDebugStop {
        self.advance(instructions, None)
    }
    pub fn continue_(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, None)
    }
    pub fn step_into(&mut self) -> LuaDebugStop {
        let initial = self.position();
        self.advance(10_000_001, Some((0, initial)))
    }
    pub fn step_over(&mut self) -> LuaDebugStop {
        let initial = self.position();
        self.advance(10_000_001, Some((1, initial)))
    }
    pub fn step_out(&mut self) -> LuaDebugStop {
        let initial = self.position();
        self.advance(10_000_001, Some((2, initial)))
    }

    pub fn set_breakpoint(&mut self, source: String, line: u32) -> WasmBreakpoint {
        let verified = self.program.functions.iter().any(|f| {
            let file = f.source_file.as_deref().unwrap_or(&self.entry);
            (file == source || f.name == source)
                && self._engine.function_bytecode(&f.name).is_some_and(|bc| {
                    (0..bc.code.len()).any(|pc| {
                        bc.source_map
                            .location(pc as u32)
                            .is_some_and(|loc| loc.line == line)
                    })
                })
        });
        let id = self.next_breakpoint;
        self.next_breakpoint += 1;
        self.breakpoints.insert(
            id,
            Breakpoint {
                source: source.clone(),
                line,
                hits: 0,
                condition: None,
                hit: None,
                log: None,
            },
        );
        WasmBreakpoint {
            id,
            function_name: source,
            line,
            verified,
            pc: None,
        }
    }
    pub fn remove_breakpoint(&mut self, id: u32) {
        self.breakpoints.remove(&id);
    }
    pub fn set_breakpoint_condition(&mut self, id: u32, value: Option<String>) {
        if let Some(bp) = self.breakpoints.get_mut(&id) {
            bp.condition = value.filter(|s| !s.trim().is_empty());
        }
    }
    pub fn set_breakpoint_hit_condition(&mut self, id: u32, value: Option<u32>) {
        if let Some(bp) = self.breakpoints.get_mut(&id) {
            bp.hit = value.filter(|n| *n != 0);
        }
    }
    pub fn set_breakpoint_log_message(&mut self, id: u32, value: Option<String>) {
        if let Some(bp) = self.breakpoints.get_mut(&id) {
            bp.log = value;
        }
    }
    pub fn get_threads(&self) -> Vec<WasmThreadInfo> {
        vec![WasmThreadInfo {
            id: 0,
            status: if self.terminal.is_some() {
                "dead"
            } else {
                "suspended"
            }
            .into(),
        }]
    }
    pub fn get_stack_trace(&self, thread: u32) -> Vec<LuaDebugFrame> {
        if thread != 0 {
            return Vec::new();
        }
        self.execution
            .frames()
            .iter()
            .rev()
            .enumerate()
            .map(|(index, f)| LuaDebugFrame {
                index: index as u32,
                name: f.bytecode.metadata.name.clone(),
                source: f
                    .bytecode
                    .source_file
                    .clone()
                    .unwrap_or_else(|| self.entry.clone()),
                line: f
                    .bytecode
                    .source_map
                    .location(f.instruction_pc())
                    .map(|loc| loc.line),
                function_type: "sol".into(),
            })
            .collect()
    }
    pub fn get_locals(&mut self, thread: u32, frame: u32) -> Vec<LuaDebugVariable> {
        if thread != 0 {
            return Vec::new();
        }
        let Ok(index) = self.frame_index(frame) else {
            return Vec::new();
        };
        let f = &self.execution.frames()[index];
        let Some(function) = self
            .program
            .functions
            .iter()
            .find(|function| function.name == f.bytecode.metadata.name)
        else {
            return Vec::new();
        };
        let types = types::collect_local_types(function);
        let locals = f
            .visible_locals()
            .map(|local| {
                (
                    local.name.clone(),
                    types[local.local_id].clone(),
                    f.registers[local.local_id],
                )
            })
            .collect::<Vec<_>>();
        locals
            .into_iter()
            .map(|(name, ty, raw)| self.variable(name, ty, raw))
            .collect()
    }
    // Proven specialized code has no dynamic globals/metatables. Captures
    // are currently lambda-lifted parameters, visible in its real locals.
    pub fn get_upvalues(&self, _thread: u32, _frame: u32) -> Vec<LuaDebugVariable> {
        Vec::new()
    }
    pub fn get_globals(&self) -> Vec<LuaDebugVariable> {
        Vec::new()
    }
    pub fn get_metatable(&self, _reference: u32) -> Option<u32> {
        None
    }
    pub fn get_table_entries(
        &mut self,
        reference: u32,
        start: u32,
        count: u32,
    ) -> Vec<LuaDebugVariable> {
        let Some(value) = self.references.get(&reference) else {
            return Vec::new();
        };
        let values = debugger::ValueRenderer {
            structs: &self.program.structs,
        }
        .expand(&value.ty, value.words[0])
        .unwrap_or_default();
        values
            .into_iter()
            .skip(start as usize)
            .take(count as usize)
            .map(|(name, ty, raw)| self.variable(name, ty, raw))
            .collect()
    }
    pub fn evaluate(&mut self, thread: u32, expression: &str, frame: u32) -> WasmEvalResult {
        let result = if thread != 0 {
            Err("invalid thread".into())
        } else {
            self.eval(frame, expression, None)
        };
        self.eval_result(result, None)
    }
    pub fn set_variable(
        &mut self,
        thread: u32,
        frame: u32,
        name: &str,
        expression: &str,
    ) -> WasmEvalResult {
        let result = (|| {
            if thread != 0 {
                return Err("invalid thread".into());
            }
            let index = self.frame_index(frame)?;
            let f = &self.execution.frames()[index];
            let local = f
                .resolve_local(name)
                .or_else(|| {
                    name.strip_prefix("local")
                        .and_then(|id| id.parse::<usize>().ok())
                        .filter(|id| f.visible_locals().any(|local| local.local_id == *id))
                })
                .ok_or_else(|| format!("unknown local '{name}'"))?;
            let function = self
                .program
                .functions
                .iter()
                .find(|function| function.name == f.bytecode.metadata.name)
                .unwrap();
            let ty = types::collect_local_types(function)[local].clone();
            let value = self.eval(frame, expression, Some(&ty))?;
            Ok((value, index, local))
        })();
        match result {
            Ok((value, index, local)) => self.eval_result(Ok(value), Some((index, local))),
            Err(error) => self.eval_result(Err(error), None),
        }
    }
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }
    pub fn memory_stats(&self) -> WasmMemoryStats {
        WasmMemoryStats {
            live_bytes: crate::gc::live_bytes() as f64,
            live_blocks: crate::gc::live_blocks() as f64,
        }
    }
    /// Collect from retained specialized frames, evaluations and inspector
    /// handles. Native conservative-stack collection keeps its existing API.
    pub fn force_gc(&self) {
        #[cfg(target_arch = "wasm32")]
        unsafe {
            crate::gc::collect_registered_roots();
        }
        #[cfg(not(target_arch = "wasm32"))]
        crate::gc::collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch(source: &str) -> WasmTypedDebugSession {
        WasmTypedDebugSession::from_project(
            "main.sol".into(),
            vec!["main.sol".into()],
            vec![source.into()],
        )
        .unwrap()
    }

    #[test]
    fn pauses_before_execution_and_edits_selected_live_frames() {
        let mut session = launch("function add(x: i64): i64\n local y=x+2\n return y\nend\nfunction main(): i64\n local base: i64=40\n local result=add(base)\n return result+base\nend");
        assert!(session.set_breakpoint("main.sol".into(), 2).verified());
        assert_eq!(session.continue_burst(100).reason, "breakpoint");
        assert_eq!(session.take_output(), "");
        let frames = session.get_stack_trace(0);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].name, "add");
        assert_eq!(frames[1].name, "main");
        assert_eq!(frames[1].line, Some(7));
        assert_eq!(session.evaluate(0, "x", 0).display(), "40");
        assert_eq!(session.evaluate(0, "base", 1).display(), "40");
        assert!(session.set_variable(0, 0, "x", "41").ok());
        assert!(session.set_variable(0, 1, "base", "1").ok());
        assert_eq!(session.continue_burst(100).reason, "terminated");
        assert_eq!(session.take_output(), "44");
    }

    #[test]
    fn aggregate_evaluation_and_typed_edits_preserve_literal_owners() {
        let mut session = launch("struct Record { name: string, value: i64 }\nfunction main(): i64\n local xs: Array<i64> = {40,2}\n local r=Record {name='original',value=1}\n return xs[0]+xs[1]+r.value\nend");
        session.set_breakpoint("main.sol".into(), 5);
        assert_eq!(session.continue_burst(100).reason, "breakpoint");
        assert_eq!(session.evaluate(0, "xs[0]+xs[1]", 0).display(), "42");
        let edit = session.set_variable(0, 0, "r", "Record {name='persist',value=2}");
        assert!(edit.ok(), "{}", edit.display());
        assert_eq!(session.evaluate(0, "r.name", 0).display(), "\"persist\"");
        assert!(!session.set_variable(0, 0, "xs", "true").ok());
        assert_eq!(session.evaluate(0, "xs[0]", 0).display(), "40");
        let xs = session
            .get_locals(0, 0)
            .into_iter()
            .find(|local| local.name == "xs")
            .unwrap();
        assert_eq!(
            session
                .get_table_entries(xs.reference.unwrap(), 0, 10)
                .len(),
            2
        );
        assert_eq!(session.continue_burst(100).reason, "terminated");
        assert_eq!(session.take_output(), "44");
        assert_eq!(
            session.get_table_entries(xs.reference.unwrap(), 0, 10)[0].display,
            "40"
        );
    }

    #[test]
    fn invalid_evaluation_returns_error_without_corrupting_live_execution() {
        let mut session =
            launch("function main(): i64\n local xs: Array<i64> = {42}\n return xs[0]\nend");
        session.set_breakpoint("main.sol".into(), 3);
        assert_eq!(session.continue_burst(100).reason, "breakpoint");
        assert!(!session.evaluate(0, "xs[99]", 0).ok());
        assert!(!session.evaluate(0, "1//0", 0).ok());
        assert!(!session.evaluate(0, "('oops' as any + 1) as i64", 0).ok());
        assert!(!session.evaluate(0, "new_array_i64(-1)", 0).ok());
        assert!(!session
            .evaluate(0, "42 end function extra(): i64 return 0", 0)
            .ok());
        assert!(session.evaluate(0, "xs[0]", 0).ok());
        assert_eq!(session.continue_burst(100).reason, "terminated");
        assert_eq!(session.take_output(), "42");
    }

    #[test]
    fn conditional_hit_count_logpoints_and_stepping_use_live_values() {
        let mut session = launch(
            "function main(): i64\n local n: i64=0\n while n<3 do\n  n=n+1\n end\n return n\nend",
        );
        let bp = session.set_breakpoint("main.sol".into(), 4);
        session.set_breakpoint_hit_condition(bp.id(), Some(2));
        session.set_breakpoint_condition(bp.id(), Some("n == 1".into()));
        assert_eq!(session.continue_burst(100).reason, "breakpoint");
        assert_eq!(session.evaluate(0, "n", 0).display(), "1");
        assert_eq!(session.step_into().reason, "step");
        session.remove_breakpoint(bp.id());
        let log = session.set_breakpoint("main.sol".into(), 6);
        session.set_breakpoint_log_message(log.id(), Some("n={n}".into()));
        assert_eq!(session.continue_burst(100).reason, "terminated");
        assert_eq!(session.take_output(), "n=3\n3");
    }

    #[test]
    fn runaway_execution_yields_without_a_replay_history() {
        let mut session =
            launch("function main(): i64 local n:i64=0 while true do n=n+1 end return n end");
        assert_eq!(session.continue_burst(0).reason, "running");
        for _ in 0..10 {
            assert_eq!(session.continue_burst(100).reason, "running");
        }
        assert_eq!(session.visited.len(), 1);
        assert_eq!(session.get_stack_trace(0).len(), 1);
    }
}

impl WasmTypedDebugSession {
    fn frame_index(&self, frame: u32) -> Result<usize, String> {
        self.execution
            .frames()
            .len()
            .checked_sub(frame as usize + 1)
            .ok_or_else(|| "invalid frame".into())
    }
    fn position(&self) -> Option<(String, u32, usize, u64)> {
        let f = self.execution.frames().last()?;
        Some((
            f.bytecode
                .source_file
                .clone()
                .unwrap_or_else(|| self.entry.clone()),
            f.bytecode
                .source_map
                .location(f.instruction_pc())
                .map(|loc| loc.line)
                .unwrap_or(f.bytecode.source_line),
            self.execution.frames().len(),
            f.identity,
        ))
    }
    fn eval(
        &self,
        frame: u32,
        expression: &str,
        expected: Option<&Type>,
    ) -> Result<Evaluated, String> {
        let f = &self.execution.frames()[self.frame_index(frame)?];
        let function = self
            .program
            .functions
            .iter()
            .find(|function| function.name == f.bytecode.metadata.name)
            .ok_or("unknown frame function")?;
        crate::debugger::live::evaluate(&self.program, function, f, expression, expected)
    }
    fn display(&self, value: &Evaluated) -> String {
        match (debugger::ValueRenderer {
            structs: &self.program.structs,
        })
        .render(&value.ty, value.raw)
        {
            DisplayValue::Scalar(s) => s,
            DisplayValue::Reference { summary, .. } => summary,
        }
    }
    fn eval_result(
        &mut self,
        result: Result<Evaluated, String>,
        destination: Option<(usize, usize)>,
    ) -> WasmEvalResult {
        match result {
            Err(display) => WasmEvalResult { ok: false, display },
            Ok(value) => {
                if let Some((frame, local)) = destination {
                    if let Err(display) = self.execution.set_register(frame, local, value.raw) {
                        return WasmEvalResult { ok: false, display };
                    }
                }
                let display = self.display(&value);
                // Display-only results are copied into the wire string.
                // Only an assigned reference can outlive its evaluator's
                // literal storage; do not grow an owner history for scalar
                // edits or repeated watch-expression reads.
                if destination.is_some()
                    && matches!(
                        value.ty,
                        Type::String | Type::Array(_) | Type::Map(_, _) | Type::Struct(_)
                    )
                {
                    self.evaluations.push(value);
                }
                WasmEvalResult { ok: true, display }
            }
        }
    }
    fn variable(&mut self, name: String, ty: Type, raw: u64) -> LuaDebugVariable {
        let value_type = debugger::type_name(&ty);
        let value = debugger::ValueRenderer {
            structs: &self.program.structs,
        }
        .render(&ty, raw);
        let (display, reference) = match value {
            DisplayValue::Scalar(s) => (s, None),
            DisplayValue::Reference { summary, reference } => {
                let id = self.references.len() as u32 + 1;
                let words = Box::new([reference]);
                let guard = crate::gc::RootGuard::new(words.as_ptr(), words.len());
                self.references.insert(
                    id,
                    Reference {
                        ty,
                        words,
                        _guard: guard,
                    },
                );
                (summary, Some(id))
            }
        };
        LuaDebugVariable {
            name,
            value_type,
            display,
            expandable: reference.is_some(),
            reference,
        }
    }
    fn stop(&self, reason: &str, message: Option<String>) -> LuaDebugStop {
        let position = self.position();
        LuaDebugStop {
            reason: reason.into(),
            line: position.as_ref().map(|p| p.1),
            source: position.map(|p| p.0),
            message,
        }
    }
    fn advance(
        &mut self,
        budget: u32,
        stepping: Option<(u8, Option<(String, u32, usize, u64)>)>,
    ) -> LuaDebugStop {
        if let Some((reason, message)) = &self.terminal {
            return self.stop(reason, message.clone());
        }
        for executed in 0..budget {
            let position = self.position();
            if executed > 0 {
                if let Some((mode, Some(initial))) = &stepping {
                    if let Some(current) = &position {
                        if current != initial
                            && (*mode == 0
                                || (*mode == 1 && current.2 <= initial.2)
                                || (*mode == 2 && current.2 < initial.2))
                        {
                            return self.stop("step", None);
                        }
                    }
                }
            }
            let frames = self.execution.frames();
            self.visited.resize(frames.len(), None);
            let frame = frames.last().unwrap();
            let current = (
                frame.identity,
                frame.instruction_pc(),
                position.as_ref().unwrap().1,
            );
            let previous = self.visited.last_mut().unwrap();
            let candidate = !frame.is_waiting_for_result()
                && previous.is_none_or(|old| {
                    old.0 != current.0 || old.2 != current.2 || current.1 <= old.1
                });
            *previous = Some(current);
            let name = frame.bytecode.metadata.name.clone();
            let skip = std::mem::take(&mut self.skip_once);
            if candidate && !skip {
                let (source, line, _, _) = position.as_ref().unwrap();
                let ids = self
                    .breakpoints
                    .iter()
                    .filter(|(_, bp)| {
                        (bp.source == *source || bp.source == name) && bp.line == *line
                    })
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for id in ids {
                    let bp = self.breakpoints.get_mut(&id).unwrap();
                    bp.hits += 1;
                    if bp.hit.is_some_and(|hit| bp.hits != hit) {
                        continue;
                    }
                    let condition = bp.condition.clone();
                    let log = bp.log.clone();
                    if let Some(condition) = condition {
                        match self.eval(0, &condition, Some(&Type::Bool)) {
                            Ok(value) if value.raw == 0 => continue,
                            Ok(_) => {}
                            Err(error) => return self.stop("exception", Some(error)),
                        }
                    }
                    if let Some(log) = log {
                        let mut remaining = log.as_str();
                        while let Some((literal, after)) = remaining.split_once('{') {
                            self.output.push_str(literal);
                            let Some((expression, rest)) = after.split_once('}') else {
                                self.output.push_str(after);
                                remaining = "";
                                break;
                            };
                            let text = self
                                .eval(0, expression, None)
                                .map(|v| self.display(&v))
                                .unwrap_or_else(|e| format!("<error: {e}>"));
                            self.output.push_str(&text);
                            remaining = rest;
                        }
                        self.output.push_str(remaining);
                        self.output.push('\n');
                    } else {
                        self.skip_once = true;
                        return self.stop("breakpoint", None);
                    }
                }
            }
            match self.execution.resume(1) {
                Stop::Paused => {}
                Stop::Returned(raw) => {
                    self.output.push_str(&render_return(&self.return_type, raw));
                    self.terminal = Some(("terminated".into(), None));
                    return self.stop("terminated", None);
                }
                Stop::Raised(error) => {
                    self.terminal = Some(("exception".into(), Some(error.clone())));
                    return self.stop("exception", Some(error));
                }
            }
        }
        self.stop("running", None)
    }
}
