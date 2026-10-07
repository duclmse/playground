//! Resumable specialized execution. Registers remain unboxed `u64` slots;
//! dispatch uses the ordinary interpreter's opcode implementation.
use super::{call_native, DispatchMode, Hooks, InstructionStep, Runtime, Slot};
use crate::{bytecode::BcFunction, gc};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Paused,
    Returned(u64),
    Raised(String),
}

/// An explicit cross-tier boundary. The scheduler owns conversion and the
/// callee's execution; no synchronous semantic callback is invoked here.
/// Arguments stay rooted even when a proper tail call has removed its frame.
pub struct SemanticCall {
    pub function: u8,
    arguments: Vec<u64>,
    exit_function: u8,
    _guard: gc::RootGuard,
}

impl SemanticCall {
    pub fn arguments(&self) -> &[u64] {
        &self.arguments
    }
}

enum Pending {
    Call(usize),
    Map {
        // Own the two roots independently of the parent's register contents.
        roots: Box<[u64; 2]>,
        _guard: gc::RootGuard,
        target: u8,
        destination: usize,
        index: usize,
    },
}

pub struct Frame {
    pub identity: u64,
    pub function: u8,
    pub bytecode: Rc<BcFunction>,
    pub registers: Vec<u64>,
    pub pc: usize,
    pub header: sol_core::FrameHeader,
    exit_function: u8,
    _guard: gc::RootGuard,
    pending: Option<Pending>,
}

impl Frame {
    pub fn is_waiting_for_result(&self) -> bool {
        self.pending.is_some()
    }
    /// A caller suspended on a call/map still points at that operation;
    /// `pc` itself is the next dispatch cursor used when its result arrives.
    pub fn instruction_pc(&self) -> u32 {
        if self.pending.is_some() {
            self.header.pc
        } else {
            self.pc as u32
        }
    }

    pub fn visible_locals(&self) -> impl Iterator<Item = &crate::bytecode::LocalDebug> {
        let pc = self.instruction_pc();
        self.bytecode.debug_locals.iter().filter(move |local| {
            local.start_pc <= pc && pc < local.end_pc && !local.name.starts_with('$')
        })
    }

    pub fn resolve_local(&self, name: &str) -> Option<usize> {
        self.visible_locals()
            .filter(|local| local.name == name)
            .max_by_key(|local| local.local_id)
            .map(|local| local.local_id)
    }
}

pub struct Execution<H: Hooks + 'static> {
    runtime: Rc<Runtime<'static, H>>,
    frames: Vec<Frame>,
    terminal: Option<Stop>,
    max_depth: usize,
    next_identity: u64,
    semantic_call: Option<SemanticCall>,
}

impl<H: Hooks + 'static> Execution<H> {
    pub(crate) fn new(
        runtime: Rc<Runtime<'static, H>>,
        function: u8,
        args: &[u64],
    ) -> Result<Self, String> {
        let mut execution = Self {
            runtime,
            frames: Vec::new(),
            terminal: None,
            max_depth: 1000,
            next_identity: 0,
            semantic_call: None,
        };
        execution.enter(function, args, None)?;
        Ok(execution)
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn semantic_call(&self) -> Option<&SemanticCall> {
        self.semantic_call.as_ref()
    }

    pub(crate) fn configure_bridge_limits(&mut self, budget: u64, max_depth: usize) {
        self.runtime.instructions_remaining.set(budget);
        self.max_depth = max_depth;
    }

    /// Finish exactly one parked cross-tier call. A missing/already-consumed
    /// continuation is rejected without changing the live execution.
    pub fn finish_semantic_call(&mut self, result: Result<u64, String>) -> Result<(), String> {
        let request = self.semantic_call.take()
            .ok_or_else(|| "no semantic call is awaiting a result".to_string())?;
        match result {
            Ok(value) => {
                self.runtime.hooks.on_call_exit(request.exit_function, value);
                self.deliver(value);
            }
            Err(error) => { self.fail(error); }
        }
        Ok(())
    }

    /// Mutate a paused frame's real register file, never a replay snapshot.
    pub fn set_register(
        &mut self,
        frame: usize,
        register: usize,
        value: u64,
    ) -> Result<(), String> {
        let slot = self
            .frames
            .get_mut(frame)
            .and_then(|f| f.registers.get_mut(register))
            .ok_or_else(|| "invalid frame or register".to_string())?;
        *slot = value;
        Ok(())
    }

    pub fn position(&self) -> Option<(u8, u32, usize)> {
        let frame = self.frames.last()?;
        let line = frame
            .bytecode
            .source_map
            .location(frame.instruction_pc())
            .map(|location| location.line)
            .unwrap_or(frame.bytecode.source_line);
        Some((frame.function, line, self.frames.len()))
    }

    fn enter(&mut self, function: u8, args: &[u64], tail_exit: Option<u8>) -> Result<(), String> {
        let slot = self
            .runtime
            .slots
            .get(function as usize)
            .ok_or_else(|| "invalid callable".to_string())?
            .borrow()
            .clone();
        if tail_exit.is_none() {
            self.runtime.hooks.on_call_enter(function, args);
        }
        match slot {
            Slot::Bytecode(bytecode) => {
                if self.frames.len() >= self.max_depth {
                    return Err("call depth exceeded".into());
                }
                if args.len() != bytecode.metadata.arity.parameters as usize {
                    return Err("argument count does not match callable".into());
                }
                let mut registers = vec![0; bytecode.metadata.registers as usize];
                registers[..args.len()].copy_from_slice(args);
                let guard = gc::RootGuard::new(registers.as_ptr(), registers.len());
                let mut header = sol_core::FrameHeader::new(
                    sol_core::FunctionId::new(function as u32),
                    0,
                    registers.len() as u32,
                );
                header.state = sol_core::FrameState::Running;
                self.runtime.hooks.on_frame_state(&header);
                self.frames.push(Frame {
                    identity: self.next_identity,
                    function,
                    bytecode,
                    registers,
                    pc: 0,
                    header,
                    exit_function: tail_exit.unwrap_or(function),
                    _guard: guard,
                    pending: None,
                });
                self.next_identity += 1;
            }
            Slot::Native(pointer) => {
                let _roots = gc::RootGuard::new(args.as_ptr(), args.len());
                let value = call_native(pointer, args);
                self.runtime
                    .hooks
                    .on_call_exit(tail_exit.unwrap_or(function), value);
                self.deliver(value);
            }
            Slot::Semantic(_) => {
                let arguments = args.to_vec();
                let guard = gc::RootGuard::new(arguments.as_ptr(), arguments.len());
                self.semantic_call = Some(SemanticCall {
                    function, arguments, exit_function: tail_exit.unwrap_or(function), _guard: guard,
                });
            }
        }
        Ok(())
    }

    fn deliver(&mut self, value: u64) {
        let Some(parent) = self.frames.last_mut() else {
            self.terminal = Some(Stop::Returned(value));
            return;
        };
        match parent
            .pending
            .take()
            .expect("a caller must own a return continuation")
        {
            Pending::Call(destination) => parent.registers[destination] = value,
            Pending::Map {
                roots,
                _guard,
                target,
                destination,
                index,
            } => {
                let output = roots[1] as *mut crate::runtime::ArrayHeader;
                unsafe {
                    *((*output).data.add(index * 8) as *mut u64) = value;
                }
                parent.pending = Some(Pending::Map {
                    roots,
                    _guard,
                    target,
                    destination,
                    index: index + 1,
                });
            }
        }
    }

    fn fail(&mut self, error: String) -> Stop {
        self.semantic_call = None;
        for frame in &mut self.frames {
            frame.header.state = sol_core::FrameState::Failed;
            self.runtime.hooks.on_frame_state(&frame.header);
        }
        self.frames.clear();
        let stop = Stop::Raised(error);
        self.terminal = Some(stop.clone());
        stop
    }

    /// Execute at most `limit` opcode/continuation operations. No Rust call
    /// stack or instruction-history buffer survives a pause. When
    /// `semantic_call()` is present, `Paused` waits for the external
    /// scheduler's `finish_semantic_call` instead of dispatching more code.
    pub fn resume(&mut self, limit: u64) -> Stop {
        if let Some(stop) = &self.terminal {
            return stop.clone();
        }
        for _ in 0..limit {
            if self.semantic_call.is_some() {
                break;
            }
            let remaining = self.runtime.instructions_remaining.get();
            if remaining == 0 {
                return self.fail("instruction budget exceeded".into());
            }
            self.runtime.instructions_remaining.set(remaining - 1);
            let parent = self
                .frames
                .last_mut()
                .expect("a running execution has a frame");
            parent.header.state = sol_core::FrameState::Running;
            if let Some(Pending::Map {
                roots,
                target,
                destination,
                index,
                ..
            }) = &parent.pending
            {
                let input = roots[0] as *const crate::runtime::ArrayHeader;
                if *index >= unsafe { (*input).len as usize } {
                    parent.registers[*destination] = roots[1];
                    parent.pending = None;
                } else {
                    let value = unsafe { *((*input).data.add(*index * 8) as *const u64) };
                    let target = *target;
                    if let Err(error) = self.enter(target, &[value], None) {
                        return self.fail(error);
                    }
                }
            } else {
                if parent.pc >= parent.bytecode.code.len() {
                    return self.fail("bytecode ended without return".into());
                }
                parent.header.pc = parent.pc as u32;
                let line = parent
                    .bytecode
                    .source_map
                    .location(parent.pc as u32)
                    .map(|location| location.line)
                    .unwrap_or(parent.bytecode.source_line);
                self.runtime.hooks.on_instruction(
                    parent.function,
                    parent.pc as u32,
                    line,
                    &parent.registers,
                );
                let step = self.runtime.execute_instruction(
                    parent.function,
                    &parent.bytecode,
                    &mut parent.registers,
                    &mut parent.pc,
                    &mut parent.header,
                    DispatchMode::PortableLive,
                );
                match step {
                    InstructionStep::Raised(error) => return self.fail(error),
                    InstructionStep::Continue => {}
                    InstructionStep::Call {
                        target,
                        destination,
                        args,
                    } => {
                        parent.pending = Some(Pending::Call(destination));
                        if let Err(error) = self.enter(target, &args, None) {
                            return self.fail(error);
                        }
                    }
                    InstructionStep::ArrayMap {
                        input,
                        output,
                        target,
                        destination,
                    } => {
                        let roots = Box::new([input as u64, output as u64]);
                        let guard = gc::RootGuard::new(roots.as_ptr(), roots.len());
                        parent.pending = Some(Pending::Map {
                            roots,
                            _guard: guard,
                            target,
                            destination,
                            index: 0,
                        });
                    }
                    InstructionStep::Returned(value) => {
                        let frame = self.frames.pop().unwrap();
                        self.runtime.hooks.on_call_exit(frame.exit_function, value);
                        self.deliver(value);
                    }
                    InstructionStep::TailCall(request) => {
                        let target = match u8::try_from(request.function.get()) {
                            Ok(target) => target,
                            Err(_) => return self.fail("invalid tail callable".into()),
                        };
                        let frame = self.frames.pop().unwrap();
                        self.runtime
                            .hooks
                            .on_tail_call(frame.function, target, &request.arguments);
                        if let Err(error) =
                            self.enter(target, &request.arguments, Some(frame.exit_function))
                        {
                            return self.fail(error);
                        }
                    }
                }
            }
            if let Some(stop) = &self.terminal {
                return stop.clone();
            }
        }
        for frame in &mut self.frames {
            frame.header.pc = frame.instruction_pc();
            frame.header.state = sol_core::FrameState::Suspended;
            self.runtime.hooks.on_frame_state(&frame.header);
        }
        Stop::Paused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier0::Engine;

    fn engine(source: &str, budget: u64) -> Engine {
        let (program, _) = crate::compile(source).unwrap();
        Engine::new_with_budget(program, (), budget).unwrap()
    }

    fn semantic_execution(source: &str, budget: u64) -> Execution<()> {
        let (program, _) = crate::compile(source).unwrap();
        let ids = crate::bccompile::function_index(&program).unwrap();
        assert!(program.externs.is_empty());
        let slots = program.functions.iter().map(|function| {
            if function.name == "external" {
                Slot::Semantic(Rc::new(|_| panic!("a live boundary must not invoke a blocking callback")))
            } else {
                Slot::Bytecode(Rc::new(crate::bccompile::compile_function(function, &ids).unwrap()))
            }
        }).collect();
        let runtime = Runtime::new(slots, u32::MAX, |_| None, u32::MAX, |_, _| None,
            crate::interp::SpeculativeConfig { candidates: std::collections::HashMap::new(), threshold: u32::MAX, promote: Box::new(|_| None) }, (), budget);
        Execution::new(Rc::new(runtime), ids["main"], &[]).unwrap()
    }

    #[test]
    fn semantic_calls_park_real_callers_without_invoking_callbacks_or_spending_wait_budget() {
        let mut live = semantic_execution("function external(x:i64):i64 return x end\nfunction main():i64 local n:i64=40 local v=external(n) return v+n end", 100);
        assert_eq!(live.resume(100), Stop::Paused);
        let request = live.semantic_call().unwrap();
        assert_eq!(request.arguments(), &[40]);
        assert_eq!(live.frames().len(), 1);
        assert!(live.frames()[0].is_waiting_for_result());
        let remaining = live.runtime.instructions_remaining.get();
        for _ in 0..10 { assert_eq!(live.resume(1_000_000), Stop::Paused); }
        assert_eq!(live.runtime.instructions_remaining.get(), remaining);
        let n = live.frames()[0].resolve_local("n").unwrap();
        live.set_register(0, n, 1).unwrap();
        live.finish_semantic_call(Ok(43)).unwrap();
        assert!(live.semantic_call().is_none());
        assert!(live.finish_semantic_call(Ok(99)).is_err());
        assert_eq!(live.resume(100), Stop::Returned(44));
    }

    #[test]
    fn semantic_tail_calls_keep_owned_arguments_after_removing_the_caller() {
        let mut live = semantic_execution("function external(x:i64):i64 return x end function main():i64 return external(42) end", 100);
        assert_eq!(live.resume(100), Stop::Paused);
        assert!(live.frames().is_empty());
        assert_eq!(live.semantic_call().unwrap().arguments(), &[42]);
        assert_eq!(live.resume(100), Stop::Paused);
        live.finish_semantic_call(Ok(44)).unwrap();
        assert_eq!(live.resume(0), Stop::Returned(44));
    }

    #[test]
    fn semantic_failures_clear_parked_frames_and_remain_terminal() {
        let mut live = semantic_execution("function external(x:i64):i64 return x end function main():i64 return external(40)+2 end", 100);
        assert_eq!(live.resume(100), Stop::Paused);
        live.finish_semantic_call(Err("generic failure".into())).unwrap();
        assert!(live.frames().is_empty());
        assert!(live.semantic_call().is_none());
        assert_eq!(live.resume(100), Stop::Raised("generic failure".into()));
        assert!(live.finish_semantic_call(Ok(42)).is_err());
    }

    #[test]
    fn mapped_semantic_callbacks_resume_their_existing_array_continuation() {
        let mut live = semantic_execution("function external(x:i64):i64 return x end function main():i64 local xs:Array<i64> = {10,20,30} local ys=map(xs,external) return ys[0]+ys[1]+ys[2] end", 1000);
        for expected in [10,20,30] {
            assert_eq!(live.resume(1000), Stop::Paused);
            assert_eq!(live.semantic_call().unwrap().arguments(), &[expected]);
            live.finish_semantic_call(Ok(expected+1)).unwrap();
        }
        assert_eq!(live.resume(1000), Stop::Returned(63));
    }

    #[test]
    fn resumes_real_frames_and_mutation_changes_the_result() {
        let engine = engine("function add(x: i64): i64 return x+2 end\nfunction main(): i64 local y = add(39) return y+1 end", 1000);
        let mut execution = engine.start_live("main", &[]).unwrap();
        while execution.frames().len() != 2 {
            assert_eq!(execution.resume(1), Stop::Paused);
        }
        assert_eq!(execution.frames()[1].registers[0], 39);
        execution.set_register(1, 0, 40).unwrap();
        assert_eq!(execution.resume(1000), Stop::Returned(43));
        assert!(execution.frames().is_empty());
    }

    #[test]
    fn zero_burst_does_not_execute_and_runaway_is_bounded() {
        let engine = engine(
            "function main(): i64 local x: i64 = 0 while true do x=x+1 end return x end",
            100,
        );
        let mut execution = engine.start_live("main", &[]).unwrap();
        assert_eq!(execution.resume(0), Stop::Paused);
        assert_eq!(execution.frames()[0].pc, 0);
        assert_eq!(execution.resume(10), Stop::Paused);
        assert_eq!(
            execution.resume(1000),
            Stop::Raised("instruction budget exceeded".into())
        );
    }

    #[test]
    fn native_string_helper_uses_the_same_calling_convention() {
        let engine = engine(
            "function main(): string return 'hello' .. ' world' end",
            1000,
        );
        let mut execution = engine.start_live("main", &[]).unwrap();
        let Stop::Returned(value) = execution.resume(1000) else {
            panic!("expected return")
        };
        let layouts = std::collections::HashMap::new();
        let renderer = crate::debugger::ValueRenderer { structs: &layouts };
        assert_eq!(
            renderer.render(&crate::types::Type::String, value),
            crate::debugger::DisplayValue::Scalar("\"hello world\"".into())
        );
    }

    #[test]
    fn mapped_callbacks_pause_without_a_rust_callback_stack() {
        let engine = engine("function add(x: i64): i64 return x+1 end\nfunction main(): i64 local xs=new_array_i64(3) xs[0]=10 xs[1]=20 xs[2]=30 local ys=map(xs,add) return ys[0]+ys[1]+ys[2] end", 1000);
        let mut execution = engine.start_live("main", &[]).unwrap();
        while execution.frames().len() != 2 {
            assert_eq!(execution.resume(1), Stop::Paused);
        }
        execution.set_register(1, 0, 40).unwrap();
        let mut pauses = 0;
        loop {
            match execution.resume(1) {
                Stop::Paused => pauses += 1,
                Stop::Returned(value) => {
                    assert_eq!(value, 93);
                    break;
                }
                other => panic!("unexpected map stop: {other:?}"),
            }
        }
        assert!(pauses > 3);
    }

    #[test]
    fn live_and_ordinary_execution_agree_on_portable_typed_fixtures() {
        for source in [
            include_str!("../../tests/fixtures/generic_map.sol"),
            include_str!("../../tests/fixtures/generic_map_f64.sol"),
            include_str!("../../tests/fixtures/map_scalar_values.sol"),
            "function sum(n: i64, total: i64): i64 if n == 0 then return total end return sum(n-1,total+n) end function main(): i64 return sum(10000,0) end",
        ] {
            let ordinary = engine(source, 1_000_000).call("main", &[]);
            let live_engine = engine(source, 1_000_000);
            let mut live = live_engine.start_live("main", &[]).unwrap();
            loop {
                assert!(live.frames().len() <= 2, "tail calls must not grow frames");
                match live.resume(100) {
                    Stop::Paused => {},
                    Stop::Returned(value) => { assert_eq!(value, ordinary); break; },
                    other => panic!("unexpected fixture stop: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn recursive_calls_report_depth_exhaustion_without_stack_overflow() {
        let engine = engine("function recurse(x: i64): i64 return recurse(x+1)+1 end function main(): i64 return recurse(0) end", 100_000);
        let mut live = engine.start_live("main", &[]).unwrap();
        assert_eq!(
            live.resume(100_000),
            Stop::Raised("call depth exceeded".into())
        );
        assert!(live.frames().is_empty());
    }

    #[test]
    fn debug_compilation_keeps_source_local_reads_mutable() {
        let source = "function main(): i64\n local x: i64 = 40\n return x+2\nend";
        let ast =
            crate::parser::parse(crate::lexer::lex_bytes(source.as_bytes()).unwrap()).unwrap();
        let (program, _) =
            crate::compile_debug_program(ast, crate::parser::SourceMode::Sol.into()).unwrap();
        let engine: Engine = Engine::new(program, ()).unwrap();
        let mut live = engine.start_live("main", &[]).unwrap();
        while live.position().unwrap().1 != 3 {
            assert_eq!(live.resume(1), Stop::Paused);
        }
        live.set_register(0, 0, 41).unwrap();
        assert_eq!(live.resume(100), Stop::Returned(43));
    }

    #[test]
    fn source_local_scopes_preserve_shadowing_and_initialization_boundaries() {
        let source = "function main(): i64\n local x: i64 = 40\n do\n  local x: i64 = 41\n  local y = x+1\n end\n return x+2\nend";
        let ast =
            crate::parser::parse(crate::lexer::lex_bytes(source.as_bytes()).unwrap()).unwrap();
        let (program, _) =
            crate::compile_debug_program(ast, crate::parser::SourceMode::Sol.into()).unwrap();
        let engine: Engine = Engine::new(program, ()).unwrap();
        let mut live = engine.start_live("main", &[]).unwrap();
        assert_eq!(live.frames()[0].resolve_local("x"), None);
        while live.position().unwrap().1 != 5 {
            assert_eq!(live.resume(1), Stop::Paused);
        }
        assert_eq!(live.frames()[0].resolve_local("x"), Some(1));
        assert_eq!(live.frames()[0].resolve_local("y"), None);
        while live.position().unwrap().1 != 7 {
            assert_eq!(live.resume(1), Stop::Paused);
        }
        assert_eq!(live.frames()[0].resolve_local("x"), Some(0));
        assert_eq!(live.frames()[0].resolve_local("y"), None);
        live.set_register(0, 0, 41).unwrap();
        assert_eq!(live.resume(100), Stop::Returned(43));
    }

    #[test]
    fn loop_variable_scope_ends_before_the_following_statement() {
        let source = "function main(): i64\n local total: i64 = 0\n for i=1,3 do\n  total=total+i\n end\n return total\nend";
        let ast =
            crate::parser::parse(crate::lexer::lex_bytes(source.as_bytes()).unwrap()).unwrap();
        let (program, _) =
            crate::compile_debug_program(ast, crate::parser::SourceMode::Sol.into()).unwrap();
        let engine: Engine = Engine::new(program, ()).unwrap();
        let mut live = engine.start_live("main", &[]).unwrap();
        while live.position().unwrap().1 != 4 {
            assert_eq!(live.resume(1), Stop::Paused);
        }
        assert!(live.frames()[0].resolve_local("i").is_some());
        while live.position().unwrap().1 != 6 {
            assert_eq!(live.resume(1), Stop::Paused);
        }
        assert_eq!(live.frames()[0].resolve_local("i"), None);
        assert_eq!(live.resume(100), Stop::Returned(6));
    }
}
