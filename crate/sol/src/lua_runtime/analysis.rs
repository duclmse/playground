//! Opt-in bytecode accounting shared by native and WASM adapters.
use super::frame::{Frame, LuaFrame};
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct FunctionStats {
    pub name: String,
    pub calls: u32,
    pub self_instructions: u32,
    pub total_instructions: u32,
}

#[derive(Clone)]
pub struct TimelineEvent {
    pub kind: String,
    pub source: String,
    pub line: Option<u32>,
    pub local0: Option<String>,
    pub duration: u32,
}

pub struct Analysis {
    pub functions: Vec<FunctionStats>,
    pub events: Vec<TimelineEvent>,
    pub truncated: bool,
    pub error: Option<String>,
}

pub(super) struct Recording {
    pub functions: BTreeMap<String, FunctionStats>,
    pub events: Vec<TimelineEvent>,
    pub active: Vec<String>,
    limit: usize,
    instructions: u32,
    last_event_instruction: u32,
    last_line: Option<(String, u32)>,
    pub truncated: bool,
}

impl Recording {
    pub fn new(limit: usize) -> Self {
        Self {
            functions: BTreeMap::new(),
            events: Vec::new(),
            active: Vec::new(),
            limit,
            instructions: 0,
            last_event_instruction: 0,
            last_line: None,
            truncated: false,
        }
    }
    pub fn call(&mut self, name: String) {
        self.functions
            .entry(name.clone())
            .or_insert_with(|| FunctionStats {
                name,
                ..Default::default()
            })
            .calls += 1;
    }
}

impl LuaRuntime {
    pub(super) fn record_event(&mut self, kind: &str, frame: &LuaFrame) {
        let Some(recording) = self.debug_recording.as_mut() else {
            return;
        };
        if recording.events.len() >= recording.limit {
            recording.truncated = true;
            return;
        }
        let duration = recording.instructions - recording.last_event_instruction;
        recording.last_event_instruction = recording.instructions;
        let source = self
            .chunk_sources
            .get(&(Rc::as_ptr(&frame.proto) as usize))
            .map(|source| {
                String::from_utf8_lossy(source)
                    .trim_start_matches('@')
                    .to_string()
            })
            .unwrap_or_default();
        let line = frame
            .proto
            .source_map
            .location(frame.header.pc)
            .map(|location| location.line);
        recording.events.push(TimelineEvent {
            kind: kind.into(),
            source,
            line,
            local0: None,
            duration,
        });
    }
    pub(super) fn analysis_name(&self, frame: &LuaFrame) -> String {
        let source = self
            .chunk_sources
            .get(&(Rc::as_ptr(&frame.proto) as usize))
            .map(|source| {
                String::from_utf8_lossy(source)
                    .trim_start_matches('@')
                    .to_string()
            })
            .unwrap_or_default();
        format!("{source}:{}", frame.proto.metadata.name)
    }
    pub(super) fn record_instruction(&mut self, frame: &LuaFrame) {
        if self.debug_recording.is_none() {
            return;
        }
        let name = self.analysis_name(frame);
        let mut ancestors = self
            .frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Lua(frame) => Some(self.analysis_name(frame)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !self.coroutine_stack.is_empty() {
            for thread in std::iter::once(self.main_coroutine).chain(
                self.coroutine_stack
                    .iter()
                    .copied()
                    .take(self.coroutine_stack.len() - 1),
            ) {
                let co = self.coroutine(thread);
                for frame in co.frames.borrow().iter() {
                    if let Frame::Lua(frame) = frame {
                        ancestors.push(self.analysis_name(frame));
                    }
                }
            }
        }
        let source = self
            .chunk_sources
            .get(&(Rc::as_ptr(&frame.proto) as usize))
            .map(|source| {
                String::from_utf8_lossy(source)
                    .trim_start_matches('@')
                    .to_string()
            })
            .unwrap_or_default();
        let line = frame
            .proto
            .source_map
            .location(frame.header.pc)
            .map(|location| location.line);
        let wants_event = self.debug_recording.as_ref().is_some_and(|recording| {
            recording.events.len() < recording.limit
                && line
                    .is_some_and(|line| recording.last_line.as_ref() != Some(&(name.clone(), line)))
        });
        let local0 = if wants_event {
            frame
                .proto
                .locals
                .iter()
                .find(|local| local.start_pc <= frame.header.pc && frame.header.pc < local.end_pc)
                .map(|local| {
                    String::from_utf8_lossy(
                        &super::util::reg_get(
                            self,
                            &frame.regs,
                            &frame.cells,
                            local.register as usize,
                        )
                        .display_bytes(),
                    )
                    .into_owned()
                })
        } else {
            None
        };
        let recording = self.debug_recording.as_mut().unwrap();
        ancestors.extend(recording.active.iter().cloned());
        recording
            .functions
            .entry(name.clone())
            .or_insert_with(|| FunctionStats {
                name: name.clone(),
                ..Default::default()
            })
            .self_instructions += 1;
        for ancestor in ancestors {
            recording
                .functions
                .entry(ancestor.clone())
                .or_insert_with(|| FunctionStats {
                    name: ancestor,
                    ..Default::default()
                })
                .total_instructions += 1;
        }
        recording.instructions += 1;
        if let Some(line) = line {
            let position = (name, line);
            if recording.last_line.as_ref() != Some(&position) {
                if recording.events.len() < recording.limit {
                    recording.events.push(TimelineEvent {
                        kind: "line".into(),
                        source,
                        line: Some(line),
                        local0,
                        duration: recording.instructions - recording.last_event_instruction,
                    });
                } else {
                    recording.truncated = true;
                }
                recording.last_event_instruction = recording.instructions;
                recording.last_line = Some(position);
            }
        }
    }
}
