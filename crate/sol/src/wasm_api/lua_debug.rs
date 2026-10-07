//! Browser adapter for the canonical dynamic runtime's live debugger.
use super::*;
use crate::lua_runtime::{
    debugger::{DebugStatus, LuaDebugSession},
    LuaValue,
};
use std::collections::HashMap;

#[wasm_bindgen(getter_with_clone)]
pub struct LuaDebugStop {
    pub reason: String,
    pub line: Option<u32>,
    pub message: Option<String>,
    pub source: Option<String>,
}

#[wasm_bindgen(getter_with_clone)]
pub struct LuaDebugVariable {
    pub name: String,
    pub value_type: String,
    pub display: String,
    pub expandable: bool,
    pub reference: Option<u32>,
}

#[wasm_bindgen(getter_with_clone)]
pub struct LuaDebugFrame {
    pub index: u32,
    pub name: String,
    pub source: String,
    pub line: Option<u32>,
    pub function_type: String,
}

#[wasm_bindgen(getter_with_clone)]
#[derive(Clone)]
pub struct LuaTimelineEvent {
    pub event_type: String,
    pub source: String,
    pub line: Option<u32>,
    pub local0: Option<String>,
    pub duration: u32,
}

#[wasm_bindgen]
pub struct LuaTimeline {
    pub(super) events: Vec<LuaTimelineEvent>,
    pub(super) truncated: bool,
    pub(super) error: Option<String>,
}
#[wasm_bindgen]
impl LuaTimeline {
    #[wasm_bindgen(getter)]
    pub fn events(&self) -> Vec<LuaTimelineEvent> {
        self.events.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    #[wasm_bindgen(getter)]
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

struct Breakpoint {
    source: String,
    line: u32,
    condition: Option<String>,
    hit_condition: Option<u32>,
    log: Option<String>,
    hits: u32,
}

#[wasm_bindgen]
pub struct WasmLuaDebugSession {
    inner: LuaDebugSession,
    breakpoints: HashMap<u32, Breakpoint>,
    next_breakpoint: u32,
    references: HashMap<u32, LuaValue>,
    next_reference: u32,
    skip_breakpoint_once: bool,
}

#[wasm_bindgen]
impl WasmLuaDebugSession {
    pub fn launch_generic_project(entry: String, names: Vec<String>, contents: Vec<String>) -> Result<Self, JsValue> {
        let inner = generic_project_session(&entry, &names, &contents)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(Self { inner, breakpoints: HashMap::new(), next_breakpoint: 1, references: HashMap::new(),
            next_reference: 1, skip_breakpoint_once: false })
    }
    /// Generic, extension-enabled Sol uses the same semantic runtime and live
    /// debugger; mandatory typed surfaces are not silently boxed here.
    pub fn launch_generic_sol(source: &str, name: &str) -> Result<Self, JsValue> {
        crate::modules::virtual_path(name).map_err(|error| JsValue::from_str(&error))?;
        let inner = LuaDebugSession::launch_with_config(source.as_bytes(), name, crate::parser::LanguageConfig::SOL)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(Self { inner, breakpoints: HashMap::new(), next_breakpoint: 1, references: HashMap::new(),
            next_reference: 1, skip_breakpoint_once: false })
    }
    pub fn launch_project(
        entry: String,
        names: Vec<String>,
        contents: Vec<String>,
    ) -> Result<Self, JsValue> {
        validate_lua_project(&entry, &names, &contents)
            .map_err(|error| JsValue::from_str(&error))?;
        if names.len() != contents.len() {
            return Err(JsValue::from_str(
                "project names and contents have different lengths",
            ));
        }
        let index = names
            .iter()
            .position(|name| name == &entry)
            .ok_or_else(|| JsValue::from_str("entry is missing from project"))?;
        let mut inner = LuaDebugSession::launch(contents[index].as_bytes(), &entry)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        for (i, (name, source)) in names.iter().zip(&contents).enumerate() {
            if i == index {
                continue;
            }
            let module = name
                .strip_suffix(".lua")
                .ok_or_else(|| JsValue::from_str("Lua debugger requires Lua project modules"))?
                .replace('/', ".");
            inner.add_named_module(module.as_bytes(), name, source.as_bytes());
        }
        Ok(Self {
            inner,
            breakpoints: HashMap::new(),
            next_breakpoint: 1,
            references: HashMap::new(),
            next_reference: 1,
            skip_breakpoint_once: false,
        })
    }

    pub fn continue_burst(&mut self, instructions: u32) -> LuaDebugStop {
        self.advance(instructions, None)
    }
    pub fn continue_(&mut self) -> LuaDebugStop {
        self.advance(10_000_001, None)
    }
    pub fn step_into(&mut self) -> LuaDebugStop {
        self.step("into")
    }
    pub fn step_over(&mut self) -> LuaDebugStop {
        self.step("over")
    }
    pub fn step_out(&mut self) -> LuaDebugStop {
        self.step("out")
    }

    pub fn set_breakpoint(&mut self, source: String, line: u32) -> WasmBreakpoint {
        let verified = self.inner.has_executable_line(&source, line);
        let id = self.next_breakpoint;
        self.next_breakpoint += 1;
        self.breakpoints.insert(
            id,
            Breakpoint {
                source: source.clone(),
                line,
                condition: None,
                hit_condition: None,
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
            bp.hit_condition = value;
        }
    }
    pub fn set_breakpoint_log_message(&mut self, id: u32, value: Option<String>) {
        if let Some(bp) = self.breakpoints.get_mut(&id) {
            bp.log = value;
        }
    }

    pub fn get_threads(&self) -> Vec<WasmThreadInfo> {
        self.inner
            .threads()
            .into_iter()
            .map(|(id, status)| WasmThreadInfo { id, status })
            .collect()
    }
    pub fn get_stack_trace(&mut self, thread: u32) -> Vec<LuaDebugFrame> {
        self.inner
            .with_thread(thread, |inner| Ok(inner.frames()))
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, (name, source, line))| LuaDebugFrame {
                index: index as u32,
                name,
                source,
                line,
                function_type: "lua".into(),
            })
            .collect()
    }
    pub fn get_locals(&mut self, thread: u32, frame: u32) -> Vec<LuaDebugVariable> {
        let values = self
            .inner
            .with_thread(thread, |inner| Ok(inner.locals(frame as usize)))
            .unwrap_or_default();
        self.variables(values)
    }
    pub fn get_upvalues(&mut self, thread: u32, frame: u32) -> Vec<LuaDebugVariable> {
        let values = self
            .inner
            .with_thread(thread, |inner| Ok(inner.upvalues(frame as usize)))
            .unwrap_or_default();
        self.variables(values)
    }
    pub fn get_globals(&mut self) -> Vec<LuaDebugVariable> {
        let values = self.inner.globals();
        self.variables(values)
    }
    pub fn get_table_entries(
        &mut self,
        reference: u32,
        start: u32,
        count: u32,
    ) -> Vec<LuaDebugVariable> {
        let values = self
            .references
            .get(&reference)
            .map(|value| self.inner.table_entries(value))
            .unwrap_or_default();
        self.variables(
            values
                .into_iter()
                .skip(start as usize)
                .take(count as usize)
                .collect(),
        )
    }
    pub fn get_metatable(&mut self, reference: u32) -> Option<u32> {
        let value = self.inner.metatable(self.references.get(&reference)?)?;
        Some(self.reference(value))
    }
    pub fn evaluate(&mut self, thread: u32, expression: &str, frame: u32) -> WasmEvalResult {
        eval_result(
            self.inner
                .with_thread(thread, |inner| inner.evaluate(frame as usize, expression)),
        )
    }
    pub fn set_variable(
        &mut self,
        thread: u32,
        frame: u32,
        name: &str,
        expression: &str,
    ) -> WasmEvalResult {
        eval_result(self.inner.with_thread(thread, |inner| {
            inner.set_variable(frame as usize, name, expression)
        }))
    }
    pub fn take_output(&mut self) -> String {
        String::from_utf8_lossy(&self.inner.take_output()).into_owned()
    }
    pub fn force_gc(&mut self) {
        self.inner.force_gc();
    }
    pub fn memory_stats(&self) -> WasmMemoryStats {
        WasmMemoryStats {
            live_bytes: self.inner.live_bytes() as f64,
            live_blocks: self.inner.live_objects() as f64,
        }
    }

    pub fn profile(&mut self) -> Result<Vec<WasmFunctionStats>, JsValue> {
        let analysis = self.inner.analyze(0);
        if let Some(error) = analysis.error {
            return Err(JsValue::from_str(&error));
        }
        Ok(analysis
            .functions
            .into_iter()
            .map(|stat| WasmFunctionStats {
                function_name: stat.name,
                calls: stat.calls as f64,
                self_instructions: stat.self_instructions as f64,
                total_instructions: stat.total_instructions as f64,
            })
            .collect())
    }

    pub fn record_timeline(&mut self, max_events: u32) -> LuaTimeline {
        let analysis = self.inner.analyze(max_events.min(100_000) as usize);
        LuaTimeline {
            events: analysis
                .events
                .into_iter()
                .map(|event| LuaTimelineEvent {
                    event_type: event.kind,
                    source: event.source,
                    line: event.line,
                    local0: event.local0,
                    duration: event.duration,
                })
                .collect(),
            truncated: analysis.truncated,
            error: analysis.error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch(source: &str) -> WasmLuaDebugSession {
        WasmLuaDebugSession::launch_project(
            "main.lua".into(),
            vec!["main.lua".into()],
            vec![source.into()],
        )
        .unwrap()
    }

    #[test]
    fn coroutine_stop_exposes_isolated_parent_and_child_frames() {
        let mut session = launch("local outer = 40\nlocal co = coroutine.create(function()\n local value = outer\n value = value + 2\n coroutine.yield(value)\n print(value)\nend)\nlocal ok, result = coroutine.resume(co)\nprint(ok, result)\nok = coroutine.resume(co)\nprint(ok)");
        session.set_breakpoint("main.lua".into(), 5);
        assert_eq!(session.continue_burst(1000).reason, "breakpoint");
        let threads = session.get_threads();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].status(), "normal");
        assert_eq!(threads[1].status(), "running");
        assert_eq!(session.evaluate(0, "outer", 0).display(), "40");
        assert_eq!(session.evaluate(1, "value", 0).display(), "42");
        assert!(session.set_variable(1, 0, "value", "44").ok());
        session.force_gc();
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "true\t44\n44\ntrue\n");
    }

    #[test]
    fn coroutine_runaway_loop_is_live_and_droppable() {
        let mut session = launch(
            "local run = coroutine.wrap(function() local n=0; while true do n=n+1 end end); run()",
        );
        for _ in 0..10 {
            assert_eq!(session.continue_burst(100).reason, "running");
        }
        assert_eq!(session.get_threads().len(), 2);
    }

    #[test]
    fn nested_coroutine_chain_keeps_all_parent_frames_rooted() {
        let mut session = launch("local mainvalue = {answer=40}\nlocal inner=coroutine.create(function(n)\n local child={answer=n+2}\n print(child.answer)\n return child\nend)\nlocal outer=coroutine.create(function()\n local parent={answer=mainvalue.answer}\n local ok,child=coroutine.resume(inner, parent.answer)\n print(ok,child.answer,parent.answer)\nend)\nprint(coroutine.resume(outer))");
        session.set_breakpoint("main.lua".into(), 4);
        assert_eq!(session.continue_burst(1000).reason, "breakpoint");
        assert_eq!(session.get_threads().len(), 3);
        assert_eq!(session.evaluate(0, "mainvalue.answer", 0).display(), "40");
        assert_eq!(session.evaluate(1, "parent.answer", 0).display(), "40");
        assert_eq!(session.evaluate(2, "child.answer", 0).display(), "42");
        assert!(session.evaluate(0, "collectgarbage()", 0).ok());
        assert!(session.set_variable(2, 0, "child", "{answer=44}").ok());
        session.force_gc();
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "44\ntrue\t44\t40\ntrue\n");
    }

    #[test]
    fn coroutine_errors_and_wrapped_protected_calls_preserve_values() {
        let mut session = launch("local co=coroutine.create(function() error({answer=42}) end); local ok,e=coroutine.resume(co); print(ok,e.answer); local f=coroutine.wrap(function() error('expected') end); print(pcall(f)); local a,b=coroutine.resume(co); print(a, b)");
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        let output = session.take_output();
        assert!(output.starts_with("false\t42\nfalse\t"), "{output}");
        assert!(output.contains("expected"), "{output}");
        assert!(
            output.ends_with("false\tcannot resume dead coroutine\n"),
            "{output}"
        );
    }

    #[test]
    fn live_coroutine_edge_cases_agree_with_normal_execution() {
        for source in [
            "local co=coroutine.create(coroutine.yield); print(coroutine.resume(co, 40)); print(coroutine.resume(co, 42)); print(coroutine.status(co))",
            "local inner=coroutine.create(function() return 42 end); local outer=coroutine.create(coroutine.resume); print(coroutine.resume(outer, inner))",
            "local co=coroutine.create(function() local x <close> = setmetatable({}, {__close=function() print('closed') end}); coroutine.close(coroutine.running()); print('unreachable') end); print(coroutine.resume(co)); print(coroutine.status(co))",
            "local co=coroutine.create(function() return coroutine.resume(coroutine.running()) end); local ok,again=coroutine.resume(co); print(ok,again)",
            "local weak=setmetatable({}, {__mode='v'}); local co=coroutine.create(function() local t={answer=42}; weak[1]=t; return t end); local ok,t=coroutine.resume(co); t=nil; tostring(0); tostring(0); collectgarbage(); print(weak[1]==nil)",
        ] {
            let expected = execute_lua(source);
            assert_eq!(expected.error(), None, "{source}");
            let mut session = launch(source);
            let stop = session.continue_burst(1000);
            assert_eq!(stop.reason, "terminated", "{source}: {:?}", stop.message);
            assert_eq!(session.take_output(), expected.result().unwrap(), "{source}");
        }
    }

    #[test]
    fn registered_require_preserves_the_non_yieldable_loader_boundary() {
        let names = vec!["main.lua".to_string(), "loader.lua".to_string()];
        let contents = vec!["local co=coroutine.create(function() require('loader') end); local ok,e=coroutine.resume(co); print(ok,type(e))".to_string(), "print('module'); coroutine.yield(42)".to_string()];
        let expected = execute_lua_project("main.lua".into(), names.clone(), contents.clone());
        assert_eq!(expected.error(), None);
        let mut session =
            WasmLuaDebugSession::launch_project("main.lua".into(), names, contents).unwrap();
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), expected.result().unwrap());
    }

    #[test]
    fn same_line_loop_hits_and_conditions_use_live_values() {
        let mut session = launch("local n = 0\nwhile n < 4 do n = n + 1 end\nprint(n)");
        let bp = session.set_breakpoint("main.lua".into(), 2);
        session.set_breakpoint_hit_condition(bp.id, Some(3));
        session.set_breakpoint_condition(bp.id, Some("n == 2".into()));
        let stop = session.continue_burst(1000);
        assert_eq!(stop.reason, "breakpoint");
        assert_eq!(session.evaluate(0, "n", 0).display(), "2");
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "4\n");
    }

    #[test]
    fn step_into_out_and_upvalue_edit_change_future_execution() {
        let mut session = launch("local count = 1\nlocal function f()\n count = count + 1\n return count\nend\nlocal total = f()\nprint(total)");
        let bp = session.set_breakpoint("main.lua".into(), 6);
        assert_eq!(session.continue_burst(1000).reason, "breakpoint");
        session.remove_breakpoint(bp.id);
        assert_eq!(session.step_into().reason, "step");
        assert_eq!(session.get_stack_trace(0).len(), 2);
        assert!(session.set_variable(0, 0, "count", "776").ok());
        assert_eq!(session.step_out().reason, "step");
        assert_eq!(session.get_stack_trace(0).len(), 1);
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "777\n");
    }

    #[test]
    fn registered_modules_suspend_at_their_own_source_breakpoints() {
        let mut session = WasmLuaDebugSession::launch_project(
            "main.lua".into(),
            vec!["main.lua".into(), "math/base.lua".into()],
            vec![
                "local base = require('math.base')\nprint(base.answer)".into(),
                "local answer = 40\nanswer = answer + 2\nreturn {answer = answer}".into(),
            ],
        )
        .unwrap();
        session.set_breakpoint("math/base.lua".into(), 3);
        let stop = session.continue_burst(1000);
        assert_eq!(stop.reason, "breakpoint");
        assert_eq!(stop.source.as_deref(), Some("math/base.lua"));
        assert_eq!(session.get_stack_trace(0).len(), 2);
        assert_eq!(session.evaluate(0, "answer", 0).display(), "42");
        session.force_gc();
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "42\n");
    }

    #[test]
    fn failed_module_cache_is_cleared_before_protected_retry() {
        let mut session = WasmLuaDebugSession::launch_project("main.lua".into(),
            vec!["main.lua".into(), "failure.lua".into()],
            vec!["local a = pcall(require, 'failure')\nlocal b = pcall(require, 'failure')\nprint(a, b)".into(),
                "error('expected module failure')".into()]).unwrap();
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(session.take_output(), "false\tfalse\n");
    }

    #[test]
    fn interpolated_logpoints_preserve_print_order_without_stopping() {
        let mut session = launch("local x = 40; print('before')\nprint(x)\nprint('after')");
        let bp = session.set_breakpoint("main.lua".into(), 2);
        session.set_breakpoint_log_message(bp.id, Some("value={x}, next={x+2}".into()));
        assert_eq!(session.continue_burst(1000).reason, "terminated");
        assert_eq!(
            session.take_output(),
            "before\nvalue=40, next=42\n40\nafter\n"
        );
    }
}

fn eval_result(result: crate::lua_runtime::LuaResult<LuaValue>) -> WasmEvalResult {
    match result {
        Ok(value) => WasmEvalResult {
            ok: true,
            display: String::from_utf8_lossy(&value.display_bytes()).into_owned(),
        },
        Err(error) => WasmEvalResult {
            ok: false,
            display: error.to_string(),
        },
    }
}

impl WasmLuaDebugSession {
    fn render_log(&mut self, template: &str) -> String {
        let mut rendered = String::new();
        let mut remaining = template;
        while let Some(open) = remaining.find('{') {
            rendered.push_str(&remaining[..open]);
            let expression = &remaining[open + 1..];
            let Some(close) = expression.find('}') else {
                rendered.push_str(&remaining[open..]);
                return rendered;
            };
            match self.inner.evaluate(0, &expression[..close]) {
                Ok(value) => rendered.push_str(&String::from_utf8_lossy(&value.display_bytes())),
                Err(error) => rendered.push_str(&format!("<error: {error}>")),
            }
            remaining = &expression[close + 1..];
        }
        rendered.push_str(remaining);
        rendered
    }
    fn stop(&self, reason: &str, message: Option<String>) -> LuaDebugStop {
        let position = self.inner.position();
        LuaDebugStop {
            reason: reason.into(),
            line: position.as_ref().map(|p| p.1),
            source: position.map(|p| p.0),
            message,
        }
    }
    fn reference(&mut self, value: LuaValue) -> u32 {
        if let Some((id, _)) = self
            .references
            .iter()
            .find(|(_, existing)| **existing == value)
        {
            return *id;
        }
        let id = self.next_reference;
        self.next_reference += 1;
        self.inner.retain(&value);
        self.references.insert(id, value);
        id
    }
    fn variables(&mut self, values: Vec<(String, LuaValue)>) -> Vec<LuaDebugVariable> {
        values
            .into_iter()
            .map(|(name, value)| {
                let expandable = matches!(value, LuaValue::Table(_));
                let value_type = value.type_name().into();
                let display = String::from_utf8_lossy(&value.display_bytes()).into_owned();
                let reference = expandable.then(|| self.reference(value));
                LuaDebugVariable {
                    name,
                    value_type,
                    display,
                    expandable,
                    reference,
                }
            })
            .collect()
    }
    fn step(&mut self, mode: &str) -> LuaDebugStop {
        let initial = self.inner.position();
        self.advance(10_000_001, Some((mode, initial)))
    }
    fn advance(
        &mut self,
        budget: u32,
        stepping: Option<(&str, Option<(String, u32, usize)>)>,
    ) -> LuaDebugStop {
        for executed in 0..budget {
            let position = self.inner.position();
            if executed > 0 {
                if let Some((mode, Some(initial))) = &stepping {
                    if let Some(current) = &position {
                        let changed = current != initial;
                        if changed
                            && (*mode == "into"
                                || (*mode == "over" && current.2 <= initial.2)
                                || (*mode == "out" && current.2 < initial.2))
                        {
                            return self.stop("step", None);
                        }
                    }
                }
            }
            let candidate = self.inner.breakpoint_candidate();
            let skip = std::mem::take(&mut self.skip_breakpoint_once);
            if !skip && candidate {
                if let Some((source, line, _)) = &position {
                    let ids = self
                        .breakpoints
                        .iter()
                        .filter(|(_, bp)| bp.source == *source && bp.line == *line)
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>();
                    for id in ids {
                        let bp = self.breakpoints.get_mut(&id).unwrap();
                        bp.hits += 1;
                        if bp.hit_condition.is_some_and(|count| bp.hits != count) {
                            continue;
                        }
                        let condition = bp.condition.clone();
                        let log = bp.log.clone();
                        if let Some(condition) = condition {
                            match self.inner.evaluate(0, &condition) {
                                Ok(LuaValue::Nil | LuaValue::Bool(false)) => continue,
                                Ok(_) => {}
                                Err(error) => {
                                    return self.stop("exception", Some(error.to_string()))
                                }
                            }
                        }
                        if let Some(log) = log {
                            let rendered = self.render_log(&log) + "\n";
                            if let Err(error) = self.inner.write_debug_output(rendered.as_bytes()) {
                                return self.stop("exception", Some(error.to_string()));
                            }
                        } else {
                            self.skip_breakpoint_once = true;
                            return self.stop("breakpoint", None);
                        }
                    }
                }
            }
            match self.inner.continue_burst(1) {
                DebugStatus::Paused => {}
                DebugStatus::Returned => return self.stop("terminated", None),
                DebugStatus::Raised(error) => return self.stop("exception", Some(error)),
            }
        }
        self.stop("running", None)
    }
}
