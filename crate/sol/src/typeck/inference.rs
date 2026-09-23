//! Sound, non-rejecting inference for the unified Lua/Sol runtime.
//!
//! The typed lowering in the parent module enforces explicit contracts. This
//! module analyzes the larger annotation-free language: facts may remove a
//! check, but loss of a fact always widens back to `dynamic` rather than
//! rejecting a Lua program.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::ast::{self, AssignTarget, BinaryOp, Expr, ExprKind, Stmt, TableField, TypeName};
use crate::parser::TypePolicy;

/// Keep unions deliberately small so loop/data-flow convergence is bounded.
pub const MAX_UNION_MEMBERS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeAtom {
    Nil,
    Bool,
    Integer,
    Float,
    String,
    Table(u32),
    Function(u32),
}

impl fmt::Display for TypeAtom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("nil"),
            Self::Bool => f.write_str("boolean"),
            Self::Integer => f.write_str("integer"),
            Self::Float => f.write_str("float"),
            Self::String => f.write_str("string"),
            Self::Table(id) => write!(f, "table#{id}"),
            Self::Function(id) => write!(f, "function#{id}"),
        }
    }
}

/// `Bottom` is unreachable control flow, `Union` is a proven finite set, and
/// `Dynamic` is the safe top type. There is intentionally no speculative fact
/// in this lattice: profiling belongs to a separate optimization tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowType {
    Bottom,
    Union(BTreeSet<TypeAtom>),
    Dynamic,
}

impl FlowType {
    pub fn atom(atom: TypeAtom) -> Self {
        Self::Union(BTreeSet::from([atom]))
    }

    pub fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Bottom, value) | (value, Self::Bottom) => value.clone(),
            (Self::Dynamic, _) | (_, Self::Dynamic) => Self::Dynamic,
            (Self::Union(left), Self::Union(right)) => {
                let members = left.union(right).cloned().collect::<BTreeSet<_>>();
                if members.len() > MAX_UNION_MEMBERS {
                    Self::Dynamic
                } else {
                    Self::Union(members)
                }
            }
        }
    }

    pub fn without_nil(&self) -> Self {
        match self {
            Self::Union(members) => {
                let members = members
                    .iter()
                    .filter(|member| **member != TypeAtom::Nil)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if members.is_empty() {
                    Self::Bottom
                } else {
                    Self::Union(members)
                }
            }
            value => value.clone(),
        }
    }

    pub fn narrow_to(&self, atom: TypeAtom) -> Self {
        match self {
            Self::Bottom => Self::Bottom,
            Self::Dynamic => Self::atom(atom),
            Self::Union(members) if members.contains(&atom) => Self::atom(atom),
            Self::Union(_) => Self::Bottom,
        }
    }

    pub fn remove(&self, atom: &TypeAtom) -> Self {
        match self {
            Self::Union(members) => {
                let mut result = members.clone();
                result.remove(atom);
                if result.is_empty() {
                    Self::Bottom
                } else {
                    Self::Union(result)
                }
            }
            value => value.clone(),
        }
    }

    pub fn is_single(&self, expected: &TypeAtom) -> bool {
        matches!(self, Self::Union(members) if members.len() == 1 && members.contains(expected))
    }

    fn from_annotation(annotation: &TypeName) -> Self {
        match annotation {
            TypeName::I64 => Self::atom(TypeAtom::Integer),
            TypeName::F64 => Self::atom(TypeAtom::Float),
            TypeName::Bool => Self::atom(TypeAtom::Bool),
            TypeName::Nil => Self::atom(TypeAtom::Nil),
            TypeName::String => Self::atom(TypeAtom::String),
            TypeName::Function { .. } => Self::Dynamic,
            TypeName::Array(_) | TypeName::Map(_, _) | TypeName::Struct(_) | TypeName::Any => {
                Self::Dynamic
            }
        }
    }
}

impl fmt::Display for FlowType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bottom => f.write_str("never"),
            Self::Dynamic => f.write_str("dynamic"),
            Self::Union(members) => {
                for (index, member) in members.iter().enumerate() {
                    if index != 0 {
                        f.write_str("|")?;
                    }
                    write!(f, "{member}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactStrength {
    Proven,
    Guarded,
    Observed,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Effects(u16);

impl Effects {
    pub const READ_GLOBAL: Self = Self(1 << 0);
    pub const WRITE_GLOBAL: Self = Self(1 << 1);
    pub const READ_TABLE: Self = Self(1 << 2);
    pub const WRITE_TABLE: Self = Self(1 << 3);
    pub const ALLOCATE: Self = Self(1 << 4);
    pub const CALL: Self = Self(1 << 5);
    pub const YIELD: Self = Self(1 << 6);
    pub const RAISE: Self = Self(1 << 7);
    pub const UNKNOWN: Self = Self(1 << 8);

    pub fn contains(self, effect: Self) -> bool {
        self.0 & effect.0 != 0
    }
    pub fn insert(&mut self, effect: Self) {
        self.0 |= effect.0;
    }
    pub fn union(&mut self, effects: Self) {
        self.0 |= effects.0;
    }
    pub fn is_pure(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (Self::READ_GLOBAL, "read-global"),
            (Self::WRITE_GLOBAL, "write-global"),
            (Self::READ_TABLE, "read-table"),
            (Self::WRITE_TABLE, "write-table"),
            (Self::ALLOCATE, "allocate"),
            (Self::CALL, "call"),
            (Self::YIELD, "yield"),
            (Self::RAISE, "raise"),
            (Self::UNKNOWN, "unknown"),
        ];
        let mut first = true;
        for (effect, name) in names {
            if self.contains(effect) {
                if !first {
                    f.write_str(",")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("pure")
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableShape {
    pub id: u32,
    pub named: BTreeMap<String, FlowType>,
    pub indexed: FlowType,
    pub escaped: bool,
    pub aliased: bool,
    pub mutated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSignature {
    pub id: u32,
    pub name: String,
    pub params: Vec<FlowType>,
    pub returns: FlowType,
    pub effects: Effects,
    pub captures: BTreeSet<String>,
    pub escapes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationExplanation {
    pub function: String,
    pub line: u32,
    pub strength: FactStrength,
    pub operation: String,
    pub inferred: FlowType,
    pub reason: String,
    pub check_elided: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionAnalysis {
    pub name: String,
    pub returns: FlowType,
    pub effects: Effects,
    pub locals: BTreeMap<String, FlowType>,
    pub cfg: ControlFlowSummary,
}

/// Compact evidence that the structured AST was analyzed as a control-flow
/// graph. `phi_nodes` counts locals whose incoming facts differ at a join.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlFlowSummary {
    pub blocks: u32,
    pub joins: u32,
    pub loop_headers: u32,
    pub phi_nodes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramAnalysis {
    pub policy: TypePolicy,
    pub functions: Vec<FunctionAnalysis>,
    pub signatures: Vec<FunctionSignature>,
    pub table_shapes: Vec<TableShape>,
    pub explanations: Vec<OptimizationExplanation>,
    pub optimization_plan: OptimizationPlan,
}

/// Proofs consumed by the generic bytecode compiler. The key stores
/// `(function, source line, BinaryOp discriminant)`; only operations whose
/// operands are proven integers on every incoming edge are included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OptimizationPlan {
    integer_binary: BTreeSet<(String, u32, u8)>,
}

impl OptimizationPlan {
    pub fn proves_integer_binary(&self, function: &str, line: u32, op: BinaryOp) -> bool {
        self.integer_binary
            .contains(&(function.to_owned(), line, op as u8))
    }
}

impl ProgramAnalysis {
    pub fn elided_checks(&self) -> usize {
        self.explanations
            .iter()
            .filter(|item| item.check_elided)
            .count()
    }

    pub fn remaining_dynamic_checks(&self) -> usize {
        self.explanations
            .iter()
            .filter(|item| !item.check_elided)
            .count()
    }

    pub fn render(&self) -> String {
        let policy = match self.policy {
            TypePolicy::Dynamic => "off",
            TypePolicy::Infer => "infer",
            TypePolicy::Strict => "strict",
        };
        let mut output = format!(
            "type-policy={policy} checks-elided={} dynamic-checks={}\n",
            self.elided_checks(),
            self.remaining_dynamic_checks()
        );
        for item in &self.explanations {
            let result = if item.check_elided {
                "elided"
            } else {
                "dynamic"
            };
            output.push_str(&format!(
                "{}:{}: {result} {} as {}: {}\n",
                item.function, item.line, item.operation, item.inferred, item.reason
            ));
        }
        output
    }
}

#[derive(Debug, Clone)]
struct Binding {
    ty: FlowType,
    shape: Option<u32>,
    function: Option<u32>,
}

#[derive(Debug, Clone)]
struct Fact {
    ty: FlowType,
    shape: Option<u32>,
    function: Option<u32>,
}

impl Fact {
    fn dynamic() -> Self {
        Self {
            ty: FlowType::Dynamic,
            shape: None,
            function: None,
        }
    }
    fn atom(atom: TypeAtom) -> Self {
        Self {
            ty: FlowType::atom(atom),
            shape: None,
            function: None,
        }
    }
}

/// Analyze a program without changing whether it is accepted or how it runs.
pub fn analyze(program: &ast::Program, policy: TypePolicy) -> ProgramAnalysis {
    Analyzer::new(program, policy).run(program)
}

struct Analyzer {
    policy: TypePolicy,
    inference: bool,
    function: String,
    env: HashMap<String, Binding>,
    globals: HashMap<String, u32>,
    signatures: Vec<FunctionSignature>,
    shapes: Vec<TableShape>,
    explanations: Vec<OptimizationExplanation>,
    effects: Effects,
    returns: FlowType,
    captures: BTreeSet<String>,
    cfg: ControlFlowSummary,
    global_signature_count: usize,
    signature_keys: Vec<String>,
    learned_local_params: HashMap<String, Vec<FlowType>>,
    optimization_plan: OptimizationPlan,
}

impl Analyzer {
    fn new(program: &ast::Program, policy: TypePolicy) -> Self {
        let mut signatures = Vec::new();
        let mut globals = HashMap::new();
        for function in &program.functions {
            let id = signatures.len() as u32;
            globals.insert(function.name.clone(), id);
            signatures.push(FunctionSignature {
                id,
                name: function.name.clone(),
                params: function
                    .params
                    .iter()
                    .map(|(_, ty)| FlowType::from_annotation(ty))
                    .collect(),
                returns: function
                    .return_type
                    .as_ref()
                    .map(FlowType::from_annotation)
                    .unwrap_or(FlowType::Bottom),
                effects: Effects::default(),
                captures: BTreeSet::new(),
                escapes: false,
            });
        }
        Self {
            policy,
            inference: policy != TypePolicy::Dynamic,
            function: String::new(),
            env: HashMap::new(),
            globals,
            signatures,
            shapes: Vec::new(),
            explanations: Vec::new(),
            effects: Effects::default(),
            returns: FlowType::Bottom,
            captures: BTreeSet::new(),
            cfg: ControlFlowSummary::default(),
            global_signature_count: program.functions.len(),
            signature_keys: program.functions.iter().map(inference_key).collect(),
            learned_local_params: HashMap::new(),
            optimization_plan: OptimizationPlan::default(),
        }
    }

    fn run(mut self, program: &ast::Program) -> ProgramAnalysis {
        let mut functions = Vec::new();
        let global_count = self.signatures.len();
        // A few fixed-point rounds propagate inferred local/global returns.
        for _ in 0..=program.functions.len() {
            let before = self
                .signatures
                .iter()
                .map(|sig| sig.returns.clone())
                .collect::<Vec<_>>();
            functions.clear();
            self.signatures.truncate(global_count);
            self.signature_keys.truncate(global_count);
            self.shapes.clear();
            self.explanations.clear();
            self.optimization_plan = OptimizationPlan::default();
            for function in &program.functions {
                functions.push(self.analyze_function(function));
            }
            if before
                == self
                    .signatures
                    .iter()
                    .map(|sig| sig.returns.clone())
                    .collect::<Vec<_>>()
            {
                break;
            }
        }
        ProgramAnalysis {
            policy: self.policy,
            functions,
            signatures: self.signatures,
            table_shapes: self.shapes.clone(),
            explanations: finalize_table_explanations(self.explanations, &self.shapes),
            optimization_plan: self.optimization_plan,
        }
    }

    fn analyze_function(&mut self, function: &ast::Function) -> FunctionAnalysis {
        self.function = function.name.clone();
        self.env.clear();
        self.effects = Effects::default();
        self.returns = FlowType::Bottom;
        self.cfg = ControlFlowSummary {
            blocks: 1,
            ..ControlFlowSummary::default()
        };
        for (index, (name, annotation)) in function.params.iter().enumerate() {
            let ty = self.parameter_type(function, index, annotation);
            self.env.insert(
                name.clone(),
                Binding {
                    ty,
                    shape: None,
                    function: None,
                },
            );
        }
        self.block(&function.body);
        if self.returns == FlowType::Bottom {
            self.returns = FlowType::atom(TypeAtom::Nil);
        }
        if let Some(id) = self.globals.get(&function.name).copied() {
            let signature = &mut self.signatures[id as usize];
            if function.return_type.is_none() {
                signature.returns = self.returns.clone();
            }
            signature.effects = self.effects;
        }
        FunctionAnalysis {
            name: function.name.clone(),
            returns: self.returns.clone(),
            effects: self.effects,
            locals: self
                .env
                .iter()
                .map(|(name, binding)| (name.clone(), binding.ty.clone()))
                .collect(),
            cfg: self.cfg,
        }
    }

    fn block(&mut self, block: &[Stmt]) {
        for statement in block {
            self.statement(statement);
        }
    }

    fn scoped_block(&mut self, block: &[Stmt]) {
        let outer = self.env.clone();
        self.block(block);
        self.env.retain(|name, _| outer.contains_key(name));
    }

    fn statement(&mut self, statement: &Stmt) {
        self.cfg.blocks += 1;
        match statement {
            Stmt::Global { values, .. } => {
                self.effects.insert(Effects::WRITE_GLOBAL);
                for value in values {
                    let fact = self.expression(value);
                    self.escape(&fact);
                }
            }
            Stmt::GlobalFunction(function) => {
                self.effects.insert(Effects::WRITE_GLOBAL);
                self.install_local_function(function, true);
            }
            Stmt::LocalFunction(function) => self.install_local_function(function, false),
            Stmt::Label { .. } | Stmt::Goto { .. } | Stmt::Break { .. } => {}
            Stmt::MultiLocal { names, values, .. } => {
                let facts = values
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>();
                for (index, (name, annotation, _, _)) in names.iter().enumerate() {
                    let fact = facts
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| Fact::atom(TypeAtom::Nil));
                    let ty = annotation
                        .as_ref()
                        .map(FlowType::from_annotation)
                        .unwrap_or_else(|| fact.ty.clone());
                    self.bind(name, ty, fact.shape, fact.function);
                }
            }
            Stmt::MultiAssign {
                targets, values, ..
            } => {
                let facts = values
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>();
                for (index, target) in targets.iter().enumerate() {
                    self.assign(
                        target,
                        facts
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| Fact::atom(TypeAtom::Nil)),
                    );
                }
            }
            Stmt::Block(body) => self.scoped_block(body),
            Stmt::Repeat { body, cond, .. } => {
                self.loop_body(body);
                self.expression(cond);
            }
            Stmt::Expr(expression) => {
                let fact = self.expression(expression);
                self.escape(&fact);
            }
            Stmt::Local {
                name, ty, value, ..
            } => {
                let fact = self.expression(value);
                let inferred = ty
                    .as_ref()
                    .map(FlowType::from_annotation)
                    .unwrap_or_else(|| fact.ty.clone());
                self.bind(name, inferred, fact.shape, fact.function);
            }
            Stmt::Assign { target, value, .. } => {
                let fact = self.expression(value);
                self.assign(target, fact);
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => self.conditional(cond, then_block, else_block.as_deref()),
            Stmt::While { cond, body, .. } => {
                self.expression(cond);
                self.loop_body(body);
            }
            Stmt::NumericFor {
                var,
                start,
                stop,
                step,
                body,
                line,
            } => {
                let start = self.expression(start);
                let stop = self.expression(stop);
                let step = step.as_ref().map(|value| self.expression(value));
                let integer = start.ty.is_single(&TypeAtom::Integer)
                    && stop.ty.is_single(&TypeAtom::Integer)
                    && step
                        .as_ref()
                        .is_none_or(|value| value.ty.is_single(&TypeAtom::Integer));
                let outer = self.env.clone();
                let ty = if integer {
                    FlowType::atom(TypeAtom::Integer)
                } else {
                    FlowType::atom(TypeAtom::Float)
                };
                self.bind(var, ty.clone(), None, None);
                self.explain(
                    *line,
                    "numeric-for index",
                    ty,
                    integer,
                    if integer {
                        "integer bounds prove an unboxed induction variable"
                    } else {
                        "numeric bounds require a dynamic numeric guard"
                    },
                );
                self.block(body);
                self.env = outer;
            }
            Stmt::GenericFor {
                iterators, body, ..
            } => {
                for iterator in iterators {
                    let fact = self.expression(iterator);
                    self.escape(&fact);
                }
                self.loop_body(body);
            }
            Stmt::Return { value, .. } => {
                let fact = value
                    .as_ref()
                    .map(|value| self.expression(value))
                    .unwrap_or_else(|| Fact::atom(TypeAtom::Nil));
                self.returns = self.returns.join(&fact.ty);
                self.escape(&fact);
            }
            Stmt::MultiReturn { values, .. } => {
                let first = values
                    .first()
                    .map(|value| self.expression(value))
                    .unwrap_or_else(|| Fact::atom(TypeAtom::Nil));
                self.returns = self.returns.join(&first.ty);
                self.escape(&first);
                for value in values.iter().skip(1) {
                    let fact = self.expression(value);
                    self.escape(&fact);
                }
            }
        }
    }

    fn bind(&mut self, name: &str, ty: FlowType, shape: Option<u32>, function: Option<u32>) {
        if let Some(id) = shape {
            if self.env.values().any(|binding| binding.shape == Some(id)) {
                self.shapes[id as usize].aliased = true;
            }
        }
        self.env.insert(
            name.to_owned(),
            Binding {
                ty,
                shape,
                function,
            },
        );
    }

    fn assign(&mut self, target: &AssignTarget, fact: Fact) {
        match target {
            AssignTarget::Name(name) => {
                if let Some(binding) = self.env.get_mut(name) {
                    binding.ty = binding.ty.join(&fact.ty);
                    binding.shape = if binding.shape == fact.shape {
                        fact.shape
                    } else {
                        None
                    };
                    binding.function = if binding.function == fact.function {
                        fact.function
                    } else {
                        None
                    };
                } else {
                    self.effects.insert(Effects::WRITE_GLOBAL);
                    self.escape(&fact);
                }
            }
            AssignTarget::Field(base, field) => {
                let base = self.expression(base);
                self.effects.insert(Effects::WRITE_TABLE);
                if let Some(id) = base.shape {
                    let shape = &mut self.shapes[id as usize];
                    shape.mutated = true;
                    shape
                        .named
                        .entry(field.clone())
                        .and_modify(|ty| *ty = ty.join(&fact.ty))
                        .or_insert(fact.ty);
                } else {
                    self.escape(&fact);
                }
            }
            AssignTarget::Index(base, index) => {
                let base = self.expression(base);
                self.expression(index);
                self.effects.insert(Effects::WRITE_TABLE);
                if let Some(id) = base.shape {
                    let shape = &mut self.shapes[id as usize];
                    shape.mutated = true;
                    shape.indexed = shape.indexed.join(&fact.ty);
                } else {
                    self.escape(&fact);
                }
            }
        }
    }

    fn conditional(&mut self, cond: &Expr, then_block: &[Stmt], else_block: Option<&[Stmt]>) {
        self.expression(cond);
        let base = self.env.clone();
        let guard = type_guard(cond);
        if let Some((name, atom)) = guard.clone() {
            let narrowed = if let Some(binding) = self.env.get_mut(&name) {
                binding.ty = binding.ty.narrow_to(atom.clone());
                Some(binding.ty.clone())
            } else {
                None
            };
            if let Some(narrowed) = narrowed {
                self.explain(
                    cond.line,
                    "dominated type test",
                    narrowed,
                    true,
                    "the true CFG edge dominates this region",
                );
            }
        } else if let ExprKind::Name(name) = &cond.kind {
            let narrowed = if let Some(binding) = self.env.get_mut(name) {
                binding.ty = binding.ty.without_nil();
                Some(binding.ty.clone())
            } else {
                None
            };
            if let Some(narrowed) = narrowed {
                self.explain(
                    cond.line,
                    "nil elimination",
                    narrowed,
                    true,
                    "the truthy CFG edge excludes nil",
                );
            }
        }
        self.block(then_block);
        let then_env = self.env.clone();
        self.env = base.clone();
        if let Some((name, atom)) = guard {
            if let Some(binding) = self.env.get_mut(&name) {
                binding.ty = binding.ty.remove(&atom);
            }
        }
        if let Some(block) = else_block {
            self.block(block);
        }
        let else_env = self.env.clone();
        self.cfg.joins += 1;
        self.cfg.phi_nodes += differing_bindings(&base, &then_env, &else_env);
        self.env = merge_env(&base, &then_env, &else_env);
    }

    fn loop_body(&mut self, body: &[Stmt]) {
        self.cfg.loop_headers += 1;
        let entry = self.env.clone();
        self.block(body);
        let body_env = self.env.clone();
        self.cfg.joins += 1;
        self.cfg.phi_nodes += differing_bindings(&entry, &entry, &body_env);
        self.env = merge_env(&entry, &entry, &body_env);
    }

    fn install_local_function(&mut self, function: &ast::Function, escapes: bool) {
        let id = self.signatures.len() as u32;
        let key = inference_key(function);
        let params = function
            .params
            .iter()
            .enumerate()
            .map(|(index, (_, ty))| self.parameter_type(function, index, ty))
            .collect();
        self.signatures.push(FunctionSignature {
            id,
            name: function.name.clone(),
            params,
            returns: function
                .return_type
                .as_ref()
                .map(FlowType::from_annotation)
                .unwrap_or(FlowType::Bottom),
            effects: Effects::default(),
            captures: BTreeSet::new(),
            escapes,
        });
        self.signature_keys.push(key);
        self.bind(
            &function.name,
            FlowType::atom(TypeAtom::Function(id)),
            None,
            Some(id),
        );

        let saved_function = self.function.clone();
        let saved_env = self.env.clone();
        let saved_effects = self.effects;
        let saved_returns = self.returns.clone();
        let saved_captures = self.captures.clone();
        let saved_cfg = self.cfg;
        self.function = function.name.clone();
        let outer_names = saved_env.keys().cloned().collect::<BTreeSet<_>>();
        self.env.clear();
        for (index, (name, ty)) in function.params.iter().enumerate() {
            self.bind(name, self.parameter_type(function, index, ty), None, None);
        }
        self.effects = Effects::default();
        self.returns = FlowType::Bottom;
        self.cfg = ControlFlowSummary {
            blocks: 1,
            ..ControlFlowSummary::default()
        };
        self.captures.clear();
        self.block(&function.body);
        let returns = if self.returns == FlowType::Bottom {
            FlowType::atom(TypeAtom::Nil)
        } else {
            self.returns.clone()
        };
        let signature = &mut self.signatures[id as usize];
        if function.return_type.is_none() {
            signature.returns = returns;
        }
        signature.effects = self.effects;
        signature.captures = self.captures.intersection(&outer_names).cloned().collect();
        self.function = saved_function;
        self.env = saved_env;
        self.effects = saved_effects;
        self.returns = saved_returns;
        self.captures = saved_captures;
        self.cfg = saved_cfg;
        self.explain(
            function.line,
            "local function signature",
            FlowType::atom(TypeAtom::Function(id)),
            !escapes,
            if escapes {
                "the closure is dynamically visible"
            } else {
                "the closure is local and has a stable inferred call signature"
            },
        );
    }

    fn expression(&mut self, expression: &Expr) -> Fact {
        if !self.inference {
            self.dynamic_expression_effects(expression);
            return Fact::dynamic();
        }
        match &expression.kind {
            ExprKind::NilLit => Fact::atom(TypeAtom::Nil),
            ExprKind::BoolLit(_) => Fact::atom(TypeAtom::Bool),
            ExprKind::IntLit(_) => Fact::atom(TypeAtom::Integer),
            ExprKind::FloatLit(_) => Fact::atom(TypeAtom::Float),
            ExprKind::StringLit(_) => Fact::atom(TypeAtom::String),
            ExprKind::Vararg => Fact::dynamic(),
            ExprKind::Name(name) => {
                if let Some(binding) = self.env.get(name) {
                    Fact {
                        ty: binding.ty.clone(),
                        shape: binding.shape,
                        function: binding.function,
                    }
                } else if let Some(id) = self.globals.get(name).copied() {
                    Fact {
                        ty: FlowType::atom(TypeAtom::Function(id)),
                        shape: None,
                        function: Some(id),
                    }
                } else {
                    self.effects.insert(Effects::READ_GLOBAL);
                    self.captures.insert(name.clone());
                    Fact::dynamic()
                }
            }
            ExprKind::Table(fields) => self.table(expression.line, fields),
            ExprKind::Function(function) => {
                self.install_local_function(function, false);
                let id = self.signatures.len() as u32 - 1;
                Fact {
                    ty: FlowType::atom(TypeAtom::Function(id)),
                    shape: None,
                    function: Some(id),
                }
            }
            ExprKind::Unary(_, value) => {
                let fact = self.expression(value);
                let ty = match expression.kind {
                    ExprKind::Unary(crate::ast::UnaryOp::Not, _) => FlowType::atom(TypeAtom::Bool),
                    _ => fact.ty,
                };
                Fact {
                    ty,
                    shape: None,
                    function: None,
                }
            }
            ExprKind::Binary(operator, left, right) => {
                let left = self.expression(left);
                let saved_env = self.env.clone();
                if *operator == BinaryOp::And {
                    if let Some((name, atom)) = type_guard(left_expr(expression)) {
                        let narrowed = if let Some(binding) = self.env.get_mut(&name) {
                            binding.ty = binding.ty.narrow_to(atom);
                            Some(binding.ty.clone())
                        } else {
                            None
                        };
                        if let Some(narrowed) = narrowed {
                            self.explain(
                                expression.line,
                                "short-circuit type test",
                                narrowed,
                                true,
                                "the right operand is dominated by the successful left guard",
                            );
                        }
                    } else if let ExprKind::Name(name) = &left_expr(expression).kind {
                        if let Some(binding) = self.env.get_mut(name) {
                            binding.ty = binding.ty.without_nil();
                        }
                    }
                }
                let right = self.expression(right);
                self.env = saved_env;
                let comparison = matches!(
                    operator,
                    BinaryOp::Eq
                        | BinaryOp::NotEq
                        | BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                );
                let logical = matches!(operator, BinaryOp::And | BinaryOp::Or);
                let ty = if comparison {
                    FlowType::atom(TypeAtom::Bool)
                } else if logical {
                    left.ty.join(&right.ty)
                } else if left.ty.is_single(&TypeAtom::Integer)
                    && right.ty.is_single(&TypeAtom::Integer)
                    && !matches!(operator, BinaryOp::Div | BinaryOp::Pow)
                {
                    FlowType::atom(TypeAtom::Integer)
                } else if is_numeric(&left.ty) && is_numeric(&right.ty) {
                    FlowType::atom(TypeAtom::Float)
                } else {
                    FlowType::Dynamic
                };
                let proven = ty != FlowType::Dynamic;
                if ty.is_single(&TypeAtom::Integer)
                    && matches!(operator, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)
                {
                    self.optimization_plan.integer_binary.insert((
                        self.function.clone(),
                        expression.line,
                        *operator as u8,
                    ));
                }
                self.explain(
                    expression.line,
                    format!("operator {operator:?}"),
                    ty.clone(),
                    proven,
                    if proven {
                        "operand lattice facts select a check-free operation"
                    } else {
                        "operand types or metamethod effects remain dynamic"
                    },
                );
                Fact {
                    ty,
                    shape: None,
                    function: None,
                }
            }
            ExprKind::Call(name, args) => {
                let facts = args
                    .iter()
                    .map(|arg| self.expression(arg))
                    .collect::<Vec<_>>();
                for fact in &facts {
                    self.escape(fact);
                }
                self.effects.insert(Effects::CALL);
                let id = self
                    .env
                    .get(name)
                    .and_then(|binding| binding.function)
                    .or_else(|| self.globals.get(name).copied());
                if let Some(id) = id {
                    self.observe_local_call(id, &facts);
                    let signature = &self.signatures[id as usize];
                    let ty = signature.returns.clone();
                    self.effects.union(signature.effects);
                    let proven = ty != FlowType::Dynamic && ty != FlowType::Bottom;
                    self.explain(
                        expression.line,
                        format!("call {name}"),
                        ty.clone(),
                        proven,
                        if proven {
                            "the local call signature has a fixed inferred result"
                        } else {
                            "the callee result is not proven"
                        },
                    );
                    Fact {
                        ty,
                        shape: None,
                        function: None,
                    }
                } else {
                    self.effects.insert(Effects::UNKNOWN);
                    if name == "error" {
                        self.effects.insert(Effects::RAISE);
                    }
                    if name == "yield" || name == "coroutine.yield" {
                        self.effects.insert(Effects::YIELD);
                    }
                    self.explain(
                        expression.line,
                        format!("call {name}"),
                        FlowType::Dynamic,
                        false,
                        "the global callee may be rebound",
                    );
                    Fact::dynamic()
                }
            }
            ExprKind::CallExpr(callee, args) | ExprKind::MethodCall(callee, _, args) => {
                let callee = self.expression(callee);
                for arg in args {
                    let fact = self.expression(arg);
                    self.escape(&fact);
                }
                self.escape(&callee);
                self.effects.insert(Effects::CALL);
                self.effects.insert(Effects::UNKNOWN);
                Fact::dynamic()
            }
            ExprKind::Index(base, index) => {
                let base = self.expression(base);
                self.expression(index);
                self.effects.insert(Effects::READ_TABLE);
                let ty = base
                    .shape
                    .filter(|id| !self.shapes[*id as usize].escaped)
                    .map(|id| self.shapes[id as usize].indexed.clone())
                    .unwrap_or(FlowType::Dynamic);
                self.explain(
                    expression.line,
                    "table index",
                    ty.clone(),
                    ty != FlowType::Dynamic,
                    if ty != FlowType::Dynamic {
                        "a nonescaping table shape fixes the indexed value type"
                    } else {
                        "the table shape is escaped or polymorphic"
                    },
                );
                Fact {
                    ty,
                    shape: None,
                    function: None,
                }
            }
            ExprKind::Field(base, field) => {
                let base = self.expression(base);
                self.effects.insert(Effects::READ_TABLE);
                let ty = base
                    .shape
                    .filter(|id| !self.shapes[*id as usize].escaped)
                    .and_then(|id| self.shapes[id as usize].named.get(field).cloned())
                    .unwrap_or(FlowType::Dynamic);
                self.explain(
                    expression.line,
                    format!("field {field}"),
                    ty.clone(),
                    ty != FlowType::Dynamic,
                    if ty != FlowType::Dynamic {
                        "a nonescaping table shape proves the field layout"
                    } else {
                        "the field shape is not locally proven"
                    },
                );
                Fact {
                    ty,
                    shape: None,
                    function: None,
                }
            }
            ExprKind::Len(value) => {
                let fact = self.expression(value);
                let proven = fact.shape.is_some() || fact.ty.is_single(&TypeAtom::String);
                self.explain(
                    expression.line,
                    "length",
                    FlowType::atom(TypeAtom::Integer),
                    proven,
                    if proven {
                        "the operand has a length-bearing proven type"
                    } else {
                        "a metamethod check remains possible"
                    },
                );
                Fact::atom(TypeAtom::Integer)
            }
            ExprKind::StructLiteral(_, fields) => {
                for (_, value) in fields {
                    self.expression(value);
                }
                self.effects.insert(Effects::ALLOCATE);
                Fact::dynamic()
            }
            ExprKind::TypeTest(value, _) => {
                self.expression(value);
                Fact::atom(TypeAtom::Bool)
            }
            ExprKind::Cast(_, target) => Fact {
                ty: FlowType::from_annotation(target),
                shape: None,
                function: None,
            },
            ExprKind::Paren(inner) => self.expression(inner),
        }
    }

    fn table(&mut self, line: u32, fields: &[TableField]) -> Fact {
        let id = self.shapes.len() as u32;
        let mut named = BTreeMap::new();
        let mut indexed = FlowType::Bottom;
        for field in fields {
            match field {
                TableField::Named(name, value) => {
                    named.insert(name.clone(), self.expression(value).ty);
                }
                TableField::Value(value) => {
                    indexed = indexed.join(&self.expression(value).ty);
                }
                TableField::Key(key, value) => {
                    self.expression(key);
                    indexed = indexed.join(&self.expression(value).ty);
                }
            }
        }
        self.effects.insert(Effects::ALLOCATE);
        self.shapes.push(TableShape {
            id,
            named,
            indexed,
            escaped: false,
            aliased: false,
            mutated: false,
        });
        let ty = FlowType::atom(TypeAtom::Table(id));
        self.explain(
            line,
            "table literal",
            ty.clone(),
            true,
            "the allocation is local and its initial shape is closed",
        );
        Fact {
            ty,
            shape: Some(id),
            function: None,
        }
    }

    fn escape(&mut self, fact: &Fact) {
        if let Some(id) = fact.shape {
            self.shapes[id as usize].escaped = true;
        }
        if let Some(id) = fact.function {
            self.signatures[id as usize].escapes = true;
        }
    }

    fn dynamic_expression_effects(&mut self, expression: &Expr) {
        match &expression.kind {
            ExprKind::Call(_, _) | ExprKind::CallExpr(_, _) | ExprKind::MethodCall(_, _, _) => {
                self.effects.insert(Effects::CALL);
                self.effects.insert(Effects::UNKNOWN);
            }
            ExprKind::Table(_) | ExprKind::Function(_) | ExprKind::StructLiteral(_, _) => {
                self.effects.insert(Effects::ALLOCATE)
            }
            ExprKind::Index(_, _) | ExprKind::Field(_, _) | ExprKind::Len(_) => {
                self.effects.insert(Effects::READ_TABLE)
            }
            _ => {}
        }
        self.explain(
            expression.line,
            "expression",
            FlowType::Dynamic,
            false,
            "type inference is disabled by policy",
        );
    }

    fn parameter_type(
        &self,
        function: &ast::Function,
        index: usize,
        annotation: &TypeName,
    ) -> FlowType {
        if !self.inference {
            return FlowType::Dynamic;
        }
        if function
            .param_annotations
            .get(index)
            .copied()
            .unwrap_or(true)
        {
            return FlowType::from_annotation(annotation);
        }
        self.learned_local_params
            .get(&inference_key(function))
            .and_then(|params| params.get(index))
            .cloned()
            .unwrap_or(FlowType::Bottom)
    }

    fn observe_local_call(&mut self, id: u32, arguments: &[Fact]) {
        let index = id as usize;
        if index < self.global_signature_count || self.signatures[index].escapes {
            return;
        }
        let key = self.signature_keys[index].clone();
        let params = self
            .learned_local_params
            .entry(key)
            .or_insert_with(|| vec![FlowType::Bottom; self.signatures[index].params.len()]);
        for (slot, argument) in params.iter_mut().zip(arguments) {
            *slot = slot.join(&argument.ty);
        }
    }

    fn explain(
        &mut self,
        line: u32,
        operation: impl Into<String>,
        inferred: FlowType,
        check_elided: bool,
        reason: impl Into<String>,
    ) {
        self.explanations.push(OptimizationExplanation {
            function: self.function.clone(),
            line,
            strength: if check_elided {
                FactStrength::Proven
            } else {
                FactStrength::Unknown
            },
            operation: operation.into(),
            inferred,
            reason: reason.into(),
            check_elided,
        });
    }
}

fn merge_env(
    base: &HashMap<String, Binding>,
    left: &HashMap<String, Binding>,
    right: &HashMap<String, Binding>,
) -> HashMap<String, Binding> {
    base.iter()
        .map(|(name, original)| {
            let left = left.get(name).unwrap_or(original);
            let right = right.get(name).unwrap_or(original);
            (
                name.clone(),
                Binding {
                    ty: left.ty.join(&right.ty),
                    shape: if left.shape == right.shape {
                        left.shape
                    } else {
                        None
                    },
                    function: if left.function == right.function {
                        left.function
                    } else {
                        None
                    },
                },
            )
        })
        .collect()
}

fn differing_bindings(
    base: &HashMap<String, Binding>,
    left: &HashMap<String, Binding>,
    right: &HashMap<String, Binding>,
) -> u32 {
    base.keys()
        .filter(|name| {
            let original = &base[*name];
            left.get(*name)
                .is_some_and(|binding| binding.ty != original.ty)
                || right
                    .get(*name)
                    .is_some_and(|binding| binding.ty != original.ty)
                || left
                    .get(*name)
                    .zip(right.get(*name))
                    .is_some_and(|(a, b)| a.ty != b.ty)
        })
        .count() as u32
}

fn finalize_table_explanations(
    mut explanations: Vec<OptimizationExplanation>,
    shapes: &[TableShape],
) -> Vec<OptimizationExplanation> {
    for explanation in &mut explanations {
        if explanation.operation != "table literal" {
            continue;
        }
        let FlowType::Union(members) = &explanation.inferred else {
            continue;
        };
        let Some(TypeAtom::Table(id)) = members.iter().next() else {
            continue;
        };
        let shape = &shapes[*id as usize];
        if shape.escaped {
            explanation.check_elided = false;
            explanation.strength = FactStrength::Unknown;
            explanation.reason = "the table escapes to an unknown operation, so allocation and dynamic layout remain".into();
        } else if shape.aliased || shape.mutated {
            explanation.reason =
                "all aliases and mutations stay local, so the table shape remains proven".into();
        }
    }
    explanations
}

fn is_numeric(ty: &FlowType) -> bool {
    matches!(ty, FlowType::Union(members) if !members.is_empty() && members.iter().all(|member| matches!(member, TypeAtom::Integer | TypeAtom::Float)))
}

fn type_guard(condition: &Expr) -> Option<(String, TypeAtom)> {
    match &condition.kind {
        ExprKind::TypeTest(value, target) => {
            let ExprKind::Name(name) = &value.kind else {
                return None;
            };
            Some((name.clone(), annotation_atom(target)?))
        }
        ExprKind::Binary(BinaryOp::And, left, _) => type_guard(left),
        ExprKind::Binary(BinaryOp::Eq, left, right) => {
            lua_type_guard(left, right).or_else(|| lua_type_guard(right, left))
        }
        _ => None,
    }
}

fn left_expr(expression: &Expr) -> &Expr {
    let ExprKind::Binary(_, left, _) = &expression.kind else {
        unreachable!("called only for a binary expression")
    };
    left
}

fn lua_type_guard(call: &Expr, value: &Expr) -> Option<(String, TypeAtom)> {
    let ExprKind::Call(name, args) = &call.kind else {
        return None;
    };
    if name != "type" || args.len() != 1 {
        return None;
    }
    let ExprKind::Name(local) = &args[0].kind else {
        return None;
    };
    let ExprKind::StringLit(kind) = &value.kind else {
        return None;
    };
    let atom = match kind.as_slice() {
        b"nil" => TypeAtom::Nil,
        b"boolean" => TypeAtom::Bool,
        b"string" => TypeAtom::String,
        _ => return None,
    };
    Some((local.clone(), atom))
}

fn annotation_atom(annotation: &TypeName) -> Option<TypeAtom> {
    match annotation {
        TypeName::I64 => Some(TypeAtom::Integer),
        TypeName::F64 => Some(TypeAtom::Float),
        TypeName::Bool => Some(TypeAtom::Bool),
        TypeName::Nil => Some(TypeAtom::Nil),
        TypeName::String => Some(TypeAtom::String),
        _ => None,
    }
}

fn inference_key(function: &ast::Function) -> String {
    format!("{}@{}", function.name, function.line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze_source(source: &str, policy: TypePolicy) -> ProgramAnalysis {
        let tokens = crate::lexer::lex(source).unwrap();
        let program =
            crate::parser::parse_with_config(tokens, crate::parser::LanguageConfig::SOL).unwrap();
        analyze(&program, policy)
    }

    #[test]
    fn lattice_joins_small_unions_eliminates_nil_and_widens() {
        let nil = FlowType::atom(TypeAtom::Nil);
        let integer = FlowType::atom(TypeAtom::Integer);
        assert_eq!(nil.join(&integer).without_nil(), integer);
        let mut ty = FlowType::Bottom;
        for atom in [
            TypeAtom::Nil,
            TypeAtom::Bool,
            TypeAtom::Integer,
            TypeAtom::Float,
            TypeAtom::String,
        ] {
            ty = ty.join(&FlowType::atom(atom));
        }
        assert_eq!(ty, FlowType::Dynamic);
    }

    #[test]
    fn inference_proves_loop_local_call_table_and_dominated_guard() {
        let report = analyze_source(
            r#"
            local function add(a: i64, b: i64) return a + b end
            local item = { value = 40 }
            local total = 0
            for i = 1, 2 do total = add(total, i) end
            local maybe: any = item.value
            if maybe is i64 then return maybe + 1 end
            return 0
        "#,
            TypePolicy::Infer,
        );
        let text = report.render();
        assert!(text.contains("local function signature"), "{text}");
        assert!(text.contains("numeric-for index"), "{text}");
        assert!(text.contains("dominated type test"), "{text}");
        assert!(text.contains("field value"), "{text}");
        assert!(report.elided_checks() >= 5, "{text}");
        let shape = report.table_shapes.first().unwrap();
        assert!(!shape.escaped);
        assert_eq!(shape.named["value"], FlowType::atom(TypeAtom::Integer));
    }

    #[test]
    fn off_policy_retains_dynamic_checks() {
        let report = analyze_source("local x = 1 return x + 2", TypePolicy::Dynamic);
        assert_eq!(report.elided_checks(), 0);
        assert!(report.remaining_dynamic_checks() > 0);
    }

    #[test]
    fn analysis_tracks_alias_mutation_escape_effects_and_cfg_joins() {
        let report = analyze_source(
            r#"
            local value: any = nil
            local item = { count = 1 }
            local alias = item
            if value then item.count = 2 else item.count = 3 end
            consume(item)
            return alias.count
        "#,
            TypePolicy::Infer,
        );
        let shape = report.table_shapes.first().unwrap();
        assert!(shape.aliased);
        assert!(shape.mutated);
        assert!(shape.escaped);
        let main = report
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        assert!(main.effects.contains(Effects::ALLOCATE));
        assert!(main.effects.contains(Effects::WRITE_TABLE));
        assert!(main.effects.contains(Effects::UNKNOWN));
        assert!(main.cfg.joins >= 1);
        assert!(report.render().contains("nil elimination"));
    }

    #[test]
    fn omitted_parameters_infer_but_explicit_any_remains_a_dynamic_contract() {
        let inferred = analyze_source(
            "local function identity(value) return value end return identity(42)",
            TypePolicy::Infer,
        );
        let inferred_text = inferred.render();
        assert!(
            inferred_text.contains("elided call identity as integer"),
            "{inferred_text}"
        );

        let explicit = analyze_source(
            "local function identity(value: any) return value end return identity(42)",
            TypePolicy::Strict,
        );
        let explicit_text = explicit.render();
        assert!(
            explicit_text.contains("dynamic call identity as dynamic"),
            "{explicit_text}"
        );
    }

    #[test]
    fn proven_integer_arithmetic_is_lowered_to_specialized_bytecode() {
        let tokens = crate::lexer::lex("local value = 40 return value + 2").unwrap();
        let program =
            crate::parser::parse_with_config(tokens, crate::parser::LanguageConfig::SOL).unwrap();
        let report = analyze(&program, TypePolicy::Infer);
        let main = program
            .functions
            .iter()
            .find(|function| function.name == "main")
            .unwrap();
        let prototype = crate::lua_bytecode::Compiler::compile_top_level_with_plan(
            main,
            &report.optimization_plan,
        )
        .unwrap();
        assert!(prototype.instrs.iter().any(|instruction| matches!(
            instruction,
            crate::lua_bytecode::Instr::IntegerBinary(BinaryOp::Add, ..)
        )));
    }

    #[test]
    fn short_circuit_right_operand_uses_the_dominating_type_guard() {
        let report = analyze_source(
            "local value: any = 41 return value is i64 and value + 1",
            TypePolicy::Infer,
        );
        let text = report.render();
        assert!(text.contains("short-circuit type test"), "{text}");
        assert!(text.contains("elided operator Add as integer"), "{text}");
    }
}
