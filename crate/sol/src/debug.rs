// M8: `sol debug <file.sol>` - a real `interp::Hooks` impl providing a
// function-call-boundary REPL debugger (breakpoints by function name,
// step, continue, backtrace). Works uniformly across every execution tier
// (interpreted, promoted-native) because `Runtime::call` wraps the hook
// around the call boundary itself, before dispatching to whichever tier
// is currently active - the debugger doesn't need to force tier-0-only.
//
// Scope: call-boundary granularity only, not per-source-line - a real,
// documented limit, not an oversight. Per-line breakpoints would need
// source line numbers threaded through the typed AST into bytecode (which
// M2/M3's optimizer passes can reorder/rewrite after type-checking), a
// substantially larger undertaking than this pass attempts. `args`/return
// values print as raw `u64` bits (no live type info at a call boundary) -
// honest, not false precision.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io::{self, BufRead, Write};

use crate::interp::Hooks;

pub struct DebugHooks {
    id_to_name: Vec<String>,
    id_to_line: Vec<Option<u32>>,
    id_to_file: Vec<Option<String>>,
    stack: RefCell<Vec<u8>>,
    stepping: Cell<bool>,
    breakpoints: RefCell<HashSet<String>>,
}

impl DebugHooks {
    /// Starts paused at the very first call (typically `main`), so there's
    /// a chance to set breakpoints before anything runs.
    pub fn new(id_to_name: Vec<String>) -> Self {
        let id_to_line = vec![None; id_to_name.len()];
        Self::with_locations(id_to_name, id_to_line)
    }

    pub fn with_locations(id_to_name: Vec<String>, id_to_line: Vec<Option<u32>>) -> Self {
        let id_to_file = vec![None; id_to_name.len()];
        Self::with_source_locations(id_to_name, id_to_file, id_to_line)
    }

    pub fn with_source_locations(
        id_to_name: Vec<String>,
        id_to_file: Vec<Option<String>>,
        id_to_line: Vec<Option<u32>>,
    ) -> Self {
        assert_eq!(id_to_name.len(), id_to_line.len());
        assert_eq!(id_to_name.len(), id_to_file.len());
        DebugHooks {
            id_to_name,
            id_to_line,
            id_to_file,
            stack: RefCell::new(Vec::new()),
            stepping: Cell::new(true),
            breakpoints: RefCell::new(HashSet::new()),
        }
    }

    fn name(&self, id: u8) -> &str {
        &self.id_to_name[id as usize]
    }

    fn repl(&self, func_id: u8, args: &[u64]) {
        print!("-> {}({args:?})", self.name(func_id));
        if let Some(line) = self.id_to_line[func_id as usize] {
            match &self.id_to_file[func_id as usize] {
                Some(file) => print!(" at {file}:{line}"),
                None => print!(" at line {line}"),
            }
        }
        println!();
        loop {
            print!("(sol-debug) ");
            io::stdout().flush().ok();
            let mut line = String::new();
            if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
                self.stepping.set(false); // stdin closed (non-interactive) - just run to completion
                return;
            }
            match line.trim() {
                "c" | "continue" => {
                    self.stepping.set(false);
                    return;
                }
                "s" | "step" => {
                    self.stepping.set(true);
                    return;
                }
                "bt" | "backtrace" => {
                    for &id in self.stack.borrow().iter().rev() {
                        match (self.id_to_file[id as usize].as_deref(), self.id_to_line[id as usize]) {
                            (Some(file), Some(line)) => println!("  {} at {file}:{line}", self.name(id)),
                            (_, Some(line)) => println!("  {} at line {line}", self.name(id)),
                            (_, None) => println!("  {}", self.name(id)),
                        }
                    }
                }
                "q" | "quit" => std::process::exit(0),
                cmd if cmd.starts_with("b ") || cmd.starts_with("break ") => {
                    let name = cmd.split_once(' ').map(|(_, rest)| rest).unwrap_or("").trim().to_string();
                    self.breakpoints.borrow_mut().insert(name.clone());
                    println!("breakpoint set: {name}");
                }
                "" => {}
                other => println!("unknown command '{other}' - try: c/continue, s/step, bt/backtrace, b <fn>/break <fn>, q/quit"),
            }
        }
    }
}

impl Hooks for DebugHooks {
    fn on_call_enter(&self, func_id: u8, args: &[u64]) {
        self.stack.borrow_mut().push(func_id);
        let hit_breakpoint = self.breakpoints.borrow().contains(self.name(func_id));
        if self.stepping.get() || hit_breakpoint {
            self.repl(func_id, args);
        }
    }

    fn on_call_exit(&self, _func_id: u8, _result: u64) {
        self.stack.borrow_mut().pop();
    }
}
