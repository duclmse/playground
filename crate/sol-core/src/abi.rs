use std::collections::HashMap;
use std::fmt;

/// Stable identity of a function inside one loaded runtime image.
///
/// IDs are deliberately wider than either legacy engine's `u8`/`u16` indices.
/// The canonical dispatcher assigns them when prototypes and native providers
/// enter the shared runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(u32);

impl FunctionId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Portable identity of a host-provided callable. The provider namespace is
/// allocated by the owning runtime; `function` is meaningful only within that
/// provider. Host pointers and Rust object addresses are deliberately absent
/// from this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NativeCallableId {
    pub provider: u32,
    pub function: u32,
}

impl NativeCallableId {
    pub const fn new(provider: u32, function: u32) -> Self {
        Self { provider, function }
    }
}

/// Number of values consumed or produced at a semantic call boundary.
/// `Open` is Lua's open argument/result sequence and replaces the legacy
/// `-1` sentinel used by the dynamic bytecode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueCount {
    Fixed(u32),
    Open,
}

impl ValueCount {
    pub const ZERO: Self = Self::Fixed(0);
    pub const ONE: Self = Self::Fixed(1);

    pub const fn fixed(count: usize) -> Option<Self> {
        if count <= u32::MAX as usize {
            Some(Self::Fixed(count as u32))
        } else {
            None
        }
    }

    pub const fn as_fixed(self) -> Option<usize> {
        match self {
            Self::Fixed(count) => Some(count as usize),
            Self::Open => None,
        }
    }

    /// Converts the temporary dynamic-bytecode representation where `-1`
    /// means open and non-negative values mean a fixed count.
    pub fn from_legacy(count: i32) -> Result<Self, ValueCountError> {
        match count {
            -1 => Ok(Self::Open),
            0.. => Ok(Self::Fixed(count as u32)),
            _ => Err(ValueCountError(count)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueCountError(pub i32);

impl fmt::Display for ValueCountError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "invalid semantic value count {}", self.0)
    }
}

impl std::error::Error for ValueCountError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionArity {
    pub parameters: u32,
    pub variadic: bool,
}

impl FunctionArity {
    pub const fn new(parameters: u32, variadic: bool) -> Self {
        Self {
            parameters,
            variadic,
        }
    }
}

/// Metadata shared by generic Lua bytecode and specialized Sol bytecode.
/// Instruction and constant representations may differ by tier, but every
/// executable prototype exposes this same identity-independent contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrototypeMetadata {
    pub name: String,
    pub arity: FunctionArity,
    pub registers: u32,
}

/// The byte-oriented source position associated with one executable instruction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLocation {
    pub line: u32,
    pub column: u32,
}

impl SourceLocation {
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

/// Tier-independent instruction-to-source mapping. Generic and specialized
/// bytecode can have different instruction formats while debugger, error, and
/// deoptimization consumers use the same program-counter lookup contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceMap {
    locations: Vec<SourceLocation>,
}

impl SourceMap {
    pub fn new(locations: Vec<SourceLocation>) -> Self {
        Self { locations }
    }

    pub fn single_line(instruction_count: usize, line: u32) -> Self {
        Self::new(vec![SourceLocation::new(line, 0); instruction_count])
    }

    pub fn location(&self, pc: u32) -> Option<SourceLocation> {
        self.locations.get(pc as usize).copied()
    }

    pub fn len(&self) -> usize {
        self.locations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

/// Common introspection contract implemented by every executable prototype.
pub trait ExecutablePrototype {
    fn metadata(&self) -> &PrototypeMetadata;
    fn source_map(&self) -> &SourceMap;
    fn instruction_count(&self) -> usize;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTier {
    Generic,
    Specialized,
    Native,
    SemanticAdapter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDescriptor {
    pub id: FunctionId,
    pub metadata: PrototypeMetadata,
    pub tier: ExecutionTier,
}

/// One identity/catalog domain for generic, specialized, and host functions.
/// Tier promotion updates a descriptor rather than allocating a second function
/// identity.
#[derive(Debug, Default)]
pub struct FunctionRegistry {
    entries: HashMap<FunctionId, FunctionDescriptor>,
    next: u32,
}

impl FunctionRegistry {
    pub fn register(
        &mut self,
        metadata: PrototypeMetadata,
        tier: ExecutionTier,
    ) -> Result<FunctionId, FunctionRegistryError> {
        while self.entries.contains_key(&FunctionId::new(self.next)) {
            self.next = self
                .next
                .checked_add(1)
                .ok_or(FunctionRegistryError::IdsExhausted)?;
        }
        let id = FunctionId::new(self.next);
        self.next = self
            .next
            .checked_add(1)
            .ok_or(FunctionRegistryError::IdsExhausted)?;
        self.insert(id, metadata, tier)?;
        Ok(id)
    }

    pub fn insert(
        &mut self,
        id: FunctionId,
        metadata: PrototypeMetadata,
        tier: ExecutionTier,
    ) -> Result<(), FunctionRegistryError> {
        if self.entries.contains_key(&id) {
            return Err(FunctionRegistryError::Duplicate(id));
        }
        self.entries
            .insert(id, FunctionDescriptor { id, metadata, tier });
        Ok(())
    }

    pub fn get(&self, id: FunctionId) -> Option<&FunctionDescriptor> {
        self.entries.get(&id)
    }

    pub fn set_tier(
        &mut self,
        id: FunctionId,
        tier: ExecutionTier,
    ) -> Result<(), FunctionRegistryError> {
        let descriptor = self
            .entries
            .get_mut(&id)
            .ok_or(FunctionRegistryError::Unknown(id))?;
        descriptor.tier = tier;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionRegistryError {
    Duplicate(FunctionId),
    Unknown(FunctionId),
    IdsExhausted,
}

impl fmt::Display for FunctionRegistryError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(out, "function ID {} is already registered", id.get()),
            Self::Unknown(id) => write!(out, "function ID {} is not registered", id.get()),
            Self::IdsExhausted => out.write_str("function ID space exhausted"),
        }
    }
}

impl std::error::Error for FunctionRegistryError {}

impl PrototypeMetadata {
    pub fn new(
        name: impl Into<String>,
        parameters: usize,
        variadic: bool,
        registers: usize,
    ) -> Option<Self> {
        Some(Self {
            name: name.into(),
            arity: FunctionArity::new(u32::try_from(parameters).ok()?, variadic),
            registers: u32::try_from(registers).ok()?,
        })
    }
}

/// Semantic behavior of a call independent of the executing tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Normal,
    Tail,
    Protected,
}

/// A tier-independent request to invoke a callable. Arguments use the caller's
/// current representation; boxing adapters translate the generic parameter when
/// a request crosses between specialized and dynamic tiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRequest<V> {
    pub function: FunctionId,
    pub arguments: Vec<V>,
    pub results: ValueCount,
    pub kind: CallKind,
}

impl<V> CallRequest<V> {
    pub fn new(
        function: FunctionId,
        arguments: Vec<V>,
        results: ValueCount,
        kind: CallKind,
    ) -> Self {
        Self {
            function,
            arguments,
            results,
            kind,
        }
    }
}

/// Register-window description shared by generic and specialized bytecode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallSite {
    pub base: u32,
    pub arguments: ValueCount,
    pub results: ValueCount,
    pub kind: CallKind,
}

impl CallSite {
    pub const fn new(
        base: u32,
        arguments: ValueCount,
        results: ValueCount,
        kind: CallKind,
    ) -> Self {
        Self {
            base,
            arguments,
            results,
            kind,
        }
    }
}

/// Common execution state for interpreter, native, coroutine, debugger, and
/// deoptimization frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    Ready,
    Running,
    Suspended,
    Returned,
    Failed,
}

/// Tier-independent frame header. Register storage remains tier-specific:
/// generic frames contain canonical `Value`s while proven typed frames may
/// keep unboxed scalar slots described by their stack map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub function: FunctionId,
    pub pc: u32,
    pub stack_base: u32,
    pub stack_top: u32,
    pub state: FrameState,
}

impl FrameHeader {
    pub const fn new(function: FunctionId, stack_base: u32, stack_top: u32) -> Self {
        Self {
            function,
            pc: 0,
            stack_base,
            stack_top,
            state: FrameState::Ready,
        }
    }
}

/// Result of crossing the semantic call ABI. A host or execution tier can
/// translate its own error representation into `Raised` without changing the
/// dispatcher contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOutcome<V, E> {
    Returned(Vec<V>),
    Yielded(Vec<V>),
    TailCall(CallRequest<V>),
    Raised(E),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_counts_replace_negative_sentinels() {
        assert_eq!(ValueCount::from_legacy(-1), Ok(ValueCount::Open));
        assert_eq!(ValueCount::from_legacy(0), Ok(ValueCount::ZERO));
        assert_eq!(ValueCount::from_legacy(3), Ok(ValueCount::Fixed(3)));
        assert_eq!(ValueCount::from_legacy(-2), Err(ValueCountError(-2)));
    }

    #[test]
    fn frame_header_can_suspend_and_resume_without_tier_state() {
        let mut frame = FrameHeader::new(FunctionId::new(7), 4, 12);
        frame.pc = 9;
        frame.state = FrameState::Suspended;
        assert_eq!(frame.function.get(), 7);
        assert_eq!(frame.pc, 9);
        assert_eq!(frame.stack_base, 4);
        assert_eq!(frame.stack_top, 12);
    }

    #[test]
    fn prototype_metadata_is_shared_without_narrow_legacy_limits() {
        let metadata = PrototypeMetadata::new("main", 300, true, 70_000).unwrap();
        assert_eq!(metadata.name, "main");
        assert_eq!(metadata.arity, FunctionArity::new(300, true));
        assert_eq!(metadata.registers, 70_000);
    }

    #[test]
    fn source_maps_have_one_lookup_contract_for_every_tier() {
        let map = SourceMap::new(vec![
            SourceLocation::new(4, 2),
            SourceLocation::new(4, 9),
            SourceLocation::new(5, 0),
        ]);
        assert_eq!(map.location(1), Some(SourceLocation::new(4, 9)));
        assert_eq!(map.location(3), None);
    }

    #[test]
    fn tail_calls_are_semantic_abi_outcomes() {
        let outcome = CallOutcome::<u64, ()>::TailCall(CallRequest::new(
            FunctionId::new(12),
            vec![7, 8],
            ValueCount::ONE,
            CallKind::Tail,
        ));
        let CallOutcome::TailCall(request) = outcome else {
            panic!("expected a tail-call request")
        };
        assert_eq!(request.function, FunctionId::new(12));
        assert_eq!(request.arguments, vec![7, 8]);
    }

    #[test]
    fn one_function_identity_survives_tier_promotion() {
        let mut registry = FunctionRegistry::default();
        let id = registry
            .register(
                PrototypeMetadata::new("work", 1, false, 2).unwrap(),
                ExecutionTier::Generic,
            )
            .unwrap();
        registry.set_tier(id, ExecutionTier::Native).unwrap();
        let descriptor = registry.get(id).unwrap();
        assert_eq!(descriptor.metadata.name, "work");
        assert_eq!(descriptor.tier, ExecutionTier::Native);
    }
}
