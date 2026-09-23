/// Boxed `any` representation: a 16-byte `{tag: i64, payload: i64}` pair
/// allocated with a precise layout. Reference values keep their original
/// pointer in the payload, preserving identity. Float/bool payloads are
/// bit-reinterpreted, not converted.
use crate::types::Type;

pub const TAG_I64: i64 = 0;
pub const TAG_F64: i64 = 1;
pub const TAG_BOOL: i64 = 2;
pub const TAG_NIL: i64 = 3;
pub const TAG_STRING: i64 = 4;
pub const TAG_REFERENCE_BASE: i64 = 256;

/// Stable structural tag for a boxed type. Scalar tags remain compact for the
/// dynamic arithmetic fast paths; reference tags include their full static
/// shape so `Array<i64>` cannot be unboxed as `Array<f64>`.
pub fn tag_for(ty: &Type) -> Option<i64> {
    match ty {
        Type::I64 => Some(TAG_I64),
        Type::F64 => Some(TAG_F64),
        Type::Bool => Some(TAG_BOOL),
        Type::Nil => Some(TAG_NIL),
        Type::String => Some(TAG_STRING),
        Type::Any => None,
        Type::Array(_) | Type::Map(_, _) | Type::Struct(_) | Type::Function { .. } => {
            Some(TAG_REFERENCE_BASE + stable_type_hash(ty) as i64)
        }
    }
}

fn stable_type_hash(ty: &Type) -> u32 {
    fn byte(hash: &mut u32, value: u8) {
        *hash ^= value as u32;
        *hash = hash.wrapping_mul(16_777_619);
    }
    fn text(hash: &mut u32, value: &str) {
        for value in value.bytes() {
            byte(hash, value);
        }
    }
    fn visit(hash: &mut u32, ty: &Type) {
        match ty {
            Type::I64 => byte(hash, 1),
            Type::F64 => byte(hash, 2),
            Type::Bool => byte(hash, 3),
            Type::Nil => byte(hash, 4),
            Type::String => byte(hash, 5),
            Type::Array(inner) => {
                byte(hash, 6);
                visit(hash, inner);
            }
            Type::Map(key, value) => {
                byte(hash, 7);
                visit(hash, key);
                visit(hash, value);
            }
            Type::Function {
                params,
                return_type,
            } => {
                byte(hash, 8);
                for param in params {
                    visit(hash, param);
                    byte(hash, 0xff);
                }
                visit(hash, return_type);
            }
            Type::Struct(name) => {
                byte(hash, 9);
                text(hash, name);
            }
            Type::Any => byte(hash, 10),
        }
    }
    let mut hash = 2_166_136_261;
    visit(&mut hash, ty);
    hash & 0x7fff_ffff
}
