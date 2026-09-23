//! Gradual scalar slow paths. Typed numeric expressions still lower to plain
//! register operations; only `any` operands reach this module.
use crate::{ast::BinaryOp as Op, numeric, runtime, value::*};

unsafe fn parts(p: *const u64) -> (i64, u64) {
    unsafe { (*p as i64, *p.add(1)) }
}

fn boxed(tag: i64, bits: u64) -> *mut u64 {
    let pointer_mask = u64::from(tag == TAG_STRING) << 1;
    let p = runtime::sol_alloc_layout(16, pointer_mask) as *mut u64;
    unsafe {
        *p = tag as u64;
        *p.add(1) = bits;
    }
    p
}

#[no_mangle]
/// # Safety
/// `p` must point to a live, two-word boxed Sol value.
pub unsafe extern "C" fn __sol_any_is(p: *const u64, expected_tag: i64) -> u8 {
    let (tag, _) = unsafe { parts(p) };
    (tag == expected_tag) as u8
}

fn float(tag: i64, bits: u64) -> f64 {
    match tag {
        TAG_I64 => bits as i64 as f64,
        TAG_F64 => f64::from_bits(bits),
        _ => std::process::abort(),
    }
}

fn integer(tag: i64, bits: u64) -> i64 {
    match tag {
        TAG_I64 => bits as i64,
        TAG_F64 => {
            let f = f64::from_bits(bits);
            if (-9223372036854775808.0..9223372036854775808.0).contains(&f) && f.fract() == 0.0 {
                f as i64
            } else {
                std::process::abort()
            }
        }
        _ => std::process::abort(),
    }
}

#[no_mangle]
/// # Safety
/// `p` must point to a live, two-word boxed Sol value.
pub unsafe extern "C" fn sol_truth(p: *const u64) -> u64 {
    let (tag, bits) = unsafe { parts(p) };
    (tag != TAG_NIL && (tag != TAG_BOOL || bits != 0)) as u64
}

#[no_mangle]
/// # Safety
/// `a` and `b` must point to live, two-word boxed Sol values, and `op` must
/// identify a supported binary operator.
pub unsafe extern "C" fn sol_dynamic_binary(a: *const u64, b: *const u64, op: i64) -> *mut u64 {
    let (at, av) = unsafe { parts(a) };
    let (bt, bv) = unsafe { parts(b) };
    if op == Op::Concat as i64 {
        let string = |tag, bits| match tag {
            TAG_STRING => bits as *const u8,
            TAG_I64 => crate::strings::__sol_string_i64(bits as i64) as *const u8,
            TAG_F64 => crate::strings::__sol_string_f64(f64::from_bits(bits)) as *const u8,
            _ => std::process::abort(),
        };
        let a = string(at, av);
        let b = string(bt, bv);
        return boxed(
            TAG_STRING,
            unsafe { crate::strings::__sol_string_concat(a, b) } as u64,
        );
    }
    let integer_op = matches!(op,x if x==Op::BitAnd as i64 || x==Op::BitOr as i64 || x==Op::BitXor as i64 || x==Op::Shl as i64 || x==Op::Shr as i64);
    if integer_op
        || (at == TAG_I64 && bt == TAG_I64 && op != Op::Div as i64 && op != Op::Pow as i64)
    {
        let a = integer(at, av);
        let b = integer(bt, bv);
        let v = match op {
            x if x == Op::Add as i64 => a.wrapping_add(b),
            x if x == Op::Sub as i64 => a.wrapping_sub(b),
            x if x == Op::Mul as i64 => a.wrapping_mul(b),
            x if x == Op::FloorDiv as i64 => {
                if b == 0 {
                    std::process::abort()
                }
                numeric::floor_div(a, b)
            }
            x if x == Op::Mod as i64 => {
                if b == 0 {
                    std::process::abort()
                }
                numeric::modulo(a, b)
            }
            x if x == Op::BitAnd as i64 => a & b,
            x if x == Op::BitOr as i64 => a | b,
            x if x == Op::BitXor as i64 => a ^ b,
            x if x == Op::Shl as i64 => numeric::shift(a, b, true),
            x if x == Op::Shr as i64 => numeric::shift(a, b, false),
            _ => std::process::abort(),
        };
        boxed(TAG_I64, v as u64)
    } else {
        let a = float(at, av);
        let b = float(bt, bv);
        let v = match op {
            x if x == Op::Add as i64 => a + b,
            x if x == Op::Sub as i64 => a - b,
            x if x == Op::Mul as i64 => a * b,
            x if x == Op::Div as i64 => a / b,
            x if x == Op::FloorDiv as i64 => (a / b).floor(),
            x if x == Op::Mod as i64 => numeric::modulo_float(a, b),
            x if x == Op::Pow as i64 => a.powf(b),
            _ => std::process::abort(),
        };
        boxed(TAG_F64, v.to_bits())
    }
}

#[no_mangle]
/// # Safety
/// `a` and `b` must point to live, two-word boxed Sol values, and `op` must
/// identify a supported comparison operator.
pub unsafe extern "C" fn sol_dynamic_compare(a: *const u64, b: *const u64, op: i64) -> u64 {
    let (at, av) = unsafe { parts(a) };
    let (bt, bv) = unsafe { parts(b) };
    let numeric = matches!(at, TAG_I64 | TAG_F64) && matches!(bt, TAG_I64 | TAG_F64);
    let order = if at == TAG_I64 && bt == TAG_I64 {
        (av as i64).partial_cmp(&(bv as i64))
    } else if numeric {
        float(at, av).partial_cmp(&float(bt, bv))
    } else if at == TAG_STRING && bt == TAG_STRING {
        unsafe {
            crate::strings::bytes(av as *const u8)
                .partial_cmp(crate::strings::bytes(bv as *const u8))
        }
    } else if at == bt {
        av.partial_cmp(&bv)
    } else {
        None
    };
    let eq = if numeric || (at == TAG_STRING && bt == TAG_STRING) {
        order == Some(std::cmp::Ordering::Equal)
    } else {
        at == bt && av == bv
    };
    if !(numeric || at == TAG_STRING && bt == TAG_STRING)
        && op != Op::Eq as i64
        && op != Op::NotEq as i64
    {
        std::process::abort();
    }
    use std::cmp::Ordering::*;
    (match op {
        x if x == Op::Eq as i64 => eq,
        x if x == Op::NotEq as i64 => !eq,
        x if x == Op::Lt as i64 => order == Some(Less),
        x if x == Op::Le as i64 => matches!(order, Some(Less | Equal)),
        x if x == Op::Gt as i64 => order == Some(Greater),
        x if x == Op::Ge as i64 => matches!(order, Some(Greater | Equal)),
        _ => std::process::abort(),
    }) as u64
}

#[no_mangle]
/// # Safety
/// `p` must point to a live, two-word boxed Sol numeric value.
pub unsafe extern "C" fn sol_dynamic_neg(p: *const u64) -> *mut u64 {
    let (tag, bits) = unsafe { parts(p) };
    match tag {
        TAG_I64 => boxed(TAG_I64, (bits as i64).wrapping_neg() as u64),
        TAG_F64 => boxed(TAG_F64, (-f64::from_bits(bits)).to_bits()),
        _ => std::process::abort(),
    }
}

#[no_mangle]
/// # Safety
/// `p` must point to a live, two-word boxed Sol value.
pub unsafe extern "C" fn sol_print_any(p: *const u64) {
    let (tag, bits) = unsafe { parts(p) };
    match tag {
        TAG_I64 => println!("{}", bits as i64),
        TAG_F64 => println!("{}", f64::from_bits(bits)),
        TAG_BOOL => println!("{}", bits != 0),
        TAG_NIL => println!("nil"),
        TAG_STRING => unsafe { crate::strings::sol_print_string(bits as *const u8) },
        tag if tag >= TAG_REFERENCE_BASE => println!("<reference>"),
        _ => std::process::abort(),
    }
}

#[no_mangle]
pub extern "C" fn sol_print_nil(_: i64) {
    println!("nil");
}
