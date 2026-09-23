use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vm::meta_ops::{self, MetaResult};
use vm::{
    raw_ops, BoxSequence, Callback, CallbackReturn, Closure, Context, Error, Execution, Executor,
    Fuel, IntoValue, Lua, Sequence, SequencePoll, Stack, StashedExecutor, Table, Value,
};
use wasm_bindgen::prelude::*;

mod debug_events;
pub use debug_events::{
    debug_events, record_timeline, record_timeline_project, run_with_debug_events, DebugEvent,
    Timeline,
};

mod session;
pub use session::{
    Breakpoint, BurstResult, DebugSession, EvalResult, MemoryStats, StackFrame, StopEvent,
    ThreadInfo, Variable,
};

mod profiler;
pub use profiler::{profile, profile_project, FunctionStats};

/// Fuel budget per `Executor::step` slice, and total instruction budget per
/// run - mirrors debug-protocol.md's "Instruction limits / infinite-loop
/// protection" design (`MAX_INSTRUCTIONS = 10_000_000`), so a runaway
/// `while true do end` terminates instead of hanging the worker forever
/// (product-brief.md: "a runaway ... must not freeze the tab").
const FUEL_PER_STEP: i32 = 4096;
const MAX_INSTRUCTIONS: i64 = 10_000_000;

/// Drives `executor` to completion (or a syntax/runtime error), capped at
/// [`MAX_INSTRUCTIONS`] total fuel. Returns `None` on success, `Some(msg)`
/// on error or instruction-limit overrun.
fn run_to_completion(lua: &mut Lua, executor: &StashedExecutor) -> Option<String> {
    let mut consumed: i64 = 0;
    loop {
        let mut fuel = Fuel::with(FUEL_PER_STEP);
        let finished = lua.enter(|ctx| ctx.fetch(executor).step(ctx, &mut fuel));
        consumed += (FUEL_PER_STEP - fuel.remaining()) as i64;
        if finished {
            break;
        }
        if consumed >= MAX_INSTRUCTIONS {
            return Some("Execution exceeded instruction limit".to_string());
        }
    }
    match lua.try_enter(|ctx| ctx.fetch(executor).take_result::<()>(ctx)?) {
        Ok(()) => None,
        Err(err) => Some(err.to_string()),
    }
}

/// Formats a Lua number the way real Lua's `tostring`/`print` do (roughly
/// `%.14g`), which always shows a decimal point or exponent for floats -
/// e.g. `2^10` prints as `1024.0`, not `1024`. piccolo 0.3.3's own
/// `Value::Display` impl just does `write!(w, "{}", f)`, which drops the
/// trailing `.0` for integral floats (confirmed in piccolo's `value.rs`);
/// this is a host-side correction layered on top, not a piccolo patch.
fn format_lua_number(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let plain = format!("{f}");
    if plain.contains('.') || plain.contains('e') {
        plain
    } else {
        format!("{plain}.0")
    }
}

/// Renders a value the way `print`/`tostring` do for values *without* a
/// `__tostring` metamethod (i.e. after `meta_ops::tostring` has already
/// resolved any metamethod call) - applying [`format_lua_number`] for
/// floats instead of piccolo's raw `Display` impl.
fn display_value(v: Value) -> String {
    match v {
        Value::Number(f) => format_lua_number(f),
        other => other.to_string(),
    }
}

/// Strips the `"@"` chunk-name prefix `run_project`/`install_require` (see
/// risks.md §6) add for `require()`d files, so a caller-facing source id is
/// always the plain virtual-FS path a caller actually knows about - a
/// single-file chunk name never has this prefix to begin with, so this is a
/// no-op there. Shared by `session.rs` (breakpoint `source_id` matching)
/// and `profiler.rs` (function-id chunk naming).
fn strip_chunk_prefix(name: &str) -> &str {
    name.strip_prefix('@').unwrap_or(name)
}

/// Adds `table.insert`, `table.concat`, and `table.sort`, which piccolo
/// 0.3.3's `load_table` doesn't provide (it only ships `pack`/`unpack` -
/// confirmed by reading piccolo's `stdlib/table.rs`). These are pure
/// callback-level additions, consistent with architecture.md's "host opts
/// stdlib pieces in explicitly" sandbox design.
fn extend_table_library<'gc>(ctx: Context<'gc>) -> Result<(), Error<'gc>> {
    let table = match ctx.get_global("table") {
        Value::Table(t) => t,
        _ => return Ok(()),
    };

    table.set(
        ctx,
        "insert",
        Callback::from_fn(&ctx, |ctx, _, mut stack| match stack.len() {
            2 => {
                let (t, v): (Table, Value) = stack.consume(ctx)?;
                let len = t.length();
                t.set(ctx, len + 1, v)?;
                Ok(CallbackReturn::Return)
            }
            3 => {
                let (t, pos, v): (Table, i64, Value) = stack.consume(ctx)?;
                let len = t.length();
                let mut i = len + 1;
                while i > pos {
                    let prev = t.get(ctx, i - 1);
                    t.set(ctx, i, prev)?;
                    i -= 1;
                }
                t.set(ctx, pos, v)?;
                Ok(CallbackReturn::Return)
            }
            _ => Err("wrong number of arguments to 'insert'"
                .into_value(ctx)
                .into()),
        }),
    )?;

    table.set(
        ctx,
        "concat",
        Callback::from_fn(&ctx, |ctx, _, mut stack| {
            let (t, sep, i, j): (Table, Option<vm::String>, Option<i64>, Option<i64>) =
                stack.consume(ctx)?;
            let sep = sep.map(|s| s.as_bytes().to_vec()).unwrap_or_default();
            let i = i.unwrap_or(1);
            let j = j.unwrap_or_else(|| t.length());

            let mut out = Vec::new();
            let mut idx = i;
            while idx <= j {
                if idx > i {
                    out.extend_from_slice(&sep);
                }
                match t.get(ctx, idx) {
                    v @ (Value::String(_) | Value::Integer(_) | Value::Number(_)) => {
                        out.extend_from_slice(display_value(v).as_bytes());
                    }
                    v => {
                        return Err(format!(
                            "invalid value ({}) at index {idx} in table for 'concat'",
                            v.type_name()
                        )
                        .into_value(ctx)
                        .into())
                    }
                }
                idx += 1;
            }

            stack.replace(ctx, ctx.intern(&out));
            Ok(CallbackReturn::Return)
        }),
    )?;

    table.set(
        ctx,
        "sort",
        Callback::from_fn(&ctx, |ctx, _, mut stack| {
            let (t, comparator): (Table, Option<Value>) = stack.consume(ctx)?;
            if comparator.is_some() {
                // Calling back into Lua from a host-side sort would need a
                // Sequence-driven comparator; not needed by any in-scope
                // fixture, so this is left as an explicit deviation rather
                // than silently ignoring the comparator.
                return Err("table.sort with a custom comparator is not supported"
                    .into_value(ctx)
                    .into());
            }

            let len = t.length();
            let mut items: Vec<Value> = (1..=len).map(|i| t.get(ctx, i)).collect();
            items.sort_by(|a, b| {
                raw_ops::less_than(*a, *b)
                    .map(|less| {
                        if less {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        }
                    })
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for (i, v) in items.into_iter().enumerate() {
                t.set(ctx, i as i64 + 1, v)?;
            }
            Ok(CallbackReturn::Return)
        }),
    )?;

    Ok(())
}

/// Adds `xpcall`, which piccolo 0.3.3's `load_base` doesn't provide (only
/// `pcall` - confirmed by reading piccolo's `stdlib/base.rs`). Mirrors
/// piccolo's own `pcall` `Sequence` implementation, but calls the message
/// handler on error instead of just returning `false, error`.
fn install_xpcall<'gc>(ctx: Context<'gc>) {
    #[derive(gc_arena::Collect)]
    #[collect(no_drop)]
    struct XPCall<'gc> {
        handler: Value<'gc>,
        errored: bool,
    }

    impl<'gc> Sequence<'gc> for XPCall<'gc> {
        fn poll(
            &mut self,
            _ctx: Context<'gc>,
            _exec: Execution<'gc, '_>,
            mut stack: Stack<'gc, '_>,
        ) -> Result<SequencePoll<'gc>, Error<'gc>> {
            stack.push_front(Value::Boolean(!self.errored));
            Ok(SequencePoll::Return)
        }

        fn error(
            &mut self,
            ctx: Context<'gc>,
            _exec: Execution<'gc, '_>,
            error: Error<'gc>,
            mut stack: Stack<'gc, '_>,
        ) -> Result<SequencePoll<'gc>, Error<'gc>> {
            self.errored = true;
            stack.clear();
            stack.push_back(error.to_value(ctx));
            let function = meta_ops::call(ctx, self.handler)?;
            Ok(SequencePoll::Call {
                function,
                is_tail: false,
            })
        }
    }

    let xpcall = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        let (target, handler): (Value, Value) = stack.consume(ctx)?;
        let function = meta_ops::call(ctx, target)?;
        Ok(CallbackReturn::Call {
            function,
            then: Some(BoxSequence::new(
                &ctx,
                XPCall {
                    handler,
                    errored: false,
                },
            )),
        })
    });
    ctx.set_global("xpcall", xpcall).unwrap();
}

/// Registers the in-memory-buffer `print` shared by [`run_named`] and
/// [`run_project`]. Mirrors piccolo's own `stdlib::io::load_io` print, but
/// writes into `print_buf` instead of real stdout (which doesn't exist in a
/// browser/worker) and is metamethod-aware (calls `__tostring` via
/// `meta_ops::tostring`), unlike a naive `Value::to_string()` print.
fn install_print<'gc>(ctx: Context<'gc>, print_buf: Rc<RefCell<String>>) -> Result<(), Error<'gc>> {
    let print = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        #[derive(Debug, Copy, Clone, Eq, PartialEq, gc_arena::Collect)]
        #[collect(require_static)]
        enum Mode {
            Init,
            First,
            Rest,
        }

        #[derive(gc_arena::Collect)]
        #[collect(no_drop)]
        struct PrintSeq<'gc> {
            mode: Mode,
            values: Vec<Value<'gc>>,
            #[collect(require_static)]
            buf: Rc<RefCell<String>>,
        }

        impl<'gc> Sequence<'gc> for PrintSeq<'gc> {
            fn poll(
                &mut self,
                ctx: Context<'gc>,
                _exec: Execution<'gc, '_>,
                mut stack: Stack<'gc, '_>,
            ) -> Result<SequencePoll<'gc>, Error<'gc>> {
                if self.mode == Mode::Init {
                    self.mode = Mode::First;
                } else {
                    self.values.push(stack.get(0));
                }
                stack.clear();

                while let Some(value) = self.values.pop() {
                    // Numbers can never carry a metatable in piccolo
                    // (`meta_ops::tostring` only ever checks Table/
                    // UserData), so it's safe to format them ourselves
                    // here rather than through `meta_ops::tostring`,
                    // which internally stringifies via piccolo's own
                    // `Value::Display` and loses the trailing `.0` on
                    // integral floats.
                    let resolved = if let Value::Number(f) = value {
                        MetaResult::Value(ctx.intern(format_lua_number(f).as_bytes()).into())
                    } else {
                        meta_ops::tostring(ctx, value)?
                    };
                    match resolved {
                        MetaResult::Value(v) => {
                            let mut buf = self.buf.borrow_mut();
                            if self.mode == Mode::First {
                                self.mode = Mode::Rest;
                            } else {
                                buf.push('\t');
                            }
                            buf.push_str(&display_value(v));
                        }
                        MetaResult::Call(call) => {
                            stack.extend(call.args);
                            return Ok(SequencePoll::Call {
                                function: call.function,
                                is_tail: false,
                            });
                        }
                    }
                }

                self.buf.borrow_mut().push('\n');
                Ok(SequencePoll::Return)
            }
        }

        Ok(CallbackReturn::Sequence(BoxSequence::new(
            &ctx,
            PrintSeq {
                mode: Mode::Init,
                values: stack.drain(..).rev().collect(),
                buf: print_buf.clone(),
            },
        )))
    });
    ctx.set_global("print", print)?;
    Ok(())
}

/// Adds `require`, backed by an in-memory virtual filesystem instead of real
/// file I/O (product-brief.md: "Browser-only execution environment; no real
/// filesystem - a virtual FS backs `require()`"). Modules are cached in a
/// `package.loaded` table exactly like real Lua, resolved by trying
/// `"<name>.lua"` then the bare `name` against `files`. A module chunk that
/// returns nothing caches `true`, matching real Lua's `require` semantics.
/// Chunks are named `"@<path>"` per risks.md §6's source-mapping convention,
/// so error messages report the originating virtual-FS path.
///
/// Circular `require`s are not detected (an in-progress module isn't marked
/// as loading before it finishes) - out of scope for this MVP slice, since
/// no fixture or planned use case needs it; a circular `require` will
/// recurse until the instruction limit or a stack-depth error stops it.
fn install_require<'gc>(
    ctx: Context<'gc>,
    files: Rc<HashMap<String, String>>,
) -> Result<(), Error<'gc>> {
    let package = Table::new(&ctx);
    let loaded = Table::new(&ctx);
    package.set(ctx, "loaded", loaded)?;
    ctx.set_global("package", package)?;

    let require = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        let name: vm::String = stack.consume(ctx)?;
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();

        let loaded = match ctx.get_global("package") {
            Value::Table(package) => match package.get(ctx, "loaded") {
                Value::Table(loaded) => loaded,
                _ => unreachable!("package.loaded is always a table"),
            },
            _ => unreachable!("package is always a table"),
        };

        let cached = loaded.get(ctx, name.clone());
        if !matches!(cached, Value::Nil) {
            stack.replace(ctx, cached);
            return Ok(CallbackReturn::Return);
        }

        let source = files
            .get(&format!("{name}.lua"))
            .or_else(|| files.get(&name))
            .ok_or_else(|| format!("module '{name}' not found").into_value(ctx))?;

        let chunk_name = format!("@{name}");
        let closure = Closure::load(ctx, Some(chunk_name.as_str()), source.as_bytes())?;

        #[derive(gc_arena::Collect)]
        #[collect(no_drop)]
        struct RequireSeq {
            #[collect(require_static)]
            name: String,
        }

        impl<'gc> Sequence<'gc> for RequireSeq {
            fn poll(
                &mut self,
                ctx: Context<'gc>,
                _exec: Execution<'gc, '_>,
                mut stack: Stack<'gc, '_>,
            ) -> Result<SequencePoll<'gc>, Error<'gc>> {
                let result = match stack.get(0) {
                    Value::Nil => Value::Boolean(true),
                    v => v,
                };
                stack.clear();

                let loaded = match ctx.get_global("package") {
                    Value::Table(package) => match package.get(ctx, "loaded") {
                        Value::Table(loaded) => loaded,
                        _ => unreachable!("package.loaded is always a table"),
                    },
                    _ => unreachable!("package is always a table"),
                };
                loaded.set(ctx, self.name.clone(), result)?;

                stack.push_back(result);
                Ok(SequencePoll::Return)
            }
        }

        Ok(CallbackReturn::Call {
            function: closure.into(),
            then: Some(BoxSequence::new(&ctx, RequireSeq { name })),
        })
    });
    ctx.set_global("require", require)?;
    Ok(())
}

#[wasm_bindgen]
pub struct ExecuteResult {
    output: String,
    error: Option<String>,
    error_source: Option<String>,
    error_line: Option<u32>,
}

#[wasm_bindgen]
impl ExecuteResult {
    #[wasm_bindgen(getter)]
    pub fn output(&self) -> String {
        self.output.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    /// Inline-diagnostics support: which open file `error` happened in, or
    /// `None` if there was no error, or if a position couldn't be recovered
    /// at all (see `debug_events::diagnose_error_position_project`'s doc
    /// comment for the one case that can happen in). Matches the source-id
    /// convention `DebugSession::set_breakpoint`'s `source_id` already uses
    /// (the entry file's own name, or a `require()`d file's require
    /// argument) - a caller with an open multi-file project already knows
    /// how to map this back to a specific editor tab.
    #[wasm_bindgen(getter)]
    pub fn error_source(&self) -> Option<String> {
        self.error_source.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn error_line(&self) -> Option<u32> {
        self.error_line
    }
}

impl ExecuteResult {
    fn with_diagnostics(
        output: String,
        error: Option<String>,
        names: Vec<String>,
        contents: Vec<String>,
        entry: &str,
    ) -> Self {
        let position = if error.is_some() {
            debug_events::diagnose_error_position_project(names, contents, entry)
        } else {
            None
        };
        ExecuteResult {
            output,
            error,
            error_source: position.as_ref().map(|(source, _)| source.clone()),
            error_line: position.map(|(_, line)| line),
        }
    }
}

#[wasm_bindgen]
pub fn execute(source: &str) -> ExecuteResult {
    let (output, error) = run(source);
    ExecuteResult::with_diagnostics(
        output,
        error,
        vec!["input".to_string()],
        vec![source.to_string()],
        "input",
    )
}

/// Runs a multi-file project: `names[i]` is the virtual-FS path of
/// `contents[i]`, and `entry` names the file to execute as the main chunk.
/// Other files become available to `require()` (see [`install_require`]).
#[wasm_bindgen]
pub fn execute_project(names: Vec<String>, contents: Vec<String>, entry: &str) -> ExecuteResult {
    let files: Vec<(&str, &str)> = names
        .iter()
        .map(String::as_str)
        .zip(contents.iter().map(String::as_str))
        .collect();
    let (output, error) = run_project(&files, entry);
    ExecuteResult::with_diagnostics(output, error, names, contents, entry)
}

/// Runs `source` against a fresh piccolo VM with a host-provided `print` that
/// writes into an in-memory buffer instead of `io::stdout` (piccolo's own
/// `load_io` prints to real stdout, which doesn't exist in a browser/worker).
/// Registering only `load_core` + this custom `print` matches the sandbox
/// design in architecture.md: the host opts stdlib pieces in explicitly.
pub fn run(source: &str) -> (String, Option<String>) {
    run_named(source, "input")
}

/// Same as [`run`], but with an explicit chunk name (used in `error()`
/// position strings, matching Lua's `@path` chunk-naming convention - see
/// risks.md §6). The conformance harness uses this to match each fixture's
/// error messages against what the real `lua` CLI produces for that file.
pub fn run_named(source: &str, chunk_name: &str) -> (String, Option<String>) {
    let output = Rc::new(RefCell::new(String::new()));
    let mut lua = Lua::core();

    let executor = lua.try_enter(|ctx| {
        install_print(ctx, output.clone())?;
        extend_table_library(ctx)?;
        install_xpcall(ctx);

        let closure = Closure::load(ctx, Some(chunk_name), source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });

    let executor = match executor {
        Ok(executor) => executor,
        Err(err) => return (output.borrow().clone(), Some(err.to_string())),
    };

    let error = run_to_completion(&mut lua, &executor);
    let result = output.borrow().clone();
    (result, error)
}

/// Runs a multi-file project (see [`execute_project`]): `entry` is executed
/// as the main chunk, with `require()` (see [`install_require`]) resolving
/// other entries in `files` against an in-memory virtual filesystem rather
/// than real file I/O.
pub fn run_project(files: &[(&str, &str)], entry: &str) -> (String, Option<String>) {
    let output = Rc::new(RefCell::new(String::new()));
    let file_map: Rc<HashMap<String, String>> = Rc::new(
        files
            .iter()
            .map(|(name, content)| (name.to_string(), content.to_string()))
            .collect(),
    );

    let Some(entry_source) = file_map.get(entry).cloned() else {
        return (
            String::new(),
            Some(format!("entry file '{entry}' not found")),
        );
    };

    let mut lua = Lua::core();

    let executor = lua.try_enter(|ctx| {
        install_print(ctx, output.clone())?;
        extend_table_library(ctx)?;
        install_xpcall(ctx);
        install_require(ctx, file_map.clone())?;

        let chunk_name = format!("@{entry}");
        let closure = Closure::load(ctx, Some(chunk_name.as_str()), entry_source.as_bytes())?;
        let executor = Executor::start(ctx, closure.into(), ());
        Ok(ctx.stash(executor))
    });

    let executor = match executor {
        Ok(executor) => executor,
        Err(err) => return (output.borrow().clone(), Some(err.to_string())),
    };

    let error = run_to_completion(&mut lua, &executor);
    let result = output.borrow().clone();
    (result, error)
}

#[cfg(test)]
mod tests {
    use super::{execute_project, run, run_project};

    #[test]
    fn prints_hello_lua() {
        let (output, error) = run(r#"print("Hello Lua")"#);
        assert_eq!(error, None);
        assert_eq!(output, "Hello Lua\n");
    }

    #[test]
    fn captures_multiple_print_args_tab_separated() {
        let (output, error) = run(r#"print(1, "two", true, nil)"#);
        assert_eq!(error, None);
        assert_eq!(output, "1\ttwo\ttrue\tnil\n");
    }

    #[test]
    fn print_is_metamethod_aware() {
        let (output, error) = run(r#"
            local t = setmetatable({}, { __tostring = function() return "custom!" end })
            print(t)
            "#);
        assert_eq!(error, None);
        assert_eq!(output, "custom!\n");
    }

    #[test]
    fn arithmetic_and_locals() {
        let (output, error) = run("local x = 2 + 3\nprint(x * 10)");
        assert_eq!(error, None);
        assert_eq!(output, "50\n");
    }

    #[test]
    fn closures_and_loop_variable_capture() {
        let (output, error) = run(r#"
            local fns = {}
            for i = 1, 3 do
                fns[i] = function() return i end
            end
            print(fns[1](), fns[2](), fns[3]())
            "#);
        assert_eq!(error, None);
        assert_eq!(output, "1\t2\t3\n");
    }

    #[test]
    fn syntax_error_is_reported_not_panicking() {
        let (_output, error) = run("this is not lua");
        assert!(error.is_some());
    }

    #[test]
    fn runtime_error_is_reported_not_panicking() {
        let (_output, error) = run("error('boom')");
        assert!(error.is_some());
        assert!(error.unwrap().contains("boom"));
    }

    #[test]
    fn integral_floats_print_with_trailing_decimal() {
        let (output, error) = run("print(2 ^ 10)");
        assert_eq!(error, None);
        assert_eq!(output, "1024.0\n");
    }

    #[test]
    fn runaway_loop_hits_instruction_limit_instead_of_hanging() {
        let (_output, error) = run("while true do end");
        assert_eq!(
            error.as_deref(),
            Some("Execution exceeded instruction limit")
        );
    }

    #[test]
    fn require_loads_and_caches_a_module_from_the_virtual_fs() {
        let files = [
            ("main.lua", "local greet = require('greet')\nprint(greet.hello('world'))\nprint(require('greet') == greet)"),
            ("greet.lua", "local M = {}\nfunction M.hello(name) return 'hello, ' .. name end\nreturn M"),
        ];
        let (output, error) = run_project(&files, "main.lua");
        assert_eq!(error, None);
        assert_eq!(output, "hello, world\ntrue\n");
    }

    #[test]
    fn require_of_missing_module_is_a_lua_error() {
        let files = [("main.lua", "require('nope')")];
        let (_output, error) = run_project(&files, "main.lua");
        assert!(error.unwrap().contains("nope"));
    }

    #[test]
    fn require_of_module_with_no_return_value_caches_true() {
        let files = [
            ("main.lua", "print(require('sideeffect'))"),
            ("sideeffect.lua", "print('loaded')"),
        ];
        let (output, error) = run_project(&files, "main.lua");
        assert_eq!(error, None);
        assert_eq!(output, "loaded\ntrue\n");
    }

    #[test]
    fn execute_project_populates_error_position_for_a_runtime_error() {
        let names = vec!["main.lua".to_string()];
        let contents = vec!["print('before')\nerror('boom')".to_string()];
        let result = execute_project(names, contents, "main.lua");
        assert!(result.error().unwrap().contains("boom"));
        assert_eq!(result.error_source(), Some("main.lua".to_string()));
        assert_eq!(result.error_line(), Some(2));
        assert_eq!(result.output(), "before\n");
    }

    #[test]
    fn execute_project_leaves_error_position_none_on_success() {
        let names = vec!["main.lua".to_string()];
        let contents = vec!["print('ok')".to_string()];
        let result = execute_project(names, contents, "main.lua");
        assert_eq!(result.error(), None);
        assert_eq!(result.error_source(), None);
        assert_eq!(result.error_line(), None);
    }
}
