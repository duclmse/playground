//! Typed Sol module graph loading and namespace lowering (M11).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{self, AssignTarget, Expr, ExprKind, Stmt, TypeName};
use crate::parser::LanguageConfig;
use crate::types::{TProgram, Type};

struct LoadedModule {
    name: String,
    path: PathBuf,
    program: ast::Program,
    dynamic: bool,
}

struct Loader {
    visiting: Vec<PathBuf>,
    loaded: HashMap<PathBuf, LoadedModule>,
    order: Vec<PathBuf>,
}

pub fn compile_project(path: &str, source: &[u8]) -> Result<(TProgram, Type), String> {
    let project = load_project_program(path, source)?;
    crate::compile_program(project.program)
}

/// Canonicalized source-level module graph used by mixed-tier execution.
pub struct ProjectProgram {
    pub program: ast::Program,
    pub has_dynamic_modules: bool,
    /// Explicitly annotated exports from dynamic modules. Mixed execution
    /// uses these names to build the same namespace table in `package.loaded`
    /// that typed `import` calls through.
    pub dynamic_contracts: Vec<DynamicModuleContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicModuleContract {
    pub name: String,
    pub exports: Vec<String>,
}

pub fn load_project_program(path: &str, source: &[u8]) -> Result<ProjectProgram, String> {
    let root = PathBuf::from(path);
    let root_key = root.canonicalize().unwrap_or_else(|_| root.clone());
    let mut root_program = parse_source(&root, source)?;
    let mut loader = Loader {
        visiting: vec![root_key],
        loaded: HashMap::new(),
        order: Vec::new(),
    };
    let root_dir = root.parent().unwrap_or_else(|| Path::new("."));
    for import in &root_program.imports {
        loader.load(&import.module, root_dir, &root, import.line)?;
    }
    loader.visiting.pop();

    let mut by_name = HashMap::new();
    for module in loader.loaded.values() {
        if by_name.insert(module.name.clone(), module).is_some() {
            return Err(format!("duplicate module name '{}'", module.name));
        }
    }
    validate_accesses(&root_program, None, &root, &by_name)?;
    for module in loader.loaded.values() {
        validate_accesses(&module.program, Some(&module.name), &module.path, &by_name)?;
    }

    let mut aliases = Vec::new();
    let mut structs = Vec::new();
    let mut functions = Vec::new();
    let mut externs = Vec::new();
    let mut initializers = Vec::new();
    for key in &loader.order {
        let module = loader
            .loaded
            .get(key)
            .expect("module order references loaded module");
        let mut program = module.program.clone();
        if program.initializer {
            initializers.push(format!("{}.main", module.name));
        }
        namespace_program(&mut program, &module.name, &module.path);
        aliases.extend(program.aliases);
        structs.extend(program.structs);
        functions.extend(program.functions);
        externs.extend(program.externs);
    }
    root_program.imports.clear();
    root_program.exports.clear();
    if let Some(main) = root_program
        .functions
        .iter_mut()
        .find(|function| function.name == "main")
    {
        let mut calls = initializers
            .into_iter()
            .map(|name| {
                Stmt::Expr(Expr {
                    kind: ExprKind::Call(name, Vec::new()),
                    line: main.line,
                })
            })
            .collect::<Vec<_>>();
        calls.append(&mut main.body);
        main.body = calls;
    }
    aliases.extend(root_program.aliases);
    structs.extend(root_program.structs);
    functions.extend(root_program.functions);
    externs.extend(root_program.externs);
    let has_dynamic_modules = loader.loaded.values().any(|module| module.dynamic);
    let dynamic_contracts = loader
        .order
        .iter()
        .filter_map(|key| {
            let module = &loader.loaded[key];
            module.dynamic.then(|| DynamicModuleContract {
                name: module.name.clone(),
                exports: module.program.exports.clone(),
            })
        })
        .collect();
    Ok(ProjectProgram {
        program: ast::Program {
            imports: Vec::new(),
            exports: Vec::new(),
            initializer: false,
            aliases,
            structs,
            functions,
            externs,
        },
        has_dynamic_modules,
        dynamic_contracts,
    })
}

impl Loader {
    fn load(
        &mut self,
        name: &str,
        relative_to: &Path,
        importer: &Path,
        import_line: u32,
    ) -> Result<(), String> {
        let path = resolve(name, relative_to).ok_or_else(|| {
            let relative = name.replace('.', "/");
            format!(
                "{}:{import_line}: module '{name}' not found; tried '{}' then '{}'",
                importer.display(),
                relative_to.join(format!("{relative}.sol")).display(),
                relative_to.join(format!("{relative}.lua")).display()
            )
        })?;
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Some(start) = self.visiting.iter().position(|entry| entry == &key) {
            let mut cycle = self.visiting[start..]
                .iter()
                .map(|entry| entry.display().to_string())
                .collect::<Vec<_>>();
            cycle.push(key.display().to_string());
            return Err(format!(
                "{}:{import_line}: import cycle: {}",
                importer.display(),
                cycle.join(" -> ")
            ));
        }
        if let Some(existing) = self.loaded.get(&key) {
            if existing.name != name {
                return Err(format!(
                    "{}:{import_line}: module '{}' was already loaded as '{}'",
                    importer.display(),
                    name,
                    existing.name
                ));
            }
            return Ok(());
        }
        let source = fs::read(&path).map_err(|error| {
            format!(
                "{}:{import_line}: failed to read module '{name}': {error}",
                importer.display()
            )
        })?;
        let dynamic = path.extension().and_then(|extension| extension.to_str()) == Some("lua");
        let mut program = parse_file(&path, &source).map_err(|error| {
            format!(
                "{error}\nimported from {}:{import_line}",
                importer.display()
            )
        })?;
        if dynamic {
            // Dynamic bodies stay dynamic. Only a fully explicit annotation
            // surface becomes an importable contract, so the compiler never
            // guesses a typed interface from ordinary Lua implementation.
            program.exports = program
                .functions
                .iter()
                .filter(|function| {
                    function.return_type.is_some()
                        && function
                            .param_annotations
                            .iter()
                            .all(|annotation| *annotation)
                })
                .map(|function| function.name.clone())
                .collect();
        }
        self.visiting.push(key.clone());
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        for import in &program.imports {
            self.load(&import.module, directory, &path, import.line)?;
        }
        self.visiting.pop();
        self.order.push(key.clone());
        self.loaded.insert(
            key,
            LoadedModule {
                name: name.to_string(),
                path,
                program,
                dynamic,
            },
        );
        Ok(())
    }
}

fn resolve(name: &str, relative_to: &Path) -> Option<PathBuf> {
    let relative = name.replace('.', "/");
    ["sol", "lua"]
        .into_iter()
        .map(|extension| relative_to.join(format!("{relative}.{extension}")))
        .find(|candidate| candidate.is_file())
}

fn parse_file(path: &Path, source: &[u8]) -> Result<ast::Program, String> {
    parse_source(path, source).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse_source(path: &Path, source: &[u8]) -> Result<ast::Program, String> {
    let config = if path.extension().and_then(|extension| extension.to_str()) == Some("lua") {
        LanguageConfig::LUA
    } else {
        LanguageConfig::SOL
    };
    let tokens = crate::lexer::lex_bytes(source)?;
    crate::parser::parse_with_config(tokens, config)
}

fn validate_accesses(
    program: &ast::Program,
    own_module: Option<&str>,
    path: &Path,
    modules: &HashMap<String, &LoadedModule>,
) -> Result<(), String> {
    let direct: HashSet<&str> = program
        .imports
        .iter()
        .map(|import| import.module.as_str())
        .collect();
    let mut references = Vec::new();
    for structure in &program.structs {
        for (_, ty) in &structure.fields {
            collect_type_references(ty, structure.line, &mut references);
        }
    }
    for alias in &program.aliases {
        collect_type_references(&alias.target, alias.line, &mut references);
    }
    for function in &program.functions {
        for (_, ty) in &function.params {
            collect_type_references(ty, function.line, &mut references);
        }
        if let Some(ty) = &function.return_type {
            collect_type_references(ty, function.line, &mut references);
        }
        collect_references(&function.body, &mut references);
    }
    for (reference, line) in references {
        let target = modules
            .keys()
            .filter(|name| reference.starts_with(&format!("{name}.")))
            .max_by_key(|name| name.len());
        let Some(module_name) = target else {
            continue;
        };
        if own_module == Some(module_name.as_str()) {
            continue;
        }
        if !direct.contains(module_name.as_str()) {
            return Err(format!(
                "{}:{line}: module '{module_name}' is not directly imported",
                path.display()
            ));
        }
        let member = reference[module_name.len() + 1..]
            .split('.')
            .next()
            .unwrap_or("");
        let module = modules[module_name];
        if !module.program.exports.iter().any(|export| export == member) {
            let declaration = module
                .program
                .functions
                .iter()
                .find(|function| function.name == member)
                .map(|function| {
                    format!(
                        "; private declaration at {}:{}",
                        module.path.display(),
                        function.line
                    )
                })
                .or_else(|| {
                    module
                        .program
                        .structs
                        .iter()
                        .find(|structure| structure.name == member)
                        .map(|structure| {
                            format!(
                                "; private declaration at {}:{}",
                                module.path.display(),
                                structure.line
                            )
                        })
                })
                .or_else(|| {
                    module
                        .program
                        .aliases
                        .iter()
                        .find(|alias| alias.name == member)
                        .map(|alias| {
                            format!(
                                "; private declaration at {}:{}",
                                module.path.display(),
                                alias.line
                            )
                        })
                })
                .unwrap_or_default();
            return Err(format!(
                "{}:{line}: module '{module_name}' does not export '{member}'{declaration}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn collect_type_references(ty: &TypeName, line: u32, out: &mut Vec<(String, u32)>) {
    match ty {
        TypeName::Array(inner) => collect_type_references(inner, line, out),
        TypeName::Map(key, value) => {
            collect_type_references(key, line, out);
            collect_type_references(value, line, out);
        }
        TypeName::Function {
            params,
            return_type,
        } => {
            for param in params {
                collect_type_references(param, line, out);
            }
            collect_type_references(return_type, line, out);
        }
        TypeName::Struct(name) => out.push((name.clone(), line)),
        _ => {}
    }
}

fn collect_references(block: &[Stmt], out: &mut Vec<(String, u32)>) {
    for statement in block {
        match statement {
            Stmt::Global { values, .. } => {
                values.iter().for_each(|value| collect_expr(value, out));
            }
            Stmt::MultiLocal {
                names,
                values,
                line,
            } => {
                for (_, ty, _, _) in names {
                    if let Some(ty) = ty {
                        collect_type_references(ty, *line, out);
                    }
                }
                values.iter().for_each(|value| collect_expr(value, out));
            }
            Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
                for (_, ty) in &function.params {
                    collect_type_references(ty, function.line, out);
                }
                if let Some(ty) = &function.return_type {
                    collect_type_references(ty, function.line, out);
                }
                collect_references(&function.body, out)
            }
            Stmt::Block(body) => collect_references(body, out),
            Stmt::Repeat { body, cond, .. } | Stmt::While { body, cond, .. } => {
                collect_references(body, out);
                collect_expr(cond, out);
            }
            Stmt::Expr(expr) => collect_expr(expr, out),
            Stmt::Local {
                ty, value, line, ..
            } => {
                if let Some(ty) = ty {
                    collect_type_references(ty, *line, out);
                }
                collect_expr(value, out);
            }
            Stmt::Assign { value, .. } => collect_expr(value, out),
            Stmt::MultiAssign {
                targets, values, ..
            } => {
                targets
                    .iter()
                    .for_each(|target| collect_target(target, out));
                values.iter().for_each(|value| collect_expr(value, out));
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                collect_expr(cond, out);
                collect_references(then_block, out);
                if let Some(block) = else_block {
                    collect_references(block, out);
                }
            }
            Stmt::NumericFor {
                start,
                stop,
                step,
                body,
                ..
            } => {
                collect_expr(start, out);
                collect_expr(stop, out);
                if let Some(step) = step {
                    collect_expr(step, out);
                }
                collect_references(body, out);
            }
            Stmt::GenericFor {
                iterators, body, ..
            } => {
                iterators.iter().for_each(|value| collect_expr(value, out));
                collect_references(body, out);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    collect_expr(value, out);
                }
            }
            Stmt::MultiReturn { values, .. } => {
                values.iter().for_each(|value| collect_expr(value, out));
            }
            Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
        }
    }
}

fn collect_target(target: &AssignTarget, out: &mut Vec<(String, u32)>) {
    match target {
        AssignTarget::Name(_) => {}
        AssignTarget::Index(base, index) => {
            collect_expr(base, out);
            collect_expr(index, out);
        }
        AssignTarget::Field(base, _) => collect_expr(base, out),
    }
}

fn collect_expr(expr: &Expr, out: &mut Vec<(String, u32)>) {
    match &expr.kind {
        ExprKind::Call(name, args) => {
            out.push((name.clone(), expr.line));
            args.iter().for_each(|arg| collect_expr(arg, out));
        }
        ExprKind::CallExpr(callee, args) => {
            collect_expr(callee, out);
            args.iter().for_each(|arg| collect_expr(arg, out));
        }
        ExprKind::MethodCall(base, _, args) => {
            collect_expr(base, out);
            args.iter().for_each(|arg| collect_expr(arg, out));
        }
        ExprKind::Unary(_, inner)
        | ExprKind::Len(inner)
        | ExprKind::Field(inner, _)
        | ExprKind::Paren(inner) => collect_expr(inner, out),
        ExprKind::TypeTest(inner, ty) | ExprKind::Cast(inner, ty) => {
            collect_expr(inner, out);
            collect_type_references(ty, expr.line, out);
        }
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            collect_expr(left, out);
            collect_expr(right, out);
        }
        ExprKind::Table(fields) => fields.iter().for_each(|field| match field {
            ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                collect_expr(value, out)
            }
            ast::TableField::Key(key, value) => {
                collect_expr(key, out);
                collect_expr(value, out);
            }
        }),
        ExprKind::Function(function) => collect_references(&function.body, out),
        ExprKind::StructLiteral(name, fields) => {
            out.push((name.clone(), expr.line));
            fields
                .iter()
                .for_each(|(_, value)| collect_expr(value, out))
        }
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => {}
    }
}

fn namespace_program(program: &mut ast::Program, namespace: &str, path: &Path) {
    let local_functions: HashSet<String> = program
        .functions
        .iter()
        .map(|function| function.name.clone())
        .collect();
    let local_structs: HashSet<String> = program
        .structs
        .iter()
        .map(|structure| structure.name.clone())
        .collect();
    let local_aliases: HashSet<String> = program
        .aliases
        .iter()
        .map(|alias| alias.name.clone())
        .collect();
    let local_types: HashSet<String> = local_structs.union(&local_aliases).cloned().collect();
    for alias in &mut program.aliases {
        alias.name = format!("{namespace}.{}", alias.name);
        namespace_type(&mut alias.target, namespace, &local_types);
    }
    for structure in &mut program.structs {
        structure.name = format!("{namespace}.{}", structure.name);
        for (_, ty) in &mut structure.fields {
            namespace_type(ty, namespace, &local_types);
        }
    }
    for function in &mut program.functions {
        function.source_file = Some(path.display().to_string());
        function.name = format!("{namespace}.{}", function.name);
        for (_, ty) in &mut function.params {
            namespace_type(ty, namespace, &local_types);
        }
        if let Some(ty) = &mut function.return_type {
            namespace_type(ty, namespace, &local_types);
        }
        namespace_block(
            &mut function.body,
            namespace,
            &local_functions,
            &local_types,
        );
    }
    program.imports.clear();
    program.exports.clear();
}

fn namespace_type(ty: &mut TypeName, namespace: &str, local_structs: &HashSet<String>) {
    match ty {
        TypeName::Array(inner) => namespace_type(inner, namespace, local_structs),
        TypeName::Map(key, value) => {
            namespace_type(key, namespace, local_structs);
            namespace_type(value, namespace, local_structs);
        }
        TypeName::Function {
            params,
            return_type,
        } => {
            params
                .iter_mut()
                .for_each(|param| namespace_type(param, namespace, local_structs));
            namespace_type(return_type, namespace, local_structs);
        }
        TypeName::Struct(name) if local_structs.contains(name) => {
            *name = format!("{namespace}.{name}");
        }
        _ => {}
    }
}

fn namespace_block(
    block: &mut [Stmt],
    namespace: &str,
    local_functions: &HashSet<String>,
    local_structs: &HashSet<String>,
) {
    for statement in block {
        match statement {
            Stmt::Global { values, .. } => values
                .iter_mut()
                .for_each(|value| namespace_expr(value, namespace, local_functions, local_structs)),
            Stmt::MultiLocal { names, values, .. } => {
                for (_, ty, _, _) in names {
                    if let Some(ty) = ty {
                        namespace_type(ty, namespace, local_structs);
                    }
                }
                values.iter_mut().for_each(|value| {
                    namespace_expr(value, namespace, local_functions, local_structs)
                });
            }
            Stmt::GlobalFunction(function) | Stmt::LocalFunction(function) => {
                for (_, ty) in &mut function.params {
                    namespace_type(ty, namespace, local_structs);
                }
                if let Some(ty) = &mut function.return_type {
                    namespace_type(ty, namespace, local_structs);
                }
                namespace_block(
                    &mut function.body,
                    namespace,
                    local_functions,
                    local_structs,
                );
            }
            Stmt::Block(body) => namespace_block(body, namespace, local_functions, local_structs),
            Stmt::Repeat { body, cond, .. } | Stmt::While { body, cond, .. } => {
                namespace_block(body, namespace, local_functions, local_structs);
                namespace_expr(cond, namespace, local_functions, local_structs);
            }
            Stmt::Expr(expr) => namespace_expr(expr, namespace, local_functions, local_structs),
            Stmt::Local { ty, value, .. } => {
                if let Some(ty) = ty {
                    namespace_type(ty, namespace, local_structs);
                }
                namespace_expr(value, namespace, local_functions, local_structs);
            }
            Stmt::Assign { target, value, .. } => {
                namespace_target(target, namespace, local_functions, local_structs);
                namespace_expr(value, namespace, local_functions, local_structs);
            }
            Stmt::MultiAssign {
                targets, values, ..
            } => {
                targets.iter_mut().for_each(|target| {
                    namespace_target(target, namespace, local_functions, local_structs)
                });
                values.iter_mut().for_each(|value| {
                    namespace_expr(value, namespace, local_functions, local_structs)
                });
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                namespace_expr(cond, namespace, local_functions, local_structs);
                namespace_block(then_block, namespace, local_functions, local_structs);
                if let Some(block) = else_block {
                    namespace_block(block, namespace, local_functions, local_structs);
                }
            }
            Stmt::NumericFor {
                start,
                stop,
                step,
                body,
                ..
            } => {
                namespace_expr(start, namespace, local_functions, local_structs);
                namespace_expr(stop, namespace, local_functions, local_structs);
                if let Some(step) = step {
                    namespace_expr(step, namespace, local_functions, local_structs);
                }
                namespace_block(body, namespace, local_functions, local_structs);
            }
            Stmt::GenericFor {
                iterators, body, ..
            } => {
                iterators.iter_mut().for_each(|value| {
                    namespace_expr(value, namespace, local_functions, local_structs)
                });
                namespace_block(body, namespace, local_functions, local_structs);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    namespace_expr(value, namespace, local_functions, local_structs);
                }
            }
            Stmt::MultiReturn { values, .. } => values
                .iter_mut()
                .for_each(|value| namespace_expr(value, namespace, local_functions, local_structs)),
            Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
        }
    }
}

fn namespace_target(
    target: &mut AssignTarget,
    namespace: &str,
    local_functions: &HashSet<String>,
    local_structs: &HashSet<String>,
) {
    match target {
        AssignTarget::Name(_) => {}
        AssignTarget::Index(base, index) => {
            namespace_expr(base, namespace, local_functions, local_structs);
            namespace_expr(index, namespace, local_functions, local_structs);
        }
        AssignTarget::Field(base, _) => {
            namespace_expr(base, namespace, local_functions, local_structs)
        }
    }
}

fn namespace_expr(
    expr: &mut Expr,
    namespace: &str,
    local_functions: &HashSet<String>,
    local_structs: &HashSet<String>,
) {
    match &mut expr.kind {
        ExprKind::Call(name, args) => {
            if local_functions.contains(name) {
                *name = format!("{namespace}.{name}");
            }
            args.iter_mut()
                .for_each(|arg| namespace_expr(arg, namespace, local_functions, local_structs));
        }
        ExprKind::CallExpr(callee, args) => {
            namespace_expr(callee, namespace, local_functions, local_structs);
            args.iter_mut()
                .for_each(|arg| namespace_expr(arg, namespace, local_functions, local_structs));
        }
        ExprKind::MethodCall(base, _, args) => {
            namespace_expr(base, namespace, local_functions, local_structs);
            args.iter_mut()
                .for_each(|arg| namespace_expr(arg, namespace, local_functions, local_structs));
        }
        ExprKind::Unary(_, inner)
        | ExprKind::Len(inner)
        | ExprKind::Field(inner, _)
        | ExprKind::Paren(inner) => {
            namespace_expr(inner, namespace, local_functions, local_structs)
        }
        ExprKind::TypeTest(inner, ty) | ExprKind::Cast(inner, ty) => {
            namespace_expr(inner, namespace, local_functions, local_structs);
            namespace_type(ty, namespace, local_structs);
        }
        ExprKind::Binary(_, left, right) | ExprKind::Index(left, right) => {
            namespace_expr(left, namespace, local_functions, local_structs);
            namespace_expr(right, namespace, local_functions, local_structs);
        }
        ExprKind::Table(fields) => fields.iter_mut().for_each(|field| match field {
            ast::TableField::Value(value) | ast::TableField::Named(_, value) => {
                namespace_expr(value, namespace, local_functions, local_structs)
            }
            ast::TableField::Key(key, value) => {
                namespace_expr(key, namespace, local_functions, local_structs);
                namespace_expr(value, namespace, local_functions, local_structs);
            }
        }),
        ExprKind::Function(function) => namespace_block(
            &mut function.body,
            namespace,
            local_functions,
            local_structs,
        ),
        ExprKind::StructLiteral(name, fields) => {
            if local_structs.contains(name) {
                *name = format!("{namespace}.{name}");
            }
            fields.iter_mut().for_each(|(_, value)| {
                namespace_expr(value, namespace, local_functions, local_structs)
            });
        }
        ExprKind::Vararg
        | ExprKind::StringLit(_)
        | ExprKind::NilLit
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::Name(_) => {}
    }
}
