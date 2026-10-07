//! Browser facade over the checked scalar semantic scheduler.
use super::*;
use crate::bridge::{Layer, MixedExecution};
use std::collections::BTreeMap;
const TYPED_REFERENCE: u32 = 1 << 31;
struct Breakpoint {
    source: String,
    line: u32,
    condition: Option<String>,
    hit: Option<u32>,
    log: Option<String>,
    hits: u32,
}
#[derive(Clone, Copy)]
enum FrameLocation {
    Typed(usize, u32),
    Generic(u32),
}

#[wasm_bindgen]
pub struct WasmMixedDebugSession {
    core: MixedExecution,
    lua: WasmLuaDebugSession,
    typed: Option<WasmTypedDebugSession>,
    entry: String,
    files: Vec<(String, Vec<u8>)>,
    breakpoints: BTreeMap<u32, Breakpoint>,
    next_breakpoint: u32,
    visited: Vec<Vec<Option<(u64, u32, u32)>>>,
    skip_once: bool,
    output: String,
    finalized: bool,
}

impl WasmMixedDebugSession {
    fn build(entry: String, files: Vec<(String, Vec<u8>)>) -> Result<Self, String> {
        let (core, lua) = MixedExecution::from_project(&entry, &files)?;
        let typed = if let Some(function) = core.program.functions.first() {
            let execution = core
                .engine
                .start_live(&function.name, &vec![0; function.params.len()])?;
            Some(WasmTypedDebugSession::from_parts(
                core.program.clone(),
                core.engine.clone(),
                execution,
                entry.clone(),
                function.return_type.clone(),
            ))
        } else {
            None
        };
        Ok(Self {
            core,
            lua: WasmLuaDebugSession::from_inner(lua),
            typed,
            entry,
            files,
            breakpoints: BTreeMap::new(),
            next_breakpoint: 1,
            visited: Vec::new(),
            skip_once: false,
            output: String::new(),
            finalized: false,
        })
    }
    fn routes(&self) -> Vec<FrameLocation> {
        let mut routes = Vec::new();
        let total = self.lua.inner.frames().len();
        let mut end = total;
        for (index, layer) in self.core.layers.iter().enumerate().rev() {
            match layer {
                Layer::Typed(exec) => routes.extend(
                    (0..exec.frames().len()).map(|frame| FrameLocation::Typed(index, frame as u32)),
                ),
                Layer::Generic { base, .. } => {
                    routes.extend(
                        (total - end..total - *base)
                            .map(|frame| FrameLocation::Generic(frame as u32)),
                    );
                    end = *base;
                }
            }
        }
        routes
    }
    fn active_lua_thread(&self) -> u32 {
        self.lua
            .inner
            .threads()
            .last()
            .map(|(id, _)| *id)
            .unwrap_or(0)
    }
    fn with_typed<T>(
        &mut self,
        layer: usize,
        operation: impl FnOnce(&mut WasmTypedDebugSession) -> T,
    ) -> T {
        let Layer::Typed(exec) = &mut self.core.layers[layer] else {
            unreachable!()
        };
        let view = self.typed.as_mut().unwrap();
        std::mem::swap(&mut view.execution, exec);
        let result = operation(view);
        std::mem::swap(&mut view.execution, exec);
        result
    }
    fn tag(values: Vec<LuaDebugVariable>) -> Vec<LuaDebugVariable> {
        values
            .into_iter()
            .map(|mut value| {
                value.reference = value.reference.map(|r| r | TYPED_REFERENCE);
                value
            })
            .collect()
    }
    fn stop(&self, reason: &str, message: Option<String>) -> LuaDebugStop {
        let position = self.core.position(&self.lua.inner);
        LuaDebugStop {
            reason: reason.into(),
            line: position.as_ref().map(|p| p.2),
            source: position.map(|p| {
                if p.1.is_empty() {
                    self.entry.clone()
                } else {
                    p.1
                }
            }),
            message,
        }
    }
    fn terminal_stop(&mut self) -> Option<LuaDebugStop> {
        let result = self.core.terminal.clone()?;
        if !self.finalized {
            self.output.push_str(&self.lua.take_output());
            if self.core.echo_return {
                if let Some(display) = &self.core.root_display {
                    self.output.push_str(display);
                } else if let Ok(Some(value)) = &result {
                    self.output
                        .push_str(&String::from_utf8_lossy(&value.display_bytes()));
                }
            }
            self.finalized = true;
        }
        Some(match result {
            Ok(_) => self.stop("terminated", None),
            Err(error) => self.stop("exception", Some(error)),
        })
    }
    fn candidate(&mut self) -> bool {
        let index = self.core.layers.len().saturating_sub(1);
        self.visited.resize(self.core.layers.len(), Vec::new());
        if let Some(Layer::Typed(exec)) = self.core.layers.last() {
            self.visited[index].resize(exec.frames().len(), None);
            let Some(frame) = exec.frames().last() else {
                return false;
            };
            if frame.is_waiting_for_result() {
                return false;
            }
            let line = frame
                .bytecode
                .source_map
                .location(frame.instruction_pc())
                .map(|p| p.line)
                .unwrap_or(0);
            let current = (frame.identity, frame.instruction_pc(), line);
            let last = self.visited[index].last_mut().unwrap();
            let candidate =
                last.is_none_or(|old| old.0 != current.0 || old.2 != line || current.1 <= old.1);
            *last = Some(current);
            candidate
        } else {
            self.lua.inner.breakpoint_candidate()
        }
    }
    fn advance(&mut self, budget: u32, step: Option<u8>) -> LuaDebugStop {
        if let Some(stop) = self.terminal_stop() {
            return stop;
        }
        let initial = self.core.position(&self.lua.inner);
        for executed in 0..budget {
            let position = self.core.position(&self.lua.inner);
            if executed > 0 && position != initial {
                if let (Some(mode), Some(current), Some(initial)) = (step, &position, &initial) {
                    if mode == 0
                        || (mode == 1 && current.3 <= initial.3)
                        || (mode == 2 && current.3 < initial.3)
                    {
                        return self.stop("step", None);
                    }
                }
            }
            let candidate = self.candidate();
            let skip = std::mem::take(&mut self.skip_once);
            if candidate && !skip {
                if let Some((name, source, line, _)) = &position {
                    let source = if source.is_empty() {
                        &self.entry
                    } else {
                        source
                    };
                    let ids = self
                        .breakpoints
                        .iter()
                        .filter(|(_, bp)| {
                            (bp.source == *source || bp.source == *name) && bp.line == *line
                        })
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>();
                    for id in ids {
                        let bp = self.breakpoints.get_mut(&id).unwrap();
                        bp.hits += 1;
                        if bp.hit.is_some_and(|hit| hit != bp.hits) {
                            continue;
                        }
                        let condition = bp.condition.clone();
                        let log = bp.log.clone();
                        if let Some(condition) = condition {
                            let value = self.evaluate(0, &condition, 0);
                            if !value.ok {
                                return self.stop("exception", Some(value.display));
                            }
                            if value.display == "false" || value.display == "nil" {
                                continue;
                            }
                        }
                        if let Some(log) = log {
                            let mut rest = log.as_str();
                            while let Some((literal, after)) = rest.split_once('{') {
                                self.output.push_str(literal);
                                let Some((expr, next)) = after.split_once('}') else {
                                    rest = after;
                                    break;
                                };
                                let value = self.evaluate(0, expr, 0);
                                self.output.push_str(&value.display);
                                rest = next;
                            }
                            self.output.push_str(rest);
                            self.output.push('\n');
                        } else {
                            self.skip_once = true;
                            return self.stop("breakpoint", None);
                        }
                    }
                }
            }
            self.core.tick(&mut self.lua.inner);
            self.output.push_str(&self.lua.take_output());
            if let Some(stop) = self.terminal_stop() {
                return stop;
            }
        }
        self.stop("running", None)
    }
    fn analyze(&self, max_events: usize) -> Result<(Vec<WasmFunctionStats>, LuaTimeline), String> {
        let mut session = Self::build(self.entry.clone(), self.files.clone())?;
        let mut stats: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
        let mut previous = std::collections::HashSet::new();
        let mut events = Vec::new();
        let mut truncated = false;
        while session.core.terminal.is_none() {
            let lua_ids = session.lua.inner.frame_identities();
            let lua_frames = session.lua.inner.frames();
            let mut stack = Vec::new();
            for route in session.routes().into_iter().rev() {
                match route {
                    FrameLocation::Typed(layer, frame) => {
                        let Layer::Typed(exec) = &session.core.layers[layer] else {
                            unreachable!()
                        };
                        let f = &exec.frames()[exec.frames().len() - 1 - frame as usize];
                        stack.push((
                            format!("t:{layer}:{}", f.identity),
                            f.bytecode.metadata.name.clone(),
                        ));
                    }
                    FrameLocation::Generic(frame) => {
                        let f = &lua_frames[frame as usize];
                        stack.push((format!("g:{}", lua_ids[frame as usize]), f.0.clone()));
                    }
                }
            }
            for (id, name) in &stack {
                if !previous.contains(id) {
                    stats.entry(name.clone()).or_default().0 += 1;
                }
            }
            let before = session.core.remaining;
            let position = (max_events > 0)
                .then(|| session.core.position(&session.lua.inner))
                .flatten();
            if max_events > 0 && session.candidate() {
                if let Some((_, source, line, _)) = &position {
                    if events.len() < max_events {
                        events.push(LuaTimelineEvent {
                            event_type: "line".into(),
                            source: if source.is_empty() {
                                session.entry.clone()
                            } else {
                                source.clone()
                            },
                            line: Some(*line),
                            local0: session.local_zero_snapshot(),
                            duration: 0,
                        });
                    } else {
                        truncated = true;
                    }
                }
            }
            session.core.tick(&mut session.lua.inner);
            let delta = before.saturating_sub(session.core.remaining);
            if delta > 0 {
                for (_, name) in &stack {
                    stats.entry(name.clone()).or_default().2 += delta;
                }
                if let Some((_, name)) = stack.last() {
                    stats.entry(name.clone()).or_default().1 += delta;
                }
                if let Some(event) = events.last_mut() {
                    event.duration = event
                        .duration
                        .saturating_add(delta.min(u32::MAX as u64) as u32);
                }
            }
            previous = stack.into_iter().map(|(id, _)| id).collect();
        }
        let error = session
            .core
            .terminal
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned();
        let functions = stats
            .into_iter()
            .map(|(name, (calls, own, total))| WasmFunctionStats {
                function_name: name,
                calls: calls as f64,
                self_instructions: own as f64,
                total_instructions: total as f64,
            })
            .collect();
        Ok((
            functions,
            LuaTimeline {
                events,
                truncated,
                error,
            },
        ))
    }
    fn local_zero_snapshot(&self) -> Option<String> {
        match self.core.layers.last()? {
            Layer::Typed(exec) => {
                let frame = exec.frames().last()?;
                let local = frame.visible_locals().find(|local| local.local_id == 0)?;
                let function = self
                    .core
                    .program
                    .functions
                    .iter()
                    .find(|f| f.name == frame.bytecode.metadata.name)?;
                let types = crate::types::collect_local_types(function);
                let value = debugger::ValueRenderer {
                    structs: &self.core.program.structs,
                }
                .render(&types[local.local_id], frame.registers[local.local_id]);
                Some(match value {
                    DisplayValue::Scalar(display) => display,
                    DisplayValue::Reference { summary, .. } => summary,
                })
            }
            Layer::Generic { .. } => self
                .lua
                .inner
                .locals(0)
                .first()
                .map(|(_, value)| String::from_utf8_lossy(&value.display_bytes()).into_owned()),
        }
    }
}

#[wasm_bindgen]
impl WasmMixedDebugSession {
    pub fn launch_project(
        entry: String,
        names: Vec<String>,
        contents: Vec<String>,
    ) -> Result<Self, JsValue> {
        if names.len() != contents.len() {
            return Err(JsValue::from_str(
                "project names and contents have different lengths",
            ));
        }
        Self::build(
            entry,
            names
                .into_iter()
                .zip(contents)
                .map(|(name, source)| (name, source.into_bytes()))
                .collect(),
        )
        .map_err(|e| JsValue::from_str(&e))
    }
    pub fn continue_burst(&mut self, instructions: u32) -> LuaDebugStop {
        self.advance(instructions, None)
    }
    pub fn continue_(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, None)
    }
    pub fn step_into(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, Some(0))
    }
    pub fn step_over(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, Some(1))
    }
    pub fn step_out(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, Some(2))
    }
    pub fn set_breakpoint(&mut self, source: String, line: u32) -> WasmBreakpoint {
        let verified = self.lua.inner.has_executable_line(&source, line)
            || self.core.program.functions.iter().any(|f| {
                (f.source_file.as_deref().unwrap_or(&self.entry) == source || f.name == source)
                    && self
                        .core
                        .engine
                        .function_bytecode(&f.name)
                        .is_some_and(|bc| {
                            (0..bc.code.len()).any(|pc| {
                                bc.source_map
                                    .location(pc as u32)
                                    .is_some_and(|p| p.line == line)
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
                condition: None,
                hit: None,
                log: None,
                hits: 0,
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
            bp.condition = value;
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
            status: if self.core.terminal.is_some() {
                "dead"
            } else {
                "suspended"
            }
            .into(),
        }]
    }
    pub fn get_stack_trace(&mut self, thread: u32) -> Vec<LuaDebugFrame> {
        if thread != 0 {
            return Vec::new();
        }
        let lua_frames = self.lua.get_stack_trace(self.active_lua_thread());
        self.routes()
            .into_iter()
            .enumerate()
            .map(|(index, route)| {
                let mut frame = match route {
                    FrameLocation::Typed(layer, frame) => self
                        .with_typed(layer, |view| view.get_stack_trace(0).remove(frame as usize)),
                    FrameLocation::Generic(frame) => {
                        let f = &lua_frames[frame as usize];
                        LuaDebugFrame {
                            index: 0,
                            name: f.name.clone(),
                            source: f.source.clone(),
                            line: f.line,
                            function_type: f.function_type.clone(),
                        }
                    }
                };
                frame.index = index as u32;
                frame
            })
            .collect()
    }
    pub fn get_locals(&mut self, thread: u32, frame: u32) -> Vec<LuaDebugVariable> {
        if thread != 0 {
            return Vec::new();
        }
        match self.routes().get(frame as usize).copied() {
            Some(FrameLocation::Typed(layer, frame)) => {
                Self::tag(self.with_typed(layer, |view| view.get_locals(0, frame)))
            }
            Some(FrameLocation::Generic(frame)) => {
                self.lua.get_locals(self.active_lua_thread(), frame)
            }
            None => Vec::new(),
        }
    }
    pub fn get_upvalues(&mut self, thread: u32, frame: u32) -> Vec<LuaDebugVariable> {
        if thread != 0 {
            return Vec::new();
        }
        match self.routes().get(frame as usize).copied() {
            Some(FrameLocation::Generic(frame)) => {
                self.lua.get_upvalues(self.active_lua_thread(), frame)
            }
            _ => Vec::new(),
        }
    }
    pub fn get_globals(&mut self) -> Vec<LuaDebugVariable> {
        self.lua.get_globals()
    }
    pub fn get_table_entries(
        &mut self,
        reference: u32,
        start: u32,
        count: u32,
    ) -> Vec<LuaDebugVariable> {
        if reference & TYPED_REFERENCE != 0 {
            self.typed
                .as_mut()
                .map(|view| {
                    Self::tag(view.get_table_entries(reference & !TYPED_REFERENCE, start, count))
                })
                .unwrap_or_default()
        } else {
            self.lua.get_table_entries(reference, start, count)
        }
    }
    pub fn get_metatable(&mut self, reference: u32) -> Option<u32> {
        if reference & TYPED_REFERENCE != 0 {
            None
        } else {
            self.lua.get_metatable(reference)
        }
    }
    pub fn evaluate(&mut self, thread: u32, expression: &str, frame: u32) -> WasmEvalResult {
        if thread != 0 {
            return WasmEvalResult {
                ok: false,
                display: "unknown thread".into(),
            };
        }
        match self.routes().get(frame as usize).copied() {
            Some(FrameLocation::Typed(layer, frame)) => {
                self.with_typed(layer, |view| view.evaluate(0, expression, frame))
            }
            Some(FrameLocation::Generic(frame)) => {
                self.lua
                    .evaluate(self.active_lua_thread(), expression, frame)
            }
            None => WasmEvalResult {
                ok: false,
                display: "unknown frame".into(),
            },
        }
    }
    pub fn set_variable(
        &mut self,
        thread: u32,
        frame: u32,
        name: &str,
        expression: &str,
    ) -> WasmEvalResult {
        if thread != 0 {
            return WasmEvalResult {
                ok: false,
                display: "unknown thread".into(),
            };
        }
        match self.routes().get(frame as usize).copied() {
            Some(FrameLocation::Typed(layer, frame)) => {
                self.with_typed(layer, |view| view.set_variable(0, frame, name, expression))
            }
            Some(FrameLocation::Generic(frame)) => {
                self.lua
                    .set_variable(self.active_lua_thread(), frame, name, expression)
            }
            None => WasmEvalResult {
                ok: false,
                display: "unknown frame".into(),
            },
        }
    }
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }
    pub fn force_gc(&mut self) {
        self.lua.force_gc();
        if let Some(view) = &self.typed {
            view.force_gc();
        }
    }
    pub fn memory_stats(&self) -> WasmMemoryStats {
        let lua = self.lua.memory_stats();
        let typed = self.typed.as_ref().map(|view| view.memory_stats());
        WasmMemoryStats {
            live_bytes: lua.live_bytes + typed.map(|s| s.live_bytes).unwrap_or(0.0),
            live_blocks: lua.live_blocks + typed.map(|s| s.live_blocks).unwrap_or(0.0),
        }
    }
    pub fn profile(&self) -> Result<Vec<WasmFunctionStats>, JsValue> {
        let (stats, timeline) = self.analyze(0).map_err(|e| JsValue::from_str(&e))?;
        if let Some(error) = timeline.error {
            return Err(JsValue::from_str(&error));
        }
        Ok(stats)
    }
    pub fn record_timeline(&self, max_events: u32) -> LuaTimeline {
        match self.analyze(max_events.min(100_000) as usize) {
            Ok((_, timeline)) => timeline,
            Err(error) => LuaTimeline {
                events: Vec::new(),
                truncated: false,
                error: Some(error),
            },
        }
    }
}

#[wasm_bindgen]
pub fn execute_mixed_project(
    entry: String,
    names: Vec<String>,
    contents: Vec<String>,
) -> ExecuteResult {
    if names.len() != contents.len() {
        return ExecuteResult {
            result: None,
            error: Some("project names and contents have different lengths".into()),
        };
    }
    let mut session = match WasmMixedDebugSession::build(
        entry,
        names
            .into_iter()
            .zip(contents)
            .map(|(n, s)| (n, s.into_bytes()))
            .collect(),
    ) {
        Ok(session) => session,
        Err(error) => {
            return ExecuteResult {
                result: None,
                error: Some(error),
            }
        }
    };
    let mut stop = session.continue_();
    while stop.reason == "running" {
        stop = session.continue_();
    }
    ExecuteResult {
        result: Some(session.take_output()),
        error: if stop.reason == "exception" {
            stop.message
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> WasmMixedDebugSession {
        WasmMixedDebugSession::build("main.sol".into(),vec![
            ("main.sol".into(),b"import helper\nfunction main():i64\n local base:i64=40\n local result=helper.add(base)\n return base+result\nend".to_vec()),
            ("helper.lua".into(),b"function add(n:i64):i64\n local y=n+2\n print(y)\n return y\nend".to_vec()),
        ]).unwrap()
    }
    #[test]
    fn mixed_live_frames_edit_both_representations_and_keep_analysis_isolated() {
        let mut session = session();
        assert!(session.set_breakpoint("helper.lua".into(), 3).verified);
        assert_eq!(session.continue_burst(1000).reason, "breakpoint");
        let frames = session.get_stack_trace(0);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].source, "helper.lua");
        assert_eq!(frames[1].source, "main.sol");
        assert_eq!(session.evaluate(0, "y", 0).display, "42");
        assert_eq!(session.evaluate(0, "base", 1).display, "40");
        assert!(session.set_variable(0, 0, "y", "43").ok);
        assert!(session.set_variable(0, 1, "base", "1").ok);
        session.force_gc();
        let profile = session.profile().unwrap();
        assert!(profile
            .iter()
            .any(|stat| stat.function_name == "helper.add" && stat.calls == 1.0));
        let timeline = session.record_timeline(2);
        assert_eq!(timeline.events.len(), 2);
        assert!(timeline.truncated);
        assert!(timeline.error.is_none());
        assert_eq!(session.evaluate(0, "y", 0).display, "43");
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "43\n44");
    }
    #[test]
    fn mixed_runaway_bursts_and_timeline_are_bounded() {
        let mut session = WasmMixedDebugSession::build(
            "main.sol".into(),
            vec![(
                "main.sol".into(),
                b"function typed(n:i64):i64 return n+1 end\nwhile true do typed(40) end".to_vec(),
            )],
        )
        .unwrap();
        for _ in 0..10 {
            assert_eq!(session.continue_burst(100).reason, "running");
        }
        assert!(session.core.layers.len() < 3);
        assert!(session.core.remaining < 10_000_000);
    }
}
