use std::fmt;

/// Stable, generation-checked identity for a managed object.
///
/// The low 32 bits store `slot + 1` (zero remains invalid); the high 32 bits
/// store the slot generation so a swept handle cannot alias a later object.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(u64);

impl ObjectId {
    pub(crate) fn new(slot: usize, generation: u32) -> Self {
        assert!(slot < u32::MAX as usize);
        Self(((generation as u64) << 32) | (slot as u64 + 1))
    }

    pub(crate) fn slot(self) -> usize {
        ((self.0 as u32) - 1) as usize
    }

    pub(crate) fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw as u32 == 0 {
            None
        } else {
            Some(Self(raw))
        }
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({}:{})", self.slot(), self.generation())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum ValueTag {
    Nil = 0,
    Boolean = 1,
    Integer = 2,
    Float = 3,
    Object = 4,
}

/// Unboxed scalar layouts understood by every semantic call adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarKind {
    I64,
    F64,
    Bool,
}

/// A value at a dynamic/specialized tier boundary. Proven scalars stay in
/// their unboxed register representation; every identity-bearing value is a
/// canonical handle and is therefore passed without copying its object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoundaryValue {
    Unboxed { kind: ScalarKind, bits: u64 },
    Canonical(Value),
}

impl BoundaryValue {
    pub const fn unboxed(kind: ScalarKind, bits: u64) -> Self {
        Self::Unboxed { kind, bits }
    }

    pub const fn canonical(value: Value) -> Self {
        Self::Canonical(value)
    }

    pub fn boxed(self) -> Value {
        match self {
            Self::Unboxed {
                kind: ScalarKind::I64,
                bits,
            } => Value::integer(bits as i64),
            Self::Unboxed {
                kind: ScalarKind::F64,
                bits,
            } => Value::float(f64::from_bits(bits)),
            Self::Unboxed {
                kind: ScalarKind::Bool,
                bits,
            } => Value::boolean(bits != 0),
            Self::Canonical(value) => value,
        }
    }

    pub fn checked_unbox(value: Value, expected: ScalarKind) -> Result<Self, BoundaryTypeError> {
        let bits = match expected {
            ScalarKind::I64 => value.as_integer().map(|value| value as u64),
            ScalarKind::F64 => value
                .as_float()
                .or_else(|| value.as_integer().map(|value| value as f64))
                .map(f64::to_bits),
            ScalarKind::Bool => value.as_bool().map(u64::from),
        }
        .ok_or(BoundaryTypeError {
            expected,
            actual: value.tag(),
        })?;
        Ok(Self::unboxed(expected, bits))
    }

    pub const fn bits(self) -> Option<u64> {
        match self {
            Self::Unboxed { bits, .. } => Some(bits),
            Self::Canonical(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryTypeError {
    pub expected: ScalarKind,
    pub actual: ValueTag,
}

impl fmt::Display for BoundaryTypeError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "semantic boundary expected {:?}, got {:?}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for BoundaryTypeError {}

/// Canonical runtime register/stack value.
///
/// The explicit tag/payload layout is portable to WebAssembly and native
/// targets. Proven typed values may remain unboxed in optimized frames; they
/// enter this representation only at the semantic ABI boundary.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Value {
    tag: ValueTag,
    payload: u64,
}

const _: () = assert!(std::mem::size_of::<Value>() == 16);

impl Value {
    pub const NIL: Self = Self {
        tag: ValueTag::Nil,
        payload: 0,
    };

    pub const fn boolean(value: bool) -> Self {
        Self {
            tag: ValueTag::Boolean,
            payload: value as u64,
        }
    }

    pub const fn integer(value: i64) -> Self {
        Self {
            tag: ValueTag::Integer,
            payload: value as u64,
        }
    }

    pub fn float(value: f64) -> Self {
        Self {
            tag: ValueTag::Float,
            payload: value.to_bits(),
        }
    }

    pub const fn object(value: ObjectId) -> Self {
        Self {
            tag: ValueTag::Object,
            payload: value.raw(),
        }
    }

    pub const fn tag(self) -> ValueTag {
        self.tag
    }

    pub const fn as_bool(self) -> Option<bool> {
        if matches!(self.tag, ValueTag::Boolean) {
            Some(self.payload != 0)
        } else {
            None
        }
    }

    pub const fn as_integer(self) -> Option<i64> {
        if matches!(self.tag, ValueTag::Integer) {
            Some(self.payload as i64)
        } else {
            None
        }
    }

    pub fn as_float(self) -> Option<f64> {
        if matches!(self.tag, ValueTag::Float) {
            Some(f64::from_bits(self.payload))
        } else {
            None
        }
    }

    pub const fn as_object(self) -> Option<ObjectId> {
        if matches!(self.tag, ValueTag::Object) {
            ObjectId::from_raw(self.payload)
        } else {
            None
        }
    }

    pub const fn truthy(self) -> bool {
        !(matches!(self.tag, ValueTag::Nil)
            || matches!(self.tag, ValueTag::Boolean) && self.payload == 0)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self.tag, other.tag) {
            (ValueTag::Integer, ValueTag::Float) => {
                integer_equals_float(self.payload as i64, f64::from_bits(other.payload))
            }
            (ValueTag::Float, ValueTag::Integer) => {
                integer_equals_float(other.payload as i64, f64::from_bits(self.payload))
            }
            (left, right) => left == right && self.payload == other.payload,
        }
    }
}

fn integer_equals_float(integer: i64, float: f64) -> bool {
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    float.is_finite()
        && float.fract() == 0.0
        && float >= i64::MIN as f64
        && float < I64_UPPER_EXCLUSIVE
        && float as i64 == integer
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.tag {
            ValueTag::Nil => f.write_str("Nil"),
            ValueTag::Boolean => f
                .debug_tuple("Boolean")
                .field(&self.as_bool().unwrap())
                .finish(),
            ValueTag::Integer => f
                .debug_tuple("Integer")
                .field(&self.as_integer().unwrap())
                .finish(),
            ValueTag::Float => f
                .debug_tuple("Float")
                .field(&self.as_float().unwrap())
                .finish(),
            ValueTag::Object => f
                .debug_tuple("Object")
                .field(&self.as_object().unwrap())
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_value_is_portable_and_preserves_lua_scalar_rules() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(Value::integer(42), Value::float(42.0));
        assert_ne!(
            Value::integer(9_007_199_254_740_993),
            Value::float(9_007_199_254_740_993.0)
        );
        assert!(!Value::NIL.truthy());
        assert!(!Value::boolean(false).truthy());
        assert!(Value::integer(0).truthy());
    }

    #[test]
    fn boundary_values_check_scalars_once_and_preserve_object_identity() {
        let integer = BoundaryValue::checked_unbox(Value::integer(42), ScalarKind::I64).unwrap();
        assert_eq!(integer.bits(), Some(42));
        assert_eq!(integer.boxed(), Value::integer(42));

        let widened = BoundaryValue::checked_unbox(Value::integer(7), ScalarKind::F64).unwrap();
        assert_eq!(widened.boxed(), Value::float(7.0));
        assert!(BoundaryValue::checked_unbox(Value::boolean(true), ScalarKind::I64).is_err());

        let object = ObjectId::from_raw(1).unwrap();
        let canonical = BoundaryValue::canonical(Value::object(object));
        assert_eq!(canonical.boxed().as_object(), Some(object));
    }
}
