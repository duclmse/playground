//! Immutable byte strings: `[length: u64][bytes...]`. Typed strings use a
//! single pointer in SSA, not boxed Values. Runtime allocations are atomic
//! GC blocks; literals live in the owning bytecode/native module.
use crate::{gc, types::Type};
use std::io::Write;

/// # Safety
/// `p` must point to a live sol string allocation or literal.
pub unsafe fn bytes<'a>(p: *const u8) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(p.add(8), *(p as *const u64) as usize) }
}

fn alloc(s: &[u8]) -> *mut u8 {
    let size = s
        .len()
        .checked_add(8)
        .and_then(|n| i64::try_from(n).ok())
        .expect("string too large");
    let p = gc::sol_gc_alloc_atomic(size);
    unsafe {
        *(p as *mut u64) = s.len() as u64;
        std::ptr::copy_nonoverlapping(s.as_ptr(), p.add(8), s.len());
    }
    p
}

pub fn literal(s: &[u8]) -> Box<[u64]> {
    let mut words = vec![0u64; 1 + s.len().div_ceil(8)].into_boxed_slice();
    words[0] = s.len() as u64;
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), words.as_mut_ptr().add(1) as *mut u8, s.len());
    }
    words
}

#[no_mangle]
/// # Safety
/// `a` and `b` must point to live Sol string allocations or literals.
pub unsafe extern "C" fn __sol_string_concat(a: *const u8, b: *const u8) -> *mut u8 {
    let mut s = unsafe { bytes(a) }.to_vec();
    s.extend_from_slice(unsafe { bytes(b) });
    alloc(&s)
}

#[no_mangle]
pub extern "C" fn __sol_string_i64(n: i64) -> *mut u8 {
    alloc(n.to_string().as_bytes())
}

#[no_mangle]
pub extern "C" fn __sol_string_f64(n: f64) -> *mut u8 {
    let mut s = n.to_string();
    if n.is_finite() && !s.contains(['.', 'e', 'E']) {
        s.push_str(".0");
    }
    alloc(s.as_bytes())
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn __sol_string_len(p: *const u8) -> i64 {
    unsafe { bytes(p).len() as i64 }
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn __sol_string_lower(p: *const u8) -> *mut u8 {
    alloc(&unsafe { bytes(p) }.to_ascii_lowercase())
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn __sol_string_upper(p: *const u8) -> *mut u8 {
    alloc(&unsafe { bytes(p) }.to_ascii_uppercase())
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn __sol_string_reverse(p: *const u8) -> *mut u8 {
    let mut s = unsafe { bytes(p) }.to_vec();
    s.reverse();
    alloc(&s)
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn __sol_string_sub(p: *const u8, i: i64, j: i64) -> *mut u8 {
    let s = unsafe { bytes(p) };
    let len = s.len() as i64;
    let index = |n: i64| {
        if n < 0 {
            len.saturating_add(n).saturating_add(1)
        } else {
            n
        }
    };
    let i = index(i).max(1);
    let j = index(j).min(len);
    if i > j {
        alloc(&[])
    } else {
        alloc(&s[(i - 1) as usize..j as usize])
    }
}

#[no_mangle]
/// # Safety
/// `p` and `sep` must point to live Sol string allocations or literals.
pub unsafe extern "C" fn __sol_string_rep(p: *const u8, n: i64, sep: *const u8) -> *mut u8 {
    if n <= 0 {
        return alloc(&[]);
    }
    let s = unsafe { bytes(p) };
    let sep = unsafe { bytes(sep) };
    let n = n as usize;
    let len = s
        .len()
        .checked_mul(n)
        .and_then(|v| sep.len().checked_mul(n - 1).and_then(|x| v.checked_add(x)))
        .expect("string too large");
    let mut out = Vec::with_capacity(len);
    for i in 0..n {
        if i > 0 {
            out.extend_from_slice(sep);
        }
        out.extend_from_slice(s);
    }
    alloc(&out)
}

#[no_mangle]
/// # Safety
/// `a` and `b` must point to live Sol string allocations or literals.
pub unsafe extern "C" fn sol_string_compare(a: *const u8, b: *const u8) -> i64 {
    match unsafe { bytes(a).cmp(bytes(b)) } {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[no_mangle]
/// # Safety
/// `p` must point to a live Sol string allocation or literal.
pub unsafe extern "C" fn sol_print_string(p: *const u8) {
    let mut out = std::io::stdout().lock();
    out.write_all(unsafe { bytes(p) }).unwrap();
    out.write_all(b"\n").unwrap();
}

pub fn signatures() -> Vec<(&'static str, Vec<Type>, Type)> {
    use Type::*;
    vec![
        ("__sol_string_concat", vec![String, String], String),
        ("__sol_string_i64", vec![I64], String),
        ("__sol_string_f64", vec![F64], String),
        ("__sol_string_len", vec![String], I64),
        ("__sol_string_lower", vec![String], String),
        ("__sol_string_upper", vec![String], String),
        ("__sol_string_reverse", vec![String], String),
        ("__sol_string_sub", vec![String, I64, I64], String),
        ("__sol_string_rep", vec![String, I64, String], String),
    ]
}

pub fn signature(name: &str) -> Option<(&'static str, Vec<Type>, Type)> {
    let suffix = name.strip_prefix("string.")?;
    signatures()
        .into_iter()
        .find(|(symbol, _, _)| symbol.strip_prefix("__sol_string_") == Some(suffix))
}

#[cfg(feature = "jit")]
pub fn register(builder: &mut cranelift_jit::JITBuilder) {
    builder.symbol("__sol_string_concat", __sol_string_concat as *const u8);
    builder.symbol("__sol_string_i64", __sol_string_i64 as *const u8);
    builder.symbol("__sol_string_f64", __sol_string_f64 as *const u8);
    builder.symbol("__sol_string_len", __sol_string_len as *const u8);
    builder.symbol("__sol_string_lower", __sol_string_lower as *const u8);
    builder.symbol("__sol_string_upper", __sol_string_upper as *const u8);
    builder.symbol("__sol_string_reverse", __sol_string_reverse as *const u8);
    builder.symbol("__sol_string_sub", __sol_string_sub as *const u8);
    builder.symbol("__sol_string_rep", __sol_string_rep as *const u8);
    builder.symbol("sol_string_compare", sol_string_compare as *const u8);
}
