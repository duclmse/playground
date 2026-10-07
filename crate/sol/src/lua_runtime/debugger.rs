//! Live debugger driver for the canonical Lua trampoline. Suspended frames
//! remain in the runtime and participate in its ordinary precise root walk.

pub use super::analysis::{Analysis, FunctionStats, TimelineEvent};
use super::frame::{CallStep, DebugResumeRequest, DriveOutcome, Frame, NativeCont};
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugStatus {
    Paused,
    Returned,
    Raised(String),
}

pub struct LuaDebugSession {
    runtime: LuaRuntime,
    depth_charged: usize,
    status: DebugStatus,
    executable_lines: HashMap<String, HashSet<u32>>,
    resume_parents: Vec<ResumeParent>,
    incoming: Option<Vec<LuaValue>>,
    root_charged: bool,
    returned_values: Vec<LuaValue>,
    base_depth: usize,
    bridge_contexts: Vec<BridgeContext>,
    pending_outcome: Option<DriveOutcome>,
}

struct BridgeContext {
    base_depth: usize,
    depth_charged: usize,
    root_charged: bool,
    status: DebugStatus,
    resume_parents: Vec<ResumeParent>,
    incoming: Option<Vec<LuaValue>>,
    request: Option<(u8, Vec<LuaValue>)>,
    returned_values: Vec<LuaValue>,
}

struct ResumeParent {
    base_depth: usize,
    thread: ThreadRef,
    parent: ThreadRef,
    depth_charged: usize,
    hook: Option<Rc<super::coroutine::HookState>>,
    wrapped: bool,
}

impl LuaDebugSession {
    pub fn launch(source: &[u8], name: &str) -> LuaResult<Self> {
        Self::launch_with_config(source, name, crate::parser::LanguageConfig::LUA)
    }

    pub fn launch_with_config(source: &[u8], name: &str, config: crate::parser::LanguageConfig) -> LuaResult<Self> {
        let program =
            crate::parser::parse_with_config(crate::lexer::lex_bytes(source).map_err(LuaError::new)?, config)
                .map_err(LuaError::new)?;
        if config.sol_extensions && crate::semantics::requires_specialized_execution(&program) {
            return Err(LuaError::new("program requires specialized execution"));
        }
        let mut runtime = LuaRuntime::with_limits(10_000_000, 1_000);
        runtime.set_chunk_name(format!("@{name}").into_bytes());
        runtime.load_with_natives(&program, &HashSet::new(), HashMap::new())?;
        let LuaValue::Closure(closure) = runtime.globals.get(&runtime, "main") else {
            return Err(LuaError::new("debugger entry is not a Lua closure"));
        };
        let (proto, upvals, globals) = runtime.closure_parts(closure)?;
        runtime.call_depth += 1;
        let frame = runtime.new_lua_frame(closure, proto, upvals, globals, Vec::new(), 0)?;
        let mut executable_lines = HashMap::new();
        let mut lines = HashSet::new();
        collect_lines(&frame.proto, &mut lines);
        for function in &program.functions {
            let proto = crate::lua_bytecode::Compiler::compile_top_level(function)
                .map_err(LuaError::new)?;
            collect_lines(&proto, &mut lines);
        }
        executable_lines.insert(name.to_string(), lines);
        runtime.frames.push(Frame::Lua(frame));
        Ok(Self {
            runtime,
            depth_charged: 0,
            status: DebugStatus::Paused,
            executable_lines,
            resume_parents: Vec::new(),
            incoming: None,
            root_charged: true,
            returned_values: Vec::new(),
            base_depth: 0,
            bridge_contexts: Vec::new(),
            pending_outcome: None,
        })
    }

    pub fn add_module(&mut self, name: &[u8], source: &[u8]) {
        let source_name = format!("{}.lua", String::from_utf8_lossy(name).replace('.', "/"));
        self.add_named_module(name, &source_name, source);
    }

    pub fn add_named_module(&mut self, name: &[u8], source_name: &str, source: &[u8]) {
        self.runtime.add_module(name, source);
        self.register_module_lines(name, source_name, source);
    }

    pub fn add_generic_named_module(&mut self, name: &[u8], source_name: &str, source: &[u8], config: crate::parser::LanguageConfig) -> LuaResult<()> {
        self.runtime.add_generic_module(name, source, config)?;
        self.register_module_lines(name, source_name, source);
        Ok(())
    }

    fn register_module_lines(&mut self, name: &[u8], source_name: &str, source: &[u8]) {
        self.runtime
            .debug_module_names
            .insert(name.to_vec(), format!("@{source_name}").into_bytes());
        if let Ok(program) = self.runtime.parse_module_source(name, source) {
            let mut lines = HashSet::new();
            for function in &program.functions {
                if let Ok(proto) = crate::lua_bytecode::Compiler::compile_top_level(function) {
                    collect_lines(&proto, &mut lines);
                }
            }
            self.executable_lines.insert(source_name.to_string(), lines);
        }
    }

    pub fn has_executable_line(&self, source: &str, line: u32) -> bool {
        self.executable_lines
            .get(source)
            .is_some_and(|lines| lines.contains(&line))
    }

    /// Executes at most `instructions` outer bytecode instructions and
    /// retains the live frame stack when the slice expires.
    pub fn continue_burst(&mut self, instructions: u64) -> DebugStatus {
        if self.runtime.debug_semantic_request.is_some() { return DebugStatus::Paused; }
        if self.status != DebugStatus::Paused {
            return self.status.clone();
        }
        let mut remaining = instructions;
        let mut immediate = self.pending_outcome.take();
        let outcome = loop {
            let settling = self.incoming.is_some();
            self.runtime.debug_slice_remaining = Some(if settling { 0 } else { remaining });
            self.runtime.debug_pause_requested = false;
            let outcome = if let Some(outcome) = immediate.take() {
                outcome
            } else {
                let outcome = self
                    .runtime
                    .drive(self.base_depth, &mut self.depth_charged, self.incoming.take());
                if settling {
                    self.runtime.debug_incoming_roots.clear();
                }
                outcome
            };
            if !settling {
                remaining = self.runtime.debug_slice_remaining.unwrap_or(0);
            }
            let paused = self.runtime.debug_pause_requested;
            if self.runtime.debug_semantic_request.is_some() {
                self.runtime.debug_slice_remaining = None;
                return DebugStatus::Paused;
            }
            if let Some(request) = self.runtime.debug_resume_request.take() {
                immediate = self.begin_resume(request);
                self.runtime.debug_slice_remaining = None;
                if remaining == 0 && immediate.is_none() && self.incoming.is_none() {
                    return DebugStatus::Paused;
                }
                continue;
            }
            self.runtime.debug_slice_remaining = None;
            if paused {
                if settling && remaining > 0 {
                    continue;
                }
                return DebugStatus::Paused;
            }
            if !self.resume_parents.is_empty() {
                immediate = self.finish_resume(outcome);
                if remaining == 0 && immediate.is_none() && self.incoming.is_none() {
                    return DebugStatus::Paused;
                }
                continue;
            }
            break outcome;
        };
        self.status = match outcome {
            DriveOutcome::Returned(values) => {
                for value in &values {
                    if !matches!(value, LuaValue::Integer(_) | LuaValue::Float(_) | LuaValue::Bool(_) | LuaValue::Nil) { self.retain(value); }
                }
                self.returned_values = values;
                DebugStatus::Returned
            }
            DriveOutcome::Yielded(_) => {
                DebugStatus::Raised("attempt to yield outside a coroutine".into())
            }
            DriveOutcome::Raised(error) => {
                let error = self.runtime.close_frames_above(self.base_depth, error);
                self.runtime.release_call_depth(self.depth_charged);
                self.depth_charged = 0;
                DebugStatus::Raised(error.to_string())
            }
            DriveOutcome::TailCall(_) => unreachable!("trampoline consumes tail calls"),
        };
        if self.status != DebugStatus::Paused && self.root_charged {
            self.runtime.release_call_depth(1);
            self.root_charged = false;
        }
        self.status.clone()
    }

    /// Rooted terminal results for a semantic scheduler. Chunk returns are
    /// deliberately separate from captured output, and unavailable while
    /// paused or failed. Handles remain owned by this session.
    pub fn returned_values(&self) -> Option<&[LuaValue]> {
        (self.status == DebugStatus::Returned).then_some(self.returned_values.as_slice())
    }

    pub(crate) fn bridge_session(program: &crate::ast::Program, typed: &[(String, u8)], contracts: &[crate::modules::DynamicModuleContract], entry: &str) -> LuaResult<Self> {
        let excluded = typed.iter().map(|(name, _)| name.clone()).collect::<HashSet<_>>();
        let mut runtime = LuaRuntime::with_limits(10_000_000, 1000);
        runtime.set_chunk_name(format!("@{entry}").into_bytes());
        runtime.load_with_natives(program, &excluded, HashMap::new())?;
        for (name, function) in typed {
            let callable = sol_core::NativeCallableId::new(3, *function as u32);
            runtime.semantic_functions.insert(callable, *function);
            runtime.globals.clone().define(&runtime, name, LuaValue::RegisteredNative(callable), false);
        }
        for contract in contracts { runtime.preload_namespace_module(&contract.name, &contract.exports)?; }
        let mut executable_lines = HashMap::new();
        for function in &program.functions {
            if excluded.contains(&function.name) { continue; }
            let proto = crate::lua_bytecode::Compiler::compile_top_level(function).map_err(LuaError::new)?;
            let mut lines = HashSet::new(); collect_lines(&proto, &mut lines);
            executable_lines.entry(function.source_file.clone().unwrap_or_else(|| entry.into())).or_insert_with(HashSet::new).extend(lines);
        }
        Ok(Self { runtime, depth_charged: 0, status: DebugStatus::Paused, executable_lines,
            resume_parents: Vec::new(), incoming: None, root_charged: false, returned_values: Vec::new(),
            base_depth: 0, bridge_contexts: Vec::new(), pending_outcome: None })
    }

    pub(crate) fn begin_bridge_call(&mut self, name: &str, args: Vec<LuaValue>) -> LuaResult<()> {
        if self.runtime.call_depth >= self.runtime.max_call_depth { return Err(self.runtime.call_depth_overflow_error()); }
        let LuaValue::Closure(closure) = self.runtime.globals.get(&self.runtime, name) else { return Err(LuaError::new(format!("'{name}' is not a generic closure"))); };
        let (proto, upvals, globals) = self.runtime.closure_parts(closure)?;
        let frame = self.runtime.new_lua_frame(closure, proto, upvals, globals, args, 0)?;
        self.bridge_contexts.push(BridgeContext { base_depth: self.base_depth, depth_charged: self.depth_charged,
            root_charged: self.root_charged, status: self.status.clone(), resume_parents: std::mem::take(&mut self.resume_parents),
            incoming: self.incoming.take(), request: self.runtime.debug_semantic_request.take(), returned_values: std::mem::take(&mut self.returned_values) });
        self.base_depth = self.runtime.frames.len();
        self.runtime.frames.push(Frame::Lua(frame));
        self.runtime.call_depth += 1;
        self.depth_charged = 0; self.root_charged = true; self.status = DebugStatus::Paused;
        Ok(())
    }

    pub(crate) fn end_bridge_call(&mut self) -> Result<Vec<LuaValue>, String> {
        if self.status == DebugStatus::Paused { return Err("generic call is still running".into()); }
        let result = match &self.status { DebugStatus::Returned => Ok(std::mem::take(&mut self.returned_values)), DebugStatus::Raised(error) => Err(error.clone()), _ => unreachable!() };
        let context = self.bridge_contexts.pop().expect("a bridge callee owns a context");
        self.base_depth = context.base_depth; self.depth_charged = context.depth_charged; self.root_charged = context.root_charged;
        self.status = context.status; self.resume_parents = context.resume_parents; self.incoming = context.incoming;
        self.runtime.debug_semantic_request = context.request; self.returned_values = context.returned_values;
        result
    }

    pub(crate) fn semantic_request(&self) -> Option<&(u8, Vec<LuaValue>)> { self.runtime.debug_semantic_request.as_ref() }
    pub(crate) fn finish_semantic_request(&mut self, result: Result<Vec<LuaValue>, String>) {
        assert!(self.runtime.debug_semantic_request.take().is_some());
        match result {
            Ok(values) => self.set_incoming(values),
            Err(error) => {
                self.pending_outcome = match self.runtime.unwind_error_to_marker(LuaError::new(error), self.base_depth, &mut self.depth_charged) {
                    Ok(CallStep::Pending) => None,
                    Ok(CallStep::Done(values)) => {
                        if self.runtime.frames.len() == self.base_depth { Some(DriveOutcome::Returned(values)) }
                        else { self.set_incoming(values); None }
                    },
                    Ok(CallStep::Yielded(values)) => Some(DriveOutcome::Yielded(values)),
                    Err(error) => Some(DriveOutcome::Raised(error)),
                };
            }
        }
    }
    pub(crate) fn bridge_budget(&self) -> u64 { self.runtime.instructions_remaining }
    pub(crate) fn configure_bridge_limits(&mut self, budget: u64, max_depth: usize) {
        self.runtime.instructions_remaining = budget; self.runtime.max_call_depth = max_depth;
    }
    pub(crate) fn bridge_depth(&self) -> usize { self.runtime.call_depth }
    pub(crate) fn bridge_string_result(&self, bytes:&[u8]) -> LuaValue { LuaValue::String(self.runtime.intern_str(bytes)) }
    /// Stable activation identities, in the same order as `frames()`.
    pub fn frame_identities(&self) -> Vec<u64> {
        self.runtime.frames.iter().rev().filter_map(|frame|match frame {Frame::Lua(frame)=>Some(frame.debug_identity),_=>None}).collect()
    }

    /// One bit per live/suspended activation, not a history of retired frames.
    /// Resuming a yielded frame must not count a second function invocation.
    pub fn take_profile_calls(&mut self) -> Vec<String> {
        self.runtime.frames.iter_mut().filter_map(|frame| {
            let Frame::Lua(frame) = frame else { return None; };
            if frame.debug_profile_seen { return None; }
            frame.debug_profile_seen = true;
            Some(frame.proto.metadata.name.clone())
        }).collect()
    }

    fn deliver_resume_error(&mut self, error: LuaError, wrapped: bool) -> Option<DriveOutcome> {
        if !wrapped {
            self.set_incoming(vec![
                LuaValue::Bool(false),
                error.into_lua_value(&self.runtime.canonical_heap),
            ]);
            return None;
        }
        match self
            .runtime
            .unwind_error_to_marker(error, self.base_depth, &mut self.depth_charged)
        {
            Ok(CallStep::Pending) => None,
            Ok(CallStep::Done(values)) => {
                self.set_incoming(values);
                None
            }
            Ok(CallStep::Yielded(values)) => Some(DriveOutcome::Yielded(values)),
            Err(error) => Some(DriveOutcome::Raised(error)),
        }
    }

    fn begin_resume(&mut self, request: DebugResumeRequest) -> Option<DriveOutcome> {
        let co = self.runtime.coroutine(request.thread);
        if co.status.get() != CoroutineStatus::Suspended {
            let message = if co.status.get() == CoroutineStatus::Dead {
                "cannot resume dead coroutine"
            } else {
                "cannot resume non-suspended coroutine"
            };
            if !request.wrapped {
                if let Err(error) = self.runtime.fire_hook("return", None) {
                    return self.deliver_resume_error(error, true);
                }
            }
            return self.deliver_resume_error(LuaError::new(message), request.wrapped);
        }
        if self.runtime.call_depth >= self.runtime.max_call_depth {
            let error = self.runtime.call_depth_overflow_error();
            return self.deliver_resume_error(error, true);
        }
        self.runtime.call_depth += 1;
        self.depth_charged += 1;
        let parent = self
            .runtime
            .coroutine_stack
            .last()
            .copied()
            .unwrap_or(self.runtime.main_coroutine);
        let parent_co = self.runtime.coroutine(parent);
        *parent_co.frames.borrow_mut() =
            std::mem::replace(&mut self.runtime.frames, co.frames.take());
        parent_co.status.set(CoroutineStatus::Normal);
        co.status.set(CoroutineStatus::Running);
        self.runtime.coroutine_stack.push(request.thread);
        let hook = std::mem::replace(&mut self.runtime.active_hook, co.hook.borrow().clone());
        let depth_charged = std::mem::replace(&mut self.depth_charged, co.depth_charged.take());
        self.runtime.call_depth += self.depth_charged;
        self.resume_parents.push(ResumeParent {
            base_depth: std::mem::replace(&mut self.base_depth, 0),
            thread: request.thread,
            parent,
            depth_charged,
            hook,
            wrapped: request.wrapped,
        });
        if self.runtime.frames.is_empty() {
            let body = co.body.borrow_mut().take().expect("fresh coroutine body");
            self.runtime.frames.push(Frame::Native(NativeCont::Once));
            self.runtime.debug_drive_nesting += 1;
            let resolved =
                self.runtime
                    .resolve_call(body, request.args, 0, &mut self.depth_charged);
            self.runtime.debug_drive_nesting -= 1;
            match resolved {
                Ok(CallStep::Pending) => None,
                Ok(CallStep::Done(values)) => {
                    self.set_incoming(values);
                    None
                }
                Ok(CallStep::Yielded(values)) => Some(DriveOutcome::Yielded(values)),
                Err(error) => Some(DriveOutcome::Raised(error)),
            }
        } else {
            self.set_incoming(request.args);
            match self.runtime.fire_hook("return", None) {
                Ok(()) => None,
                Err(error) => Some(DriveOutcome::Raised(error)),
            }
        }
    }

    fn finish_resume(&mut self, outcome: DriveOutcome) -> Option<DriveOutcome> {
        self.runtime.debug_incoming_roots = match &outcome {
            DriveOutcome::Returned(values) | DriveOutcome::Yielded(values) => values
                .iter()
                .filter_map(|value| self.runtime.encode_value(value).ok())
                .collect(),
            DriveOutcome::Raised(error) => error
                .value
                .as_ref()
                .and_then(|value| self.runtime.encode_value(value).ok())
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let state = self.resume_parents.pop().unwrap();
        let co = self.runtime.coroutine(state.thread);
        let result = match outcome {
            DriveOutcome::Yielded(values) => {
                co.status.set(CoroutineStatus::Suspended);
                co.depth_charged.set(self.depth_charged);
                self.runtime.release_call_depth(self.depth_charged);
                Ok(values)
            }
            DriveOutcome::Returned(values) => {
                co.status.set(CoroutineStatus::Dead);
                Ok(values)
            }
            DriveOutcome::Raised(error) => {
                self.runtime.release_call_depth(self.depth_charged);
                self.depth_charged = 0;
                co.status.set(CoroutineStatus::Dead);
                let error = if error.uncatchable {
                    self.runtime.close_frames_above_optional(0, None)
                } else {
                    Some(self.runtime.close_frames_above(0, error))
                };
                match error {
                    Some(error) => {
                        *co.dead_error.borrow_mut() =
                            Some(error.clone().into_lua_value(&self.runtime.canonical_heap));
                        Err(error)
                    }
                    None => Ok(Vec::new()),
                }
            }
            DriveOutcome::TailCall(_) => unreachable!("trampoline consumes tail calls"),
        };
        let parent = self.runtime.coroutine(state.parent);
        *co.frames.borrow_mut() = std::mem::replace(&mut self.runtime.frames, parent.frames.take());
        self.runtime.active_hook = state.hook;
        self.runtime.coroutine_stack.pop();
        parent.status.set(CoroutineStatus::Running);
        self.depth_charged = state.depth_charged - 1;
        self.base_depth = state.base_depth;
        self.runtime.release_call_depth(1);
        if !state.wrapped || result.is_ok() {
            if let Err(error) = self.runtime.fire_hook("return", None) {
                return self.deliver_resume_error(error, true);
            }
        }
        match result {
            Ok(mut values) => {
                if !state.wrapped {
                    values.insert(0, LuaValue::Bool(true));
                }
                self.set_incoming(values);
                None
            }
            Err(error) => self.deliver_resume_error(error, state.wrapped),
        }
    }

    fn set_incoming(&mut self, values: Vec<LuaValue>) {
        self.runtime.debug_incoming_roots = values
            .iter()
            .filter_map(|value| self.runtime.encode_value(value).ok())
            .collect();
        self.incoming = Some(values);
    }

    /// Thread IDs are positions on the active resume chain, matching the
    /// browser protocol's main-thread zero and per-stop coroutine selection.
    pub fn threads(&self) -> Vec<(u32, String)> {
        let mut threads = self
            .all_resume_parents()
            .enumerate()
            .map(|(id, state)| {
                (
                    id as u32,
                    self.runtime
                        .coroutine(state.parent)
                        .status
                        .get()
                        .as_str()
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        threads.push((
            self.active_thread(),
            if self.status == DebugStatus::Paused {
                "running"
            } else {
                "dead"
            }
            .to_string(),
        ));
        threads
    }

    fn all_resume_parents(&self) -> impl Iterator<Item = &ResumeParent> {
        self.bridge_contexts
            .iter()
            .flat_map(|context| context.resume_parents.iter())
            .chain(self.resume_parents.iter())
    }

    pub(crate) fn active_thread(&self) -> u32 {
        self.all_resume_parents().count() as u32
    }

    pub(crate) fn total_frame_depth(&self) -> usize {
        self.frames().len()
            + self.all_resume_parents().map(|state| {
                self.runtime.coroutine(state.parent).frames.borrow().iter()
                    .filter(|frame| matches!(frame, Frame::Lua(_))).count()
            }).sum::<usize>()
    }

    pub fn with_thread<T>(
        &mut self,
        thread: u32,
        operation: impl FnOnce(&mut Self) -> LuaResult<T>,
    ) -> LuaResult<T> {
        let thread = thread as usize;
        if thread > self.active_thread() as usize {
            return Err(LuaError::new("unknown thread"));
        }
        if thread == self.active_thread() as usize {
            return operation(self);
        }
        let selected = self.runtime.coroutine(self.all_resume_parents().nth(thread).unwrap().parent);
        let active = self
            .runtime
            .coroutine(*self.runtime.coroutine_stack.last().unwrap());
        *active.frames.borrow_mut() =
            std::mem::replace(&mut self.runtime.frames, selected.frames.take());
        let stack = self.runtime.coroutine_stack.clone();
        self.runtime.pinned_roots.push(
            stack
                .iter()
                .map(|thread| sol_core::Value::object(thread.object_id()))
                .collect(),
        );
        self.runtime.coroutine_stack.truncate(thread);
        let hook = std::mem::replace(
            &mut self.runtime.active_hook,
            selected.hook.borrow().clone(),
        );
        let result = operation(self);
        *selected.frames.borrow_mut() =
            std::mem::replace(&mut self.runtime.frames, active.frames.take());
        self.runtime.coroutine_stack = stack;
        self.runtime.active_hook = hook;
        self.runtime.pinned_roots.pop();
        result
    }

    pub fn position(&self) -> Option<(String, u32, usize)> {
        let frame = self
            .runtime
            .frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame),
                _ => None,
            })?;
        let source = self
            .runtime
            .chunk_sources
            .get(&(Rc::as_ptr(&frame.proto) as usize))
            .map(|source| {
                String::from_utf8_lossy(source)
                    .trim_start_matches('@')
                    .to_string()
            })
            .unwrap_or_else(|| frame.proto.metadata.name.clone());
        let line = frame.proto.source_map.location(frame.header.pc)?.line;
        let mut depth = self
            .runtime
            .frames
            .iter()
            .filter(|frame| matches!(frame, Frame::Lua(_)))
            .count();
        depth += self
            .resume_parents
            .iter()
            .map(|state| {
                self.runtime
                    .coroutine(state.parent)
                    .frames
                    .borrow()
                    .iter()
                    .filter(|frame| matches!(frame, Frame::Lua(_)))
                    .count()
            })
            .sum::<usize>();
        Some((source, line, depth))
    }

    pub fn program_counter(&self) -> Option<u32> {
        self.runtime
            .frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame.header.pc),
                _ => None,
            })
    }

    pub fn breakpoint_candidate(&mut self) -> bool {
        let Some(frame) = self
            .runtime
            .frames
            .iter_mut()
            .rev()
            .find_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame),
                _ => None,
            })
        else {
            return false;
        };
        if !matches!(frame.pending, super::frame::Pending::None) {
            return false;
        }
        let line = frame
            .proto
            .source_map
            .location(frame.header.pc)
            .map(|location| location.line);
        let candidate = line != frame.debugger_last_line
            || frame
                .debugger_last_pc
                .is_some_and(|pc| frame.header.pc <= pc);
        frame.debugger_last_line = line;
        frame.debugger_last_pc = Some(frame.header.pc);
        candidate && line.is_some()
    }

    pub fn locals(&self, frame_index: usize) -> Vec<(String, LuaValue)> {
        let Some(frame) = self
            .runtime
            .frames
            .iter()
            .rev()
            .filter_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame),
                _ => None,
            })
            .nth(frame_index)
        else {
            return Vec::new();
        };
        frame
            .proto
            .locals
            .iter()
            .filter(|local| local.start_pc <= frame.header.pc && frame.header.pc < local.end_pc)
            .map(|local| {
                (
                    local.name.clone(),
                    super::util::reg_get(
                        &self.runtime,
                        &frame.regs,
                        &frame.cells,
                        local.register as usize,
                    ),
                )
            })
            .collect()
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        self.runtime.take_output()
    }

    pub fn write_debug_output(&mut self, bytes: &[u8]) -> LuaResult<()> {
        self.runtime.charge_allocation(bytes.len(), None)?;
        self.runtime.output.extend_from_slice(bytes);
        Ok(())
    }

    pub fn frames(&self) -> Vec<(String, String, Option<u32>)> {
        self.runtime
            .frames
            .iter()
            .rev()
            .filter_map(|frame| {
                let Frame::Lua(frame) = frame else {
                    return None;
                };
                let source = self
                    .runtime
                    .chunk_sources
                    .get(&(Rc::as_ptr(&frame.proto) as usize))
                    .map(|source| {
                        String::from_utf8_lossy(source)
                            .trim_start_matches('@')
                            .to_string()
                    })
                    .unwrap_or_default();
                Some((
                    frame.proto.metadata.name.clone(),
                    source,
                    frame
                        .proto
                        .source_map
                        .location(frame.header.pc)
                        .map(|location| location.line),
                ))
            })
            .collect()
    }

    pub fn upvalues(&self, frame_index: usize) -> Vec<(String, LuaValue)> {
        let Some(frame) = self
            .runtime
            .frames
            .iter()
            .rev()
            .filter_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame),
                _ => None,
            })
            .nth(frame_index)
        else {
            return Vec::new();
        };
        frame
            .proto
            .upval_names
            .iter()
            .zip(frame.upvals.iter())
            .map(|(name, cell)| {
                let value = self
                    .runtime
                    .canonical_heap
                    .borrow()
                    .upvalue_value(cell.get())
                    .expect("live frame upvalue");
                (
                    name.clone(),
                    self.runtime.decode_value(value).expect("live frame value"),
                )
            })
            .collect()
    }

    pub fn globals(&self) -> Vec<(String, LuaValue)> {
        let LuaValue::Table(table) = self.runtime.globals.as_value() else {
            return Vec::new();
        };
        self.runtime
            .table_entries(table)
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| {
                (
                    String::from_utf8_lossy(&key.display_bytes()).into_owned(),
                    value,
                )
            })
            .collect()
    }

    /// Evaluates in an isolated binding table, while sharing referenced Lua
    /// objects. The paused program's live frames remain roots throughout.
    pub fn evaluate(&mut self, frame_index: usize, expression: &str) -> LuaResult<LuaValue> {
        if self
            .runtime
            .frames
            .iter()
            .filter(|frame| matches!(frame, Frame::Lua(_)))
            .nth(frame_index)
            .is_none()
        {
            return Err(LuaError::new("unknown frame"));
        }
        let bindings = self
            .globals()
            .into_iter()
            .chain(self.upvalues(frame_index))
            .chain(self.locals(frame_index))
            .collect::<Vec<_>>();
        let env = Globals::root(&self.runtime.canonical_heap);
        // Keep the new environment rooted before compilation may collect.
        let env_value = env.as_value();
        let root = self.runtime.encode_value(&env_value)?;
        self.runtime.pinned_roots.push(vec![root]);
        let result = (|| {
            for (name, value) in bindings {
                env.assign(&self.runtime, &name, value)?;
            }
            let closure = self
                .runtime
                .compile_chunk(format!("return {expression}").as_bytes(), Some(env_value))
                .map_err(LuaError::new)?;
            let values = self.runtime.call(closure, Vec::new())?;
            Ok(values.into_iter().next().unwrap_or(LuaValue::Nil))
        })();
        self.runtime.pinned_roots.pop();
        result
    }

    pub fn set_variable(
        &mut self,
        frame_index: usize,
        name: &str,
        expression: &str,
    ) -> LuaResult<LuaValue> {
        let value = self.evaluate(frame_index, expression)?;
        let encoded = self.runtime.encode_value(&value)?;
        let index = self
            .runtime
            .frames
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, frame)| matches!(frame, Frame::Lua(_)))
            .nth(frame_index)
            .map(|(index, _)| index)
            .ok_or_else(|| LuaError::new("unknown frame"))?;
        let Frame::Lua(mut frame) = self.runtime.frames.remove(index) else {
            unreachable!()
        };
        let register = frame
            .proto
            .locals
            .iter()
            .rev()
            .find(|local| {
                local.name == name
                    && local.start_pc <= frame.header.pc
                    && frame.header.pc < local.end_pc
            })
            .map(|local| local.register);
        if let Some(register) = register {
            super::util::reg_set(
                &self.runtime,
                &mut frame.regs,
                &frame.cells,
                register as usize,
                value.clone(),
            );
        }
        let upvalue = if register.is_none() {
            frame
                .proto
                .upval_names
                .iter()
                .position(|upvalue| upvalue == name)
                .map(|index| frame.upvals[index].get())
        } else {
            None
        };
        if let Some(cell) = upvalue {
            self.runtime
                .canonical_heap
                .borrow_mut()
                .set_upvalue(cell, encoded)
                .expect("live frame upvalue");
        }
        self.runtime.frames.insert(index, Frame::Lua(frame));
        if register.is_none() && upvalue.is_none() {
            return Err(LuaError::new(format!("unknown local or upvalue '{name}'")));
        }
        Ok(value)
    }

    pub fn table_entries(&self, value: &LuaValue) -> Vec<(String, LuaValue)> {
        let LuaValue::Table(table) = value else {
            return Vec::new();
        };
        self.runtime
            .table_entries(*table)
            .unwrap_or_default()
            .into_iter()
            .map(|(key, value)| {
                (
                    String::from_utf8_lossy(&key.display_bytes()).into_owned(),
                    value,
                )
            })
            .collect()
    }

    pub fn retain(&mut self, value: &LuaValue) {
        if let Ok(value) = self.runtime.encode_value(value) {
            self.runtime.pinned_roots.push(vec![value]);
        }
    }

    pub fn metatable(&self, value: &LuaValue) -> Option<LuaValue> {
        let LuaValue::Table(table) = value else {
            return None;
        };
        self.runtime.table_metatable(*table).map(LuaValue::Table)
    }

    pub fn force_gc(&mut self) {
        self.runtime.collect_garbage();
    }
    pub fn live_bytes(&self) -> usize {
        self.runtime.live_heap_bytes()
    }

    pub fn live_objects(&self) -> usize {
        self.runtime.canonical_heap.borrow().len()
    }

    pub fn analyze(&mut self, max_events: usize) -> Analysis {
        let mut recording = super::analysis::Recording::new(max_events);
        for frame in &self.runtime.frames {
            if let Frame::Lua(frame) = frame {
                recording.call(self.runtime.analysis_name(frame));
            }
        }
        self.runtime.debug_recording = Some(recording);
        let active = self
            .runtime
            .frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Lua(frame) => Some(frame.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for frame in active {
            self.runtime.record_event("call", &frame);
        }
        while self.continue_burst(10_000) == DebugStatus::Paused {}
        let recording = self.runtime.debug_recording.take().unwrap();
        Analysis {
            functions: recording.functions.into_values().collect(),
            events: recording.events,
            truncated: recording.truncated,
            error: match &self.status {
                DebugStatus::Raised(error) => Some(error.clone()),
                _ => None,
            },
        }
    }
}

impl Drop for LuaDebugSession {
    fn drop(&mut self) {
        loop {
            while let Some(state) = self.resume_parents.pop() {
                self.runtime.close_frames_above_optional(0, None);
                self.runtime.release_call_depth(self.depth_charged);
                self.runtime
                    .coroutine(state.thread)
                    .status
                    .set(CoroutineStatus::Dead);
                let parent = self.runtime.coroutine(state.parent);
                self.runtime.frames = parent.frames.take();
                self.runtime.active_hook = state.hook;
                self.runtime.coroutine_stack.pop();
                parent.status.set(CoroutineStatus::Running);
                self.depth_charged = state.depth_charged;
                self.base_depth = state.base_depth;
            }
            self.runtime.close_frames_above_optional(self.base_depth, None);
            self.runtime.release_call_depth(self.depth_charged);
            if self.root_charged {
                self.runtime.release_call_depth(1);
            }
            let Some(context) = self.bridge_contexts.pop() else { break; };
            self.base_depth = context.base_depth;
            self.depth_charged = context.depth_charged;
            self.root_charged = context.root_charged;
            self.resume_parents = context.resume_parents;
        }
    }
}

fn collect_lines(proto: &crate::lua_bytecode::Proto, lines: &mut HashSet<u32>) {
    for pc in 0..proto.source_map.len() {
        if let Some(location) = proto.source_map.location(pc as u32) {
            lines.insert(location.line);
        }
    }
    for nested in &proto.nested {
        collect_lines(nested, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_semantic_results_are_separate_from_output_and_survive_collection() {
        let mut session = LuaDebugSession::launch(
            b"local t={answer=42}; return t, 'result', nil", "callee.lua").unwrap();
        assert!(session.returned_values().is_none());
        assert_eq!(session.continue_burst(1000), DebugStatus::Returned);
        assert!(session.take_output().is_empty());
        session.force_gc();
        let values = session.returned_values().unwrap();
        assert_eq!(values.len(), 3);
        assert!(session.table_entries(&values[0]).iter().any(|(name, value)| name.contains("answer") && *value == LuaValue::Integer(42)));
        assert!(matches!(&values[1], LuaValue::String(bytes) if bytes.as_bytes() == b"result"));
        assert_eq!(values[2], LuaValue::Nil);
        assert_eq!(session.continue_burst(1000), DebugStatus::Returned);
        assert_eq!(session.returned_values().unwrap().len(), 3);
        let mut failed = LuaDebugSession::launch(b"error('expected')", "failed.lua").unwrap();
        assert!(matches!(failed.continue_burst(1000), DebugStatus::Raised(_)));
        assert!(failed.returned_values().is_none());
    }
    #[test]
    fn generic_sol_extensions_use_live_generic_frames_without_return_echo() {
        let mut session = LuaDebugSession::launch_with_config(
            b"fn f(n)\n local x=n+2\n print(x)\nend\nf(40)", "main.sol", crate::parser::LanguageConfig::SOL).unwrap();
        assert!(session.has_executable_line("main.sol", 3));
        while session.position().unwrap().1 != 3 {
            assert_eq!(session.continue_burst(1), DebugStatus::Paused);
        }
        assert!(session.take_output().is_empty());
        assert_eq!(session.evaluate(0, "x").unwrap(), LuaValue::Integer(42));
        session.set_variable(0, "x", "44").unwrap();
        assert_eq!(session.continue_burst(100), DebugStatus::Returned);
        assert_eq!(session.take_output(), b"44\n");
        assert!(LuaDebugSession::launch_with_config(
            b"function main():i64 return 42 end", "main.sol", crate::parser::LanguageConfig::SOL).is_err());
    }

    #[test]
    fn live_slices_preserve_locals_and_delay_output_until_execution() {
        let mut session =
            LuaDebugSession::launch(b"local x = 40\nx = x + 2\nprint(x)", "main.lua").unwrap();
        assert!(session.take_output().is_empty());
        for _ in 0..100 {
            if session.position().is_some_and(|(_, line, _)| line == 3) {
                break;
            }
            assert_eq!(session.continue_burst(1), DebugStatus::Paused);
        }
        assert_eq!(
            session
                .locals(0)
                .iter()
                .find(|(name, _)| name == "x")
                .unwrap()
                .1,
            LuaValue::Integer(42)
        );
        assert!(session.take_output().is_empty());
        assert_eq!(session.continue_burst(100), DebugStatus::Returned);
        assert_eq!(session.take_output(), b"42\n");
    }

    #[test]
    fn a_runaway_loop_returns_control_each_slice() {
        let mut session =
            LuaDebugSession::launch(b"local x = 0\nwhile true do x = x + 1 end", "main.lua")
                .unwrap();
        for _ in 0..10 {
            assert_eq!(session.continue_burst(100), DebugStatus::Paused);
        }
        assert!(session.position().is_some());
    }

    #[test]
    fn live_nested_frames_evaluation_mutation_and_gc() {
        let mut session = LuaDebugSession::launch(b"local x = 40\nlocal function f()\n local t = {answer = x}\n print(t.answer)\nend\nf()", "main.lua").unwrap();
        for _ in 0..200 {
            if session
                .position()
                .is_some_and(|(_, line, depth)| line == 4 && depth == 2)
            {
                break;
            }
            assert_eq!(session.continue_burst(1), DebugStatus::Paused);
        }
        assert_eq!(session.frames().len(), 2);
        assert!(session.evaluate(99, "42").is_err());
        assert_eq!(
            session.evaluate(0, "t.answer + x").unwrap(),
            LuaValue::Integer(80)
        );
        session.set_variable(0, "t", "{answer = 42}").unwrap();
        session.force_gc();
        assert_eq!(
            session.evaluate(0, "t.answer").unwrap(),
            LuaValue::Integer(42)
        );
        assert_eq!(session.continue_burst(100), DebugStatus::Returned);
        assert_eq!(session.take_output(), b"42\n");
    }

    #[test]
    fn profiling_counts_activations_and_timeline_is_bounded() {
        let source = b"local function f(n)\n return n+1\nend\nlocal x=f(40)\nx=f(x)\nprint(x)";
        let mut session = LuaDebugSession::launch(source, "main.lua").unwrap();
        let analysis = session.analyze(2);
        assert_eq!(analysis.error, None);
        assert_eq!(analysis.events.len(), 2);
        assert!(analysis.truncated);
        let callee = analysis
            .functions
            .iter()
            .find(|stat| stat.name.ends_with(":f"))
            .unwrap();
        assert_eq!(callee.calls, 2);
        assert!(callee.self_instructions > 0);
        let main = analysis
            .functions
            .iter()
            .find(|stat| stat.name.ends_with(":main"))
            .unwrap();
        assert_eq!(main.calls, 1);
        assert!(main.total_instructions > main.self_instructions);
        assert_eq!(session.take_output(), b"42\n");
    }

    #[test]
    fn coroutine_call_depth_budget_agrees_with_the_normal_driver() {
        let source = b"local n=0; local function f() n=n+1; local ok,err=coroutine.resume(coroutine.create(f)); if not ok then error(err) end end; local ok,err=coroutine.resume(coroutine.create(f)); print(ok,type(err),n)";
        let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).unwrap()).unwrap();
        let mut runtime = LuaRuntime::with_limits(10_000_000, 12);
        runtime.run(&program).unwrap();
        let expected = runtime.take_output();
        let mut session = LuaDebugSession::launch(source, "main.lua").unwrap();
        session.runtime.max_call_depth = 12;
        while session.continue_burst(1000) == DebugStatus::Paused {}
        assert_eq!(session.status, DebugStatus::Returned);
        assert_eq!(session.take_output(), expected);
    }
}
