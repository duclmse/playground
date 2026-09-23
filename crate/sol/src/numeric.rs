//! Lua numeric rules shared by constant folding and tier 0. Native integer
//! operations lower directly to SSA; floating remainder/power use these helpers.
pub fn floor_div(a: i64, b: i64) -> i64 {
    let q = a.wrapping_div(b);
    let r = a.wrapping_rem(b);
    if r != 0 && (r ^ b) < 0 {
        q.wrapping_sub(1)
    } else {
        q
    }
}

pub fn modulo(a: i64, b: i64) -> i64 {
    let r = a.wrapping_rem(b);
    if r != 0 && (r ^ b) < 0 {
        r.wrapping_add(b)
    } else {
        r
    }
}

pub fn modulo_float(a: f64, b: f64) -> f64 {
    let r = a % b;
    if (r > 0.0 && b < 0.0) || (r < 0.0 && b > 0.0) {
        r + b
    } else {
        r
    }
}

pub fn shift(a: i64, b: i64, left: bool) -> i64 {
    let count = b.unsigned_abs();
    if count >= 64 {
        0
    } else if left != (b < 0) {
        ((a as u64) << count) as i64
    } else {
        ((a as u64) >> count) as i64
    }
}

#[no_mangle]
pub extern "C" fn sol_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

#[no_mangle]
pub extern "C" fn sol_mod_float(a: f64, b: f64) -> f64 {
    modulo_float(a, b)
}
