//! Bounded specialized bytecode accounting. No register/instruction replay
//! is retained, and native machine instructions are not fabricated.
use super::{DisplayValue, ValueRenderer};
use crate::{
    bytecode::LocalDebug,
    interp::Hooks,
    types::{TProgram, Type},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
};

#[derive(Clone, Default)]
pub struct FunctionStats {
    pub function_name: String,
    pub calls: u64,
    pub self_instructions: u64,
    pub total_instructions: u64,
}
#[derive(Clone)]
pub struct Event {
    pub kind: String,
    pub source: String,
    pub line: Option<u32>,
    pub local0: Option<String>,
    pub duration: u32,
}
pub struct Analysis {
    pub functions: Vec<FunctionStats>,
    pub events: Vec<Event>,
    pub truncated: bool,
    pub error: Option<String>,
}
struct Info {
    name: String,
    source: String,
    line: u32,
    local0: Option<Type>,
    locals: Vec<LocalDebug>,
}
struct Activation {
    function: u8,
    started: u32,
    line: u32,
    pc: Option<u32>,
    local0: Option<String>,
}
struct State {
    infos: HashMap<u8, Info>,
    stats: BTreeMap<u8, FunctionStats>,
    stack: Vec<Activation>,
    events: Vec<Event>,
    limit: usize,
    truncated: bool,
    instructions: u32,
    last_event: u32,
    structs: HashMap<String, crate::types::StructLayout>,
}
impl State {
    fn finish(&mut self, frame: &Activation) {
        self.stats
            .get_mut(&frame.function)
            .unwrap()
            .total_instructions += (self.instructions - frame.started) as u64;
    }
    fn event(&mut self, kind: &str, function: u8, line: u32, local0: Option<String>) {
        if self.events.len() >= self.limit {
            self.truncated = true;
            return;
        }
        let source = self.infos[&function].source.clone();
        self.events.push(Event {
            kind: kind.into(),
            source,
            line: Some(line),
            local0,
            duration: self.instructions - self.last_event,
        });
        self.last_event = self.instructions;
    }
    fn display(&self, function: u8, raw: Option<u64>) -> Option<String> {
        if self.events.len() >= self.limit {
            return None;
        }
        let ty = self.infos[&function].local0.as_ref()?;
        let raw = raw?;
        Some(
            match (ValueRenderer {
                structs: &self.structs,
            })
            .render(ty, raw)
            {
                DisplayValue::Scalar(value) => value,
                DisplayValue::Reference { summary, .. } => summary,
            },
        )
    }
    fn call(&mut self, function: u8) {
        let name = &self.infos[&function].name;
        self.stats
            .entry(function)
            .or_insert_with(|| FunctionStats {
                function_name: name.clone(),
                ..Default::default()
            })
            .calls += 1;
    }
}
#[derive(Clone)]
struct Recorder(Rc<RefCell<State>>);
impl Hooks for Recorder {
    fn on_call_enter(&self, function: u8, args: &[u64]) {
        let mut state = self.0.borrow_mut();
        state.call(function);
        let line = state.infos[&function].line;
        let local0 = state.display(function, args.first().copied());
        state.event("call_enter", function, line, local0.clone());
        let started = state.instructions;
        state.stack.push(Activation {
            function,
            started,
            line,
            pc: None,
            local0,
        });
    }
    fn on_call_exit(&self, _function: u8, _result: u64) {
        let mut state = self.0.borrow_mut();
        if let Some(frame) = state.stack.pop() {
            state.finish(&frame);
            state.event("call_exit", frame.function, frame.line, frame.local0);
        }
    }
    fn on_tail_call(&self, from: u8, to: u8, args: &[u64]) {
        let mut state = self.0.borrow_mut();
        if let Some(frame) = state.stack.pop() {
            state.finish(&frame);
            state.event("tail_call", from, frame.line, frame.local0);
        }
        state.call(to);
        let line = state.infos[&to].line;
        let local0 = state.display(to, args.first().copied());
        let started = state.instructions;
        state.stack.push(Activation {
            function: to,
            started,
            line,
            pc: None,
            local0,
        });
    }
    fn on_instruction(&self, function: u8, pc: u32, line: u32, registers: &[u64]) {
        let mut state = self.0.borrow_mut();
        state.instructions += 1;
        state.stats.get_mut(&function).unwrap().self_instructions += 1;
        // Inclusive counts use invocation intervals finalized at return,
        // tail replacement, or failure. A deep hot loop does not walk all
        // ancestors at every opcode.
        let initialized = state.events.len() < state.limit
            && state.infos[&function]
                .locals
                .iter()
                .any(|local| local.local_id == 0 && local.start_pc <= pc && pc < local.end_pc);
        let local0 = if initialized {
            state.display(function, registers.first().copied())
        } else {
            None
        };
        let frame = state.stack.last_mut().unwrap();
        let changed = frame.pc.is_none_or(|old| pc <= old) || frame.line != line;
        frame.pc = Some(pc);
        frame.line = line;
        frame.local0 = local0.clone();
        if changed {
            state.event("line", function, line, local0);
        }
    }
}

pub fn analyze(program: &TProgram, entry: &str, event_limit: usize) -> Result<Analysis, String> {
    analyze_with_budget(
        program,
        entry,
        event_limit,
        crate::tier0::DEFAULT_INSTRUCTION_BUDGET,
    )
}

fn analyze_with_budget(
    program: &TProgram,
    entry: &str,
    event_limit: usize,
    budget: u64,
) -> Result<Analysis, String> {
    let ids = crate::bccompile::function_index(program)?;
    let mut infos = HashMap::new();
    for function in &program.functions {
        let types = crate::types::collect_local_types(function);
        infos.insert(
            ids[&function.name],
            Info {
                name: function.name.clone(),
                source: function.source_file.clone().unwrap_or_else(|| entry.into()),
                line: function.source_line,
                local0: types.first().cloned(),
                locals: Vec::new(),
            },
        );
    }
    for function in &program.externs {
        infos.insert(
            ids[&function.name],
            Info {
                name: function.name.clone(),
                source: entry.into(),
                line: 0,
                local0: function.params.first().cloned(),
                locals: Vec::new(),
            },
        );
    }
    let state = Rc::new(RefCell::new(State {
        infos,
        stats: BTreeMap::new(),
        stack: Vec::new(),
        events: Vec::new(),
        limit: event_limit.min(100_000),
        truncated: false,
        instructions: 0,
        last_event: 0,
        structs: program.structs.clone(),
    }));
    let engine =
        crate::tier0::Engine::new_with_budget(program.clone(), Recorder(state.clone()), budget)?;
    for function in &program.functions {
        if let Some(bytecode) = engine.function_bytecode(&function.name) {
            state
                .borrow_mut()
                .infos
                .get_mut(&ids[&function.name])
                .unwrap()
                .locals = bytecode.debug_locals.clone();
        }
    }
    let mut execution = engine.start_live("main", &[])?;
    let error = loop {
        match execution.resume(10_000) {
            crate::interp::live::Stop::Paused => {}
            crate::interp::live::Stop::Returned(_) => break None,
            crate::interp::live::Stop::Raised(error) => break Some(error),
        }
    };
    let mut state = state.borrow_mut();
    while let Some(frame) = state.stack.pop() {
        state.finish(&frame);
    }
    Ok(Analysis {
        functions: std::mem::take(&mut state.stats).into_values().collect(),
        events: std::mem::take(&mut state.events),
        truncated: state.truncated,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn program(source: &str) -> TProgram {
        let ast =
            crate::parser::parse(crate::lexer::lex_bytes(source.as_bytes()).unwrap()).unwrap();
        crate::compile_debug_program(ast, crate::parser::LanguageConfig::SOL)
            .unwrap()
            .0
    }
    #[test]
    fn counts_live_calls_and_self_inclusive_bytecodes_without_a_trace() {
        let program = program("function add(x:i64):i64 return x+1 end\nfunction main():i64 local a=add(40) local b=add(41) return a+b end");
        let analysis = analyze(&program, "main.sol", 2).unwrap();
        assert!(analysis.error.is_none());
        assert_eq!(analysis.events.len(), 2);
        assert!(analysis.truncated);
        let helper = analysis
            .functions
            .iter()
            .find(|f| f.function_name == "add")
            .unwrap();
        assert_eq!(helper.calls, 2);
        assert_eq!(helper.self_instructions, helper.total_instructions);
        let main = analysis
            .functions
            .iter()
            .find(|f| f.function_name == "main")
            .unwrap();
        assert_eq!(main.calls, 1);
        assert!(main.total_instructions > main.self_instructions);
        assert_eq!(
            main.total_instructions,
            analysis.functions.iter().map(|f| f.self_instructions).sum()
        );
        let no_events = analyze(&program, "main.sol", 0).unwrap();
        assert!(no_events.events.is_empty());
        assert_eq!(
            no_events
                .functions
                .iter()
                .find(|f| f.function_name == "add")
                .unwrap()
                .calls,
            2
        );
    }
    #[test]
    fn bounded_timeline_has_live_source_lines_and_local_values() {
        let program = program("function main():i64\n local n:i64=40\n n=n+2\n return n\nend");
        let analysis = analyze(&program, "main.sol", 100).unwrap();
        assert!(!analysis.truncated);
        assert!(analysis
            .events
            .iter()
            .any(|e| e.kind == "line" && e.line == Some(3) && e.local0.as_deref() == Some("40")));
        assert_eq!(analysis.events.last().unwrap().kind, "call_exit");
        assert!(analysis.events.iter().all(|e| e.source == "main.sol"));
    }
    #[test]
    fn proper_tail_calls_count_each_activation_without_growing_an_event_history() {
        let program = program("function recur(n:i64):i64 if n==0 then return 42 end return recur(n-1) end function main():i64 return recur(10000) end");
        let analysis = analyze(&program, "main.sol", 1).unwrap();
        assert_eq!(analysis.events.len(), 1);
        assert!(analysis.truncated);
        assert_eq!(
            analysis
                .functions
                .iter()
                .find(|f| f.function_name == "recur")
                .unwrap()
                .calls,
            10001
        );
    }

    #[test]
    fn runaway_timeline_retains_only_the_requested_window_and_reports_budget_error() {
        let program =
            program("function main():i64 local n:i64=0 while true do n=n+1 end return n end");
        let analysis = analyze_with_budget(&program, "main.sol", 2, 1000).unwrap();
        assert_eq!(analysis.events.len(), 2);
        assert!(analysis.truncated);
        assert_eq!(
            analysis.error.as_deref(),
            Some("instruction budget exceeded")
        );
        assert_eq!(analysis.functions[0].self_instructions, 1000);
    }

    #[test]
    fn inclusive_instruction_counters_do_not_wrap_at_32_bits() {
        let mut stats = BTreeMap::new();
        stats.insert(
            0,
            FunctionStats {
                function_name: "main".into(),
                calls: 1,
                self_instructions: u32::MAX as u64,
                total_instructions: u32::MAX as u64,
            },
        );
        let state = Rc::new(RefCell::new(State {
            infos: HashMap::from([(
                0,
                Info {
                    name: "main".into(),
                    source: "main.sol".into(),
                    line: 1,
                    local0: None,
                    locals: Vec::new(),
                },
            )]),
            stats,
            stack: vec![Activation {
                function: 0,
                started: 0,
                line: 1,
                pc: None,
                local0: None,
            }],
            events: Vec::new(),
            limit: 0,
            truncated: false,
            instructions: 0,
            last_event: 0,
            structs: HashMap::new(),
        }));
        Recorder(state.clone()).on_instruction(0, 0, 1, &[]);
        Recorder(state.clone()).on_call_exit(0, 0);
        assert_eq!(state.borrow().stats[&0].self_instructions, 1u64 << 32);
        assert_eq!(state.borrow().stats[&0].total_instructions, 1u64 << 32);
    }
}
