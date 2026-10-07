//! Checked scalar boundaries over explicit specialized/generic continuations.
//! One generic runtime retains globals, module identity and coroutine state.
use crate::{
    interp::live::{Execution, Stop},
    lua_runtime::{
        debugger::{DebugStatus, LuaDebugSession},
        LuaValue,
    },
    tier0::Engine,
    types::{TExternFunction, TProgram, Type},
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub enum Layer {
    Typed(Execution<()>),
    Generic { base: usize, result: Option<Type> },
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(files: &[(&str, &str)], entry: &str) -> (Result<Option<LuaValue>, String>, Vec<u8>) {
        let files = files
            .iter()
            .map(|(name, source)| (name.to_string(), source.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        let (mut exec, mut lua) = MixedExecution::from_project(entry, &files).unwrap();
        for _ in 0..100_000 {
            if exec.terminal.is_some() {
                break;
            }
            exec.tick(&mut lua);
        }
        (
            exec.terminal.expect("bounded completion"),
            lua.take_output(),
        )
    }
    #[test]
    fn typed_import_calls_generic_body_with_captured_output() {
        let (result, output) = run(
            &[
                (
                    "main.sol",
                    "import helper\nfunction main():i64 return helper.add(40) end",
                ),
                (
                    "helper.lua",
                    "function add(n:i64):i64 print(n); return n+2 end",
                ),
            ],
            "main.sol",
        );
        assert_eq!(result.unwrap(), Some(LuaValue::Integer(42)));
        assert_eq!(output, b"40\n");
    }
    #[test]
    fn generic_entry_calls_specialized_functions_and_catches_boundary_errors() {
        let (result,output) = run(&[("main.sol", "function add(n:i64):i64 return n+2 end\nprint(add(40)); local ok=pcall(add,'bad'); print(ok)")], "main.sol");
        assert!(result.is_ok());
        assert_eq!(output, b"42\nfalse\n");
    }
    #[test]
    fn reentrant_generic_typed_generic_calls_preserve_runtime_state() {
        let (result,output) = run(&[("main.sol", "function generic(n:i64):i64 print(n); if n==0 then return 40 end; return typed(n-1)+1 end\nfunction typed(n:i64):i64 return generic(n)+1 end\nfunction main():i64 return typed(1) end")], "main.sol");
        assert_eq!(result.unwrap(), Some(LuaValue::Integer(43)));
        assert_eq!(output, b"1\n0\n");
    }
    #[test]
    fn bridge_result_contract_errors_propagate_without_panics() {
        let (result, _) = run(
            &[
                (
                    "main.sol",
                    "import helper\nfunction main():i64 return helper.add(40) end",
                ),
                (
                    "helper.lua",
                    "function add(n:i64):i64 print(n); return 'bad' end",
                ),
            ],
            "main.sol",
        );
        assert!(result.unwrap_err().contains("contract expected I64"));
    }
    #[test]
    fn portable_mixed_manifest_matches_the_same_native_adapter() {
        for row in include_str!("../tests/wasm-mixed.tsv")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
        {
            let fields = row.split('\t').collect::<Vec<_>>();
            let files = fields[1]
                .split(',')
                .map(|name| {
                    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/wasm-mixed")
                        .join(name);
                    (name.to_string(), std::fs::read(path).unwrap())
                })
                .collect::<Vec<_>>();
            let (mut exec, mut lua) = MixedExecution::from_project(fields[0], &files).unwrap();
            for _ in 0..100_000 {
                if exec.terminal.is_some() {
                    break;
                }
                exec.tick(&mut lua);
            }
            let result = exec
                .terminal
                .as_ref()
                .expect("bounded fixture completion")
                .as_ref()
                .unwrap();
            let mut output = lua.take_output();
            if exec.echo_return {
                if let Some(display) = &exec.root_display {
                    output.extend(display.as_bytes());
                } else if let Some(value) = result {
                    output.extend(value.display_bytes());
                }
            }
            let actual = output
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, fields[2], "{}", fields[0]);
        }
    }
    #[test]
    fn shared_budget_cannot_be_reset_by_reentrant_calls() {
        let files = vec![(
            "main.sol".into(),
            b"function typed(n:i64):i64 return n+1 end\nwhile true do typed(40) end".to_vec(),
        )];
        let (mut exec, mut lua) = MixedExecution::from_project("main.sol", &files).unwrap();
        exec.remaining = 100;
        for _ in 0..1000 {
            if exec.terminal.is_some() {
                break;
            }
            exec.tick(&mut lua);
        }
        assert!(exec
            .terminal
            .unwrap()
            .unwrap_err()
            .contains("instruction budget"));
        assert_eq!(exec.remaining, 0);
    }
}

pub struct MixedExecution {
    pub program: TProgram,
    pub engine: Engine,
    pub layers: Vec<Layer>,
    /// Owning resume-chain thread for each parked execution layer.
    pub layer_threads: Vec<u32>,
    pub names: HashMap<u8, String>,
    signatures: HashMap<u8, (Vec<Type>, Type)>,
    pub remaining: u64,
    pub terminal: Option<Result<Option<LuaValue>, String>>,
    pub echo_return: bool,
    root_type: Option<Type>,
    pub root_display: Option<String>,
}

fn scalar(ty: &Type) -> bool {
    matches!(ty, Type::I64 | Type::F64 | Type::Bool | Type::Nil)
}
fn ast_scalar(ty: &crate::ast::TypeName) -> Option<Type> {
    Some(match ty {
        crate::ast::TypeName::I64 => Type::I64,
        crate::ast::TypeName::F64 => Type::F64,
        crate::ast::TypeName::Bool => Type::Bool,
        crate::ast::TypeName::Nil => Type::Nil,
        _ => return None,
    })
}
pub fn pack(value: &LuaValue, ty: &Type) -> Result<u64, String> {
    match (value, ty) {
        (LuaValue::Integer(value), Type::I64) => Ok(*value as u64),
        (LuaValue::Integer(value), Type::F64) => Ok((*value as f64).to_bits()),
        (LuaValue::Float(value), Type::F64) => Ok(value.to_bits()),
        (LuaValue::Bool(value), Type::Bool) => Ok(*value as u64),
        (LuaValue::Nil, Type::Nil) => Ok(0),
        _ => Err(format!(
            "cross-tier contract expected {ty:?}, got {}",
            value.type_name()
        )),
    }
}
pub fn unpack(value: u64, ty: &Type) -> Result<LuaValue, String> {
    Ok(match ty {
        Type::I64 => LuaValue::Integer(value as i64),
        Type::F64 => LuaValue::Float(f64::from_bits(value)),
        Type::Bool => LuaValue::Bool(value != 0),
        Type::Nil => LuaValue::Nil,
        _ => return Err(format!("cross-tier contract does not support {ty:?}")),
    })
}

impl MixedExecution {
    pub fn from_project(
        entry: &str,
        files: &[(String, Vec<u8>)],
    ) -> Result<(Self, LuaDebugSession), String> {
        let project = crate::modules::load_project_program_from_sources(entry, files)?;
        let mut ast = project.program;
        for function in &mut ast.functions {
            if project.initializer_names.contains(&function.name) {
                function.return_type = Some(crate::ast::TypeName::Nil);
            }
        }
        crate::aliases::expand(&mut ast)?;
        // Lower specialized local functions without attempting to lambda-lift
        // generic coroutine bodies. Only the shared type checker's explicit
        // dynamic-body classification may accept an unlowered function.
        let mut lowered = Vec::new();
        let mut lower_errors = Vec::new();
        for function in std::mem::take(&mut ast.functions) {
            let mut candidate = ast.clone();
            candidate.functions = vec![function.clone()];
            match crate::closures::lower(&mut candidate) {
                Ok(()) => lowered.extend(candidate.functions),
                Err(error) => {
                    if !crate::closures::requires_generic_anonymous_environment(&error) {
                        return Err(error);
                    }
                    lower_errors.push((function.name.clone(), error));
                    lowered.push(function);
                }
            }
        }
        ast.functions = lowered;
        let partition = crate::typeck::check_partitioned(&ast)?;
        for (name, error) in lower_errors {
            if !partition.interpreted.contains(&name) {
                return Err(error);
            }
        }
        let mut program = partition.native;
        program.functions.extend(partition.mixed);
        program.functions.sort_by(|a, b| a.name.cmp(&b.name));
        let mut adapters: HashMap<String, Rc<crate::interp::SemanticCallable>> = HashMap::new();
        for function in &ast.functions {
            if !partition.interpreted.contains(&function.name) {
                continue;
            }
            let called = program
                .functions
                .iter()
                .any(|f| crate::typeck::called_functions(f).contains(&function.name));
            if !called {
                continue;
            }
            let params = function
                .params
                .iter()
                .map(|(_, ty)| ast_scalar(ty))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    format!(
                        "generic function '{}' needs explicit scalar parameter contracts",
                        function.name
                    )
                })?;
            let result = if project.initializer_names.contains(&function.name) {
                Type::Nil
            } else {
                function
                    .return_type
                    .as_ref()
                    .and_then(ast_scalar)
                    .ok_or_else(|| {
                        format!(
                            "generic function '{}' needs an explicit scalar result contract",
                            function.name
                        )
                    })?
            };
            if function.vararg && !project.initializer_names.contains(&function.name) {
                return Err(format!(
                    "generic function '{}' has a variadic boundary",
                    function.name
                ));
            }
            program.externs.push(TExternFunction {
                name: function.name.clone(),
                params,
                return_type: result,
            });
            adapters.insert(
                function.name.clone(),
                Rc::new(|_| {
                    sol_core::CallOutcome::Raised(
                        "semantic call requires the live scheduler".into(),
                    )
                }),
            );
        }
        crate::verify::verify(&program)?;
        let engine = Engine::new_with_adapters(program.clone(), (), 10_000_000, adapters)?;
        let mut names = HashMap::new();
        let mut signatures = HashMap::new();
        for function in &program.functions {
            let id = engine.function_id(&function.name).unwrap();
            names.insert(id, function.name.clone());
            signatures.insert(
                id,
                (
                    function.params.iter().map(|(_, ty)| ty.clone()).collect(),
                    function.return_type.clone(),
                ),
            );
        }
        for function in &program.externs {
            let id = engine.function_id(&function.name).unwrap();
            names.insert(id, function.name.clone());
            signatures.insert(id, (function.params.clone(), function.return_type.clone()));
        }
        let bindings = program
            .functions
            .iter()
            .map(|f| (f.name.clone(), engine.function_id(&f.name).unwrap()))
            .collect::<Vec<_>>();
        let mut lua =
            LuaDebugSession::bridge_session(&ast, &bindings, &project.namespace_contracts, entry)
                .map_err(|e| e.to_string())?;
        let mut registered = HashSet::new();
        for (name, source) in files {
            if name == entry {
                continue;
            }
            let (stem, config) = if let Some(stem) = name.strip_suffix(".lua") {
                (stem, crate::parser::LanguageConfig::LUA)
            } else if let Some(stem) = name.strip_suffix(".sol") {
                (stem, crate::parser::LanguageConfig::SOL)
            } else {
                continue;
            };
            let module = stem.replace('/', ".");
            let normalized = crate::modules::virtual_path(name)?.display().to_string();
            if project.loaded_paths.contains(&normalized) {
                continue;
            }
            if !registered.insert(module.clone()) {
                return Err(format!("ambiguous generic module path '{name}'"));
            }
            let parsed =
                crate::parser::parse_with_config(crate::lexer::lex_bytes(source)?, config)?;
            let bytes = if crate::semantics::requires_specialized_execution(&parsed) {
                // Do not execute or reject an unused typed module. Dynamic
                // loading of a typed export requires its canonical import
                // graph, not a second generic instance of that source.
                b"error('typed modules must be imported before require')".as_slice()
            } else {
                source.as_slice()
            };
            lua.add_generic_named_module(module.as_bytes(), name, bytes, config)
                .map_err(|e| e.to_string())?;
        }
        let echo_return = ast
            .functions
            .iter()
            .find(|f| f.name == "main")
            .is_some_and(|f| f.return_type.is_some());
        let root_type = ast
            .functions
            .iter()
            .find(|f| f.name == "main")
            .and_then(|f| f.return_type.as_ref())
            .and_then(|ty| {
                if matches!(ty, crate::ast::TypeName::String) {
                    Some(Type::String)
                } else {
                    ast_scalar(ty)
                }
            });
        let layers = if engine.function_id("main").is_some() {
            vec![Layer::Typed(engine.start_live("main", &[])?)]
        } else {
            lua.begin_bridge_call("main", Vec::new())
                .map_err(|e| e.to_string())?;
            vec![Layer::Generic {
                base: 0,
                result: None,
            }]
        };
        Ok((
            Self {
                program,
                engine,
                layers,
                layer_threads: vec![0],
                names,
                signatures,
                remaining: 10_000_000,
                terminal: None,
                echo_return,
                root_type,
                root_display: None,
            },
            lua,
        ))
    }
    pub fn typed_depth(&self) -> usize {
        self.layers
            .iter()
            .map(|layer| match layer {
                Layer::Typed(exec) => exec.frames().len(),
                _ => 0,
            })
            .sum()
    }
    pub fn active_typed(&self) -> bool {
        matches!(self.layers.last(), Some(Layer::Typed(_)))
    }
    pub fn position(&self, lua: &LuaDebugSession) -> Option<(String, String, u32, usize)> {
        let depth = self.typed_depth() + lua.total_frame_depth();
        match self.layers.last()? {
            Layer::Typed(exec) => {
                let f = exec.frames().last()?;
                Some((
                    f.bytecode.metadata.name.clone(),
                    f.bytecode.source_file.clone().unwrap_or_default(),
                    f.bytecode
                        .source_map
                        .location(f.instruction_pc())
                        .map(|p| p.line)
                        .unwrap_or(f.bytecode.source_line),
                    depth,
                ))
            }
            Layer::Generic { .. } => {
                let (source, line, _) = lua.position()?;
                let name = lua.frames().first()?.0.clone();
                Some((name, source, line, depth))
            }
        }
    }
    /// One budgeted dispatch operation, or a constant-space layer transition.
    pub fn tick(&mut self, lua: &mut LuaDebugSession) {
        if self.terminal.is_some() {
            return;
        }
        if self.layers.len() >= 1000 {
            self.terminal = Some(Err("cross-tier call depth exceeded".into()));
            return;
        }
        let typed_depth = self.typed_depth();
        lua.configure_bridge_limits(self.remaining, 1000usize.saturating_sub(typed_depth));
        if self.active_typed() {
            let Layer::Typed(exec) = self.layers.last_mut().unwrap() else {
                unreachable!()
            };
            if let Some(request) = exec.semantic_call() {
                let id = request.function;
                let arguments = request.arguments().to_vec();
                let call = (|| {
                    let (params, result) = self
                        .signatures
                        .get(&id)
                        .ok_or("unknown semantic signature")?;
                    if params.len() != arguments.len() {
                        return Err("cross-tier argument count mismatch".into());
                    }
                    let args = params
                        .iter()
                        .zip(arguments)
                        .map(|(ty, raw)| unpack(raw, ty))
                        .collect::<Result<Vec<_>, String>>()?;
                    if !scalar(result) {
                        return Err("cross-tier result requires a scalar contract".into());
                    }
                    let base = lua.frames().len();
                    lua.begin_bridge_call(&self.names[&id], args)
                        .map_err(|e| e.to_string())?;
                    Ok(Layer::Generic {
                        base,
                        result: Some(result.clone()),
                    })
                })();
                match call {
                    Ok(layer) => {
                        self.layer_threads.push(lua.active_thread());
                        self.layers.push(layer);
                    }
                    Err(error) => {
                        exec.finish_semantic_call(Err(error)).unwrap();
                    }
                }
                return;
            }
            let own_depth = exec.frames().len();
            exec.configure_bridge_limits(
                self.remaining,
                1000usize.saturating_sub(lua.bridge_depth() + typed_depth - own_depth),
            );
            let stop = exec.resume(1);
            self.remaining = self.engine.remaining_budget();
            match stop {
                Stop::Paused => {}
                Stop::Returned(value) => self.complete_typed(lua, Ok(value)),
                Stop::Raised(error) => self.complete_typed(lua, Err(error)),
            }
        } else {
            if let Some((id, args)) = lua.semantic_request() {
                if typed_depth + lua.bridge_depth() >= 1000 {
                    lua.finish_semantic_request(Err("cross-tier call depth exceeded".into()));
                    return;
                }
                let id = *id;
                let args = args.clone();
                let call = (|| {
                    let (params, result) = self
                        .signatures
                        .get(&id)
                        .ok_or("unknown specialized signature")?;
                    if !scalar(result) {
                        return Err("cross-tier result requires a scalar contract".into());
                    }
                    if params.len() != args.len() {
                        return Err("cross-tier argument count mismatch".into());
                    }
                    let raw = params
                        .iter()
                        .zip(&args)
                        .map(|(ty, value)| pack(value, ty))
                        .collect::<Result<Vec<_>, String>>()?;
                    self.engine.start_live(&self.names[&id], &raw)
                })();
                match call {
                    Ok(exec) => {
                        self.layer_threads.push(lua.active_thread());
                        self.layers.push(Layer::Typed(exec));
                    }
                    Err(error) => lua.finish_semantic_request(Err(error)),
                }
                return;
            }
            match lua.continue_burst(1) {
                DebugStatus::Paused => {
                    self.remaining = lua.bridge_budget();
                }
                _ => {
                    self.remaining = lua.bridge_budget();
                    self.layer_threads.pop();
                    let Layer::Generic { result, .. } = self.layers.pop().unwrap() else {
                        unreachable!()
                    };
                    let returned = lua.end_bridge_call();
                    if let Some(result) = result {
                        let raw = returned.and_then(|values| {
                            if result == Type::Nil {
                                Ok(0)
                            } else {
                                pack(values.first().unwrap_or(&LuaValue::Nil), &result)
                            }
                        });
                        let Some(Layer::Typed(exec)) = self.layers.last_mut() else {
                            unreachable!()
                        };
                        exec.finish_semantic_call(raw).unwrap();
                    } else {
                        self.finish_root(returned.map(|values| values.into_iter().next()));
                    }
                }
            }
        }
    }
    fn complete_typed(&mut self, lua: &mut LuaDebugSession, result: Result<u64, String>) {
        self.layers.pop();
        self.layer_threads.pop();
        if self.layers.is_empty() {
            let ty = self
                .program
                .functions
                .iter()
                .find(|f| f.name == "main")
                .map(|f| &f.return_type)
                .unwrap();
            let value = result
                .and_then(|raw| {
                    if *ty == Type::String && raw != 0 {
                        Ok(lua.bridge_string_result(unsafe {
                            crate::strings::bytes(raw as *const u8)
                        }))
                    } else if *ty == Type::String {
                        Ok(LuaValue::Nil)
                    } else {
                        unpack(raw, ty)
                    }
                })
                .map(Some);
            self.finish_root(value);
        } else {
            let id = lua
                .semantic_request()
                .expect("generic caller owns request")
                .0;
            let ty = &self.signatures[&id].1;
            lua.finish_semantic_request(
                result
                    .and_then(|raw| unpack(raw, ty))
                    .map(|value| vec![value]),
            );
        }
    }
    fn finish_root(&mut self, result: Result<Option<LuaValue>, String>) {
        let result = result.and_then(|value| {
            if let Some(ty) = &self.root_type {
                let actual = value.as_ref().unwrap_or(&LuaValue::Nil);
                if *ty == Type::String {
                    if !matches!(actual, LuaValue::String(_) | LuaValue::Nil) {
                        return Err("main result contract expected string".into());
                    }
                    self.root_display =
                        Some(String::from_utf8_lossy(&actual.display_bytes()).into_owned());
                } else {
                    let raw = pack(actual, ty)?;
                    self.root_display = Some(match ty {
                        Type::I64 => (raw as i64).to_string(),
                        Type::F64 => f64::from_bits(raw).to_string(),
                        Type::Bool => (raw != 0).to_string(),
                        Type::Nil => "nil".into(),
                        _ => unreachable!(),
                    });
                }
            }
            Ok(value)
        });
        self.terminal = Some(result);
    }
}
