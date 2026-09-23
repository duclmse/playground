//! Misc free-function helpers shared by `dispatch`/`natives`: register-file
//! access (`reg_get`/`reg_set`/...), bytecode constant conversion, numeric
//! coercion/arithmetic helpers (`floor_div`, `shift`, `number_as_f64`, ...),
//! byte-range/plain-substring helpers, and proleptic-Gregorian civil-date
//! conversion (`civil_from_days`/`days_from_civil`) used by `os.date`/`os.time`.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use crate::lua_bytecode::{Const, Proto};

use super::*;

pub(super) fn ensure_regs(regs: &mut Vec<LuaValue>, cells: &mut Cells, len: usize) {
    while regs.len() < len {
        regs.push(LuaValue::Nil);
        cells.push(None);
    }
}

/// Reads register `i`: from its cell if captured, otherwise the plain slot.
pub(super) fn reg_get(regs: &[LuaValue], cells: &[Option<RcRef<LuaValue>>], i: usize) -> LuaValue {
    match &cells[i] {
        Some(cell) => cell.borrow().clone(),
        None => regs[i].clone(),
    }
}

pub(super) fn reg_truthy(regs: &[LuaValue], cells: &[Option<RcRef<LuaValue>>], i: usize) -> bool {
    match &cells[i] {
        Some(cell) => cell.borrow().truthy(),
        None => regs[i].truthy(),
    }
}

/// Writes register `i` in place: through its existing cell if captured,
/// otherwise the plain slot. Use this for ordinary assignments that must be
/// visible through any closure that already captured this register's cell.
pub(super) fn reg_set(
    regs: &mut [LuaValue],
    cells: &[Option<RcRef<LuaValue>>],
    i: usize,
    value: LuaValue,
) {
    match &cells[i] {
        Some(cell) => *cell.borrow_mut() = value,
        None => regs[i] = value,
    }
}

/// Gives register `i` a *fresh* identity: a new cell if it's a captured
/// register (so a previously-created closure keeps its own cell instead of
/// observing this write), or just the plain slot otherwise (there is no
/// aliasing to protect against). Used for `NewLocal`/loop-variable
/// materialization, where a new "declaration" of a local must not retroactively
/// change what an earlier closure captured.
pub(super) fn reg_set_fresh(
    regs: &mut [LuaValue],
    cells: &mut [Option<RcRef<LuaValue>>],
    i: usize,
    value: LuaValue,
) {
    if cells[i].is_some() {
        cells[i] = Some(Rc::new(RefCell::new(value)));
    } else {
        regs[i] = value;
    }
}

/// Fetches a `GetField`/`SetField` name operand's pooled bytes (always a
/// `Const::Str`, checked by the compiler's own emission sites) as a cheap
/// `Rc` clone, for use directly as a `LuaValue::String` table key.
pub(super) fn name_const(proto: &Proto, index: u32) -> Rc<Vec<u8>> {
    match &proto.consts[index as usize] {
        Const::Str(value) => value.clone(),
        _ => unreachable!("GetField/SetField name operand must be a Const::Str"),
    }
}

pub(super) fn const_to_value(value: &Const) -> LuaValue {
    match value {
        Const::Nil => LuaValue::Nil,
        Const::Bool(value) => LuaValue::Bool(*value),
        Const::Integer(value) => LuaValue::Integer(*value),
        Const::Float(value) => LuaValue::Float(*value),
        Const::Str(value) => LuaValue::String(value.clone()),
    }
}

pub(super) fn number_float(left: Number, right: Number) -> (f64, f64) {
    (
        match left {
            Number::Integer(value) => value as f64,
            Number::Float(value) => value,
        },
        match right {
            Number::Integer(value) => value as f64,
            Number::Float(value) => value,
        },
    )
}

pub(super) fn compare_numbers(left: Number, right: Number) -> Option<Ordering> {
    match (left, right) {
        (Number::Integer(left), Number::Integer(right)) => Some(left.cmp(&right)),
        (Number::Float(left), Number::Float(right)) => left.partial_cmp(&right),
        (Number::Integer(integer), Number::Float(float)) => {
            if float.is_nan() {
                None
            } else if float >= -(i64::MIN as f64) {
                Some(Ordering::Less)
            } else if float < i64::MIN as f64 {
                Some(Ordering::Greater)
            } else {
                let floor = float.floor() as i64;
                Some(match integer.cmp(&floor) {
                    Ordering::Equal if float != float.floor() => Ordering::Less,
                    ordering => ordering,
                })
            }
        }
        (Number::Float(left), Number::Integer(right)) => {
            compare_numbers(Number::Integer(right), Number::Float(left)).map(Ordering::reverse)
        }
    }
}

/// Parse the complete contents of a Lua string as a numeral. Lua arithmetic
/// accepts surrounding ASCII whitespace and one leading sign, but it must not
/// accept a valid numeric prefix followed by comments or other source text.
/// Reusing the language lexer here keeps decimal, hexadecimal, and hexadecimal
/// floating-point syntax identical between source literals and coercion.
pub(super) fn parse_lua_number(bytes: &[u8]) -> Option<Number> {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut text = &bytes[start..end];
    let negative = match text.first() {
        Some(b'+') => {
            text = &text[1..];
            false
        }
        Some(b'-') => {
            text = &text[1..];
            true
        }
        _ => false,
    };
    if text.is_empty() {
        return None;
    }
    let tokens = crate::lexer::lex_bytes(text).ok()?;
    let token = tokens.as_slice().first().filter(|_| tokens.len() == 1)?;
    if token.lexeme.as_slice() != text {
        return None;
    }
    match token.token {
        crate::lexer::Token::IntLit(value) => Some(Number::Integer(if negative {
            value.wrapping_neg()
        } else {
            value
        })),
        crate::lexer::Token::FloatLit(value) => {
            // Real Lua's string->number conversion (`l_str2int` in
            // lobject.c) accumulates decimal digits unsigned and only
            // negates at the end, so a negative literal whose magnitude is
            // exactly `-i64::MIN` (one past `i64::MAX`) is still an exact
            // integer - unlike the same digit run without a sign, or as
            // source-code syntax (a separate, unrelated lexer path), both of
            // which overflow to float. Recover that one boundary case
            // instead of losing precision to a float approximation.
            if negative && !text.is_empty() && text.iter().all(u8::is_ascii_digit) {
                if let Ok(magnitude) = std::str::from_utf8(text).unwrap_or_default().parse::<u64>()
                {
                    if magnitude == i64::MIN.unsigned_abs() {
                        return Some(Number::Integer(i64::MIN));
                    }
                }
            }
            Some(Number::Float(if negative { -value } else { value }))
        }
        _ => None,
    }
}

pub(super) fn coerce_number(value: &LuaValue) -> LuaResult<Number> {
    match value {
        LuaValue::Integer(value) => Ok(Number::Integer(*value)),
        LuaValue::Float(value) => Ok(Number::Float(*value)),
        LuaValue::String(value) => {
            parse_lua_number(value).ok_or_else(|| LuaError::new("number expected"))
        }
        _ => Err(LuaError::new("number expected")),
    }
}

pub(super) fn coerce_integer(value: &LuaValue) -> LuaResult<i64> {
    match coerce_number(value)? {
        Number::Integer(value) => Ok(value),
        Number::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && value >= i64::MIN as f64
                && value < -(i64::MIN as f64) =>
        {
            Ok(value as i64)
        }
        Number::Float(_) => Err(LuaError::new("number has no integer representation")),
    }
}

pub(super) fn next_random(state: &mut [u64; 4]) -> u64 {
    let state0 = state[0];
    let state1 = state[1];
    let state2 = state[2] ^ state0;
    let state3 = state[3] ^ state1;
    let result = state1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    state[0] = state0 ^ state3;
    state[1] = state1 ^ state2;
    state[2] = state2 ^ state1.wrapping_shl(17);
    state[3] = state3.rotate_left(45);
    result
}

pub(super) fn seeded_random_state(seed1: u64, seed2: u64) -> [u64; 4] {
    let mut state = [seed1, 0xff, seed2, 0];
    for _ in 0..16 {
        next_random(&mut state);
    }
    state
}

pub(super) fn project_random(mut random: u64, upper: u64, state: &mut [u64; 4]) -> u64 {
    let mut limit = upper;
    let mut shift = 1;
    while limit & limit.wrapping_add(1) != 0 {
        limit |= limit >> shift;
        shift *= 2;
    }
    loop {
        random &= limit;
        if random <= upper {
            return random;
        }
        random = next_random(state);
    }
}

pub(super) fn frexp(value: f64) -> (f64, i64) {
    if value == 0.0 || !value.is_finite() {
        return (value, 0);
    }
    let mut normalized = value;
    let mut adjustment = 0i64;
    let mut bits = normalized.to_bits();
    let mut exponent = ((bits >> 52) & 0x7ff) as i64;
    if exponent == 0 {
        normalized *= 2f64.powi(54);
        adjustment = -54;
        bits = normalized.to_bits();
        exponent = ((bits >> 52) & 0x7ff) as i64;
    }
    let fraction = f64::from_bits((bits & ((1u64 << 63) | ((1u64 << 52) - 1))) | (1022u64 << 52));
    (fraction, exponent - 1022 + adjustment)
}

pub(super) fn ldexp(mut value: f64, mut exponent: i64) -> f64 {
    // Apply the exponent in representable chunks so subnormal results do not
    // underflow prematurely and oversized Lua integers never narrow to i32.
    while exponent > 1023 {
        value *= 2f64.powi(1023);
        exponent -= 1023;
        if !value.is_finite() {
            return value;
        }
    }
    while exponent < -1022 {
        value *= 2f64.powi(-1022);
        exponent += 1022;
        if value == 0.0 {
            return value;
        }
    }
    value * 2f64.powi(exponent as i32)
}

pub(super) fn utf8_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// Decode Lua 5.5's extended UTF-8 format. In lax mode Lua accepts codepoints
/// through 0x7fffffff (up to six-byte sequences); strict mode additionally
/// rejects surrogates and values above Unicode's 0x10ffff ceiling.
pub(super) fn decode_lua_utf8(bytes: &[u8], start: usize, strict: bool) -> Option<(u32, usize)> {
    let first = *bytes.get(start)?;
    if first < 0x80 {
        return Some((first as u32, start + 1));
    }
    if first >= 0xfe {
        return None;
    }
    let count = first.leading_ones() as usize;
    if !(2..=6).contains(&count) {
        return None;
    }
    let continuation_count = count - 1;
    let mut result = (first & (0x7f >> count)) as u32;
    for index in 1..=continuation_count {
        let byte = *bytes.get(start + index)?;
        if !utf8_continuation(byte) {
            return None;
        }
        result = (result << 6) | (byte & 0x3f) as u32;
    }
    const MINIMUM: [u32; 7] = [0, 0, 0x80, 0x800, 0x1_0000, 0x20_0000, 0x400_0000];
    if result < MINIMUM[count] || result > 0x7fff_ffff {
        return None;
    }
    if strict && (result > 0x10_ffff || (0xd800..=0xdfff).contains(&result)) {
        return None;
    }
    Some((result, start + count))
}

pub(super) fn encode_lua_utf8(codepoint: u32, output: &mut Vec<u8>) -> bool {
    if codepoint > 0x7fff_ffff {
        return false;
    }
    let count = if codepoint < 0x80 {
        1
    } else if codepoint < 0x800 {
        2
    } else if codepoint < 0x1_0000 {
        3
    } else if codepoint < 0x20_0000 {
        4
    } else if codepoint < 0x400_0000 {
        5
    } else {
        6
    };
    if count == 1 {
        output.push(codepoint as u8);
        return true;
    }
    let first_index = output.len();
    output.resize(first_index + count, 0);
    let mut remaining = codepoint;
    for index in (1..count).rev() {
        output[first_index + index] = 0x80 | (remaining as u8 & 0x3f);
        remaining >>= 6;
    }
    output[first_index] = (!0u8 << (8 - count)) | remaining as u8;
    true
}

pub(super) fn relative_position(position: i64, length: usize) -> i64 {
    if position >= 0 {
        position
    } else if position.unsigned_abs() > length as u64 {
        0
    } else {
        length as i64 + position + 1
    }
}

pub(super) fn floor_div(a: i64, b: i64) -> LuaResult<i64> {
    if b == 0 {
        Err(LuaError::new("attempt to divide by zero"))
    } else if a == i64::MIN && b == -1 {
        Ok(i64::MIN)
    } else {
        Ok(crate::numeric::floor_div(a, b))
    }
}

pub(super) fn floor_mod(a: i64, b: i64) -> LuaResult<i64> {
    if b == 0 {
        // Lua distinguishes integer remainder from floor division in its
        // diagnostic (`n%0` versus "divide by zero").
        Err(LuaError::new("attempt to perform 'n%0'"))
    } else if a == i64::MIN && b == -1 {
        Ok(0)
    } else {
        Ok(crate::numeric::modulo(a, b))
    }
}

pub(super) fn shift(value: i64, amount: i64, left: bool) -> i64 {
    crate::numeric::shift(value, amount, left)
}

pub(super) fn byte_range(start: i64, end: i64, length: usize) -> Option<(usize, usize)> {
    let length = i64::try_from(length).ok()?;
    let relative = |index: i64| {
        if index < 0 {
            length.saturating_add(index).saturating_add(1)
        } else {
            index
        }
    };
    let start = relative(start).max(1);
    let end = relative(end).min(length);
    if start > end || start > length || end < 1 {
        None
    } else {
        Some(((start - 1) as usize, (end - 1) as usize))
    }
}

/// Mirrors `lstrlib.c`'s `posrelat` + range check for `string.find`/`match`/
/// `gmatch`'s optional `init` argument: converts a 1-based (possibly
/// negative) position into a 0-based search offset, or `None` if `init`
/// starts past the end of the subject (the caller should return `nil`).
pub(super) fn resolve_init(init: Option<i64>, length: usize) -> Option<usize> {
    let pos = init.unwrap_or(1);
    let relative = if pos >= 0 {
        pos
    } else if pos.unsigned_abs() as usize > length {
        0
    } else {
        length as i64 + pos + 1
    };
    let relative = relative.max(1);
    if relative > length as i64 + 1 {
        None
    } else {
        Some((relative - 1) as usize)
    }
}

pub(super) fn find_plain(source: &[u8], pattern: &[u8], init: usize) -> Option<(usize, usize)> {
    if pattern.is_empty() {
        return Some((init, init));
    }
    if init > source.len() {
        return None;
    }
    source[init..]
        .windows(pattern.len())
        .position(|window| window == pattern)
        .map(|offset| (init + offset, init + offset + pattern.len()))
}

pub(super) fn number_as_f64(value: &LuaValue) -> LuaResult<f64> {
    Ok(match coerce_number(value)? {
        Number::Integer(value) => value as f64,
        Number::Float(value) => value,
    })
}

/// Converts a numeric `for` loop's float limit into the fixed integer limit
/// real Lua's `forlimit` computes when the loop's init/step are already
/// integers but the limit is a float (e.g. `for i = 1, 10.9 do`): the whole
/// loop still runs as an integer loop, with the limit rounded toward the
/// loop (floor when ascending, ceil when descending) and clamped to
/// `i64::MAX`/`i64::MIN` when the float is out of `i64` range - it does not
/// force the loop into floats just because the limit happened to be
/// written as one. Returns `None` when no integer value could possibly
/// satisfy the loop condition (the limit is NaN, or is out of range on the
/// side that makes the loop trivially empty), meaning the loop must not run
/// at all.
pub(super) fn float_for_limit(limit: f64, ascending: bool) -> Option<i64> {
    if limit.is_nan() {
        return None;
    }
    let rounded = if ascending {
        limit.floor()
    } else {
        limit.ceil()
    };
    const MIN_F: f64 = -9223372036854775808.0; // i64::MIN, exact in f64
    const BOUND_F: f64 = 9223372036854775808.0; // one past i64::MAX, exact in f64
    if rounded >= MIN_F && rounded < BOUND_F {
        Some(rounded as i64)
    } else if limit > 0.0 {
        if ascending {
            Some(i64::MAX)
        } else {
            None
        }
    } else if ascending {
        None
    } else {
        Some(i64::MIN)
    }
}

/// Days-since-epoch (1970-01-01) -> (year, month `1..=12`, day `1..=31`),
/// proleptic Gregorian. Howard Hinnant's public-domain `civil_from_days`
/// algorithm: avoids pulling in a date/time crate for `os.date`'s small
/// calendar-math need. Correct for the full `i64` range representable as a
/// Unix timestamp; UTC only (this runtime has no timezone database, so
/// `os.date` and `os.date("!*t")` currently render identically).
pub(super) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of `civil_from_days`: (year, month, day) -> days-since-epoch.
pub(super) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Replaces every non-overlapping occurrence of `from` in `data` with `to`,
/// byte-for-byte (no pattern matching) - used by `package.searchpath`/
/// `require`'s Lua-5.5-compatible template substitution (`?` -> module
/// name, `sep` -> `dirsep`), which is always a plain literal substitution in
/// real Lua's own `lauxlib.c` (`luaL_gsub`), never a Lua pattern. Returns
/// `data` unchanged if `from` is empty, matching `luaL_gsub`'s own no-op on
/// an empty search string rather than looping forever.
pub(super) fn bytes_replace_all(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut rest = data;
    while let Some(pos) = rest.windows(from.len()).position(|window| window == from) {
        out.extend_from_slice(&rest[..pos]);
        out.extend_from_slice(to);
        rest = &rest[pos + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// `package.config`'s exact value: the platform directory separator, the
/// template-path separator, the wildcard mark, the executable-directory
/// mark, and the "ignore the rest of the name" mark, each on its own line -
/// real Lua 5.5's `loadlib.c` builds this from its own compiled-in
/// `LUA_DIRSEP`/`LUA_PATH_SEP`/`LUA_PATH_MARK`/`LUA_EXECDIR`/`LUA_IGMARK`
/// constants (verified against a pinned `lua5.5.1` build: `"/\n;\n?\n!\n-\n"`
/// on this platform). Sol has no execdir/igmark behavior of its own (no
/// dynamic-library loader - see `require`'s doc comment), but the constant
/// is still published verbatim since scripts parse it structurally (e.g.
/// `attrib.lua` reads its first line for the directory separator) rather
/// than acting on the execdir/igmark marks themselves.
pub(super) fn package_config_bytes() -> Vec<u8> {
    format!("{}\n;\n?\n!\n-\n", std::path::MAIN_SEPARATOR).into_bytes()
}

/// `package.searchpath`'s default `dirsep` argument (real Lua's compiled-in
/// `LUA_DIRSEP`) - the same platform separator published as `package.config`'s
/// first line.
pub(super) fn default_dirsep() -> Vec<u8> {
    std::path::MAIN_SEPARATOR.to_string().into_bytes()
}

/// Converts a Lua byte string (a candidate file path built by
/// `search_path_candidates`) into a real filesystem path, for the
/// `filesystem`-capability-gated existence probe backing
/// `package.searchpath`. Byte-oriented on unix (matches this crate's
/// byte-oriented source/string handling - see `AGENTS.md`); falls back to
/// requiring valid UTF-8 on other platforms, where `OsStr` has no public
/// from-bytes constructor. A candidate that isn't valid UTF-8 on such a
/// platform is conservatively treated as unreadable rather than panicking.
pub(super) fn bytes_to_path(bytes: &[u8]) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .ok()
            .map(std::path::PathBuf::from)
    }
}
