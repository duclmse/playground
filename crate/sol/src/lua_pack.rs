//! Lua 5.5's `string.pack`/`string.unpack`/`string.packsize` binary format
//! mini-language.
//!
//! This mirrors the Lua manual's format-string options directly (rather than
//! being a general-purpose serialization format) so packed byte layouts match
//! a real Lua build on the same option set. Operates on raw bytes and a small
//! `PackValue` enum instead of `lua_runtime.rs`'s `LuaValue` directly,
//! mirroring `lua_pattern.rs`'s standalone-engine style; `lua_runtime.rs`
//! converts at the boundary.
//!
//! Supported options: endianness `<`/`>`/`=`, alignment `!`/`!n`, integers
//! `b`/`B`/`h`/`H`/`l`/`L`/`j`/`J`/`T` (fixed native sizes) and `i`/`I`
//! (default 4 bytes, or `in`/`In` for an explicit 1..=8 byte size), floats
//! `f`/`d`/`n`, strings `s`/`sn` (length-prefixed), `z` (zero-terminated),
//! `cn` (fixed-size, `n` required), padding `x`, the no-op ` ` (space), and
//! `Xop` (align-without-storing before the following fixed-size option).

type PResult<T> = Result<T, String>;

/// One packed/unpacked value. `lua_runtime.rs` maps this to/from `LuaValue`
/// at the call sites (`string.pack`/`unpack`/`packsize`'s native functions).
#[derive(Clone, Debug, PartialEq)]
pub enum PackValue {
    Int(i64),
    Num(f64),
    Str(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq)]
enum Endian {
    Little,
    Big,
}

fn native_endian() -> Endian {
    if cfg!(target_endian = "big") {
        Endian::Big
    } else {
        Endian::Little
    }
}

struct FormatState {
    endian: Endian,
    max_align: usize,
}

impl Default for FormatState {
    fn default() -> Self {
        Self {
            endian: native_endian(),
            max_align: 1,
        }
    }
}

/// Reads an optional decimal size suffix (e.g. the `4` in `i4`) starting at
/// `*pos`, advancing past it. Returns `default` if no digits are present.
fn read_size(format: &[u8], pos: &mut usize, default: usize) -> PResult<usize> {
    let start = *pos;
    let mut value = 0usize;
    while let Some(digit) = format.get(*pos).filter(|digit| digit.is_ascii_digit()) {
        let digit = (digit - b'0') as usize;
        let Some(next) = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
            .filter(|value| *value <= i64::MAX as usize)
        else {
            // Lua's format parser stops consuming the numeric suffix when
            // its host size type overflows. The still-unconsumed digit is
            // then diagnosed by the outer parser as an invalid option.
            break;
        };
        value = next;
        *pos += 1;
    }
    if *pos == start {
        return Ok(default);
    }
    Ok(value)
}

/// Natural (unaligned) byte size of a fixed-size option. `size_hint` is the
/// already-parsed numeric suffix for `i`/`I`/`c` (for options where the size
/// is fixed regardless of a suffix, `size_hint` is ignored).
fn option_size(opt: u8, size_hint: usize) -> PResult<usize> {
    Ok(match opt {
        b'b' | b'B' | b'x' => 1,
        b'h' | b'H' => 2,
        b'i' | b'I' => size_hint,
        b'l' | b'L' | b'j' | b'J' | b'T' => 8,
        b'f' => 4,
        b'd' | b'n' => 8,
        b'c' => size_hint,
        _ => return Err(format!("invalid format option '{}'", opt as char)),
    })
}

fn check_int_size(opt: u8, size: usize) -> PResult<()> {
    if size == 0 || size > 16 {
        return Err(format!(
            "integer size ({size}) out of limits [1,16] for format option '{}'",
            opt as char
        ));
    }
    Ok(())
}

fn check_max_align(size: usize) -> PResult<()> {
    if size == 0 || size > 16 {
        return Err(format!("alignment {size} out of limits [1,16]"));
    }
    if !size.is_power_of_two() {
        return Err(format!("alignment {size} is not power of 2"));
    }
    Ok(())
}

fn check_option_alignment(size: usize, state: &FormatState) -> PResult<()> {
    let align = size.min(state.max_align);
    if !align.is_power_of_two() {
        return Err(format!("alignment {align} is not power of 2"));
    }
    Ok(())
}

fn add_format_size(total: &mut usize, amount: usize) -> PResult<()> {
    let next = total
        .checked_add(amount)
        .ok_or_else(|| "format size too large".to_string())?;
    // `string.packsize` returns a Lua integer, so a format larger than the
    // signed Lua-integer range is unrepresentable even on a 64-bit host.
    if next > i64::MAX as usize {
        return Err("format size too large".to_string());
    }
    *total = next;
    Ok(())
}

fn align_pad(out: &mut Vec<u8>, natural_size: usize, state: &FormatState) {
    let align = natural_size.min(state.max_align).max(1);
    let pad = (align - (out.len() % align)) % align;
    out.extend(std::iter::repeat_n(0u8, pad));
}

fn align_skip(offset: usize, natural_size: usize, state: &FormatState) -> usize {
    let align = natural_size.min(state.max_align).max(1);
    (align - (offset % align)) % align
}

/// Consumes the option immediately following `X` and returns its natural
/// alignment. The following option contributes no value or bytes of its own;
/// whitespace, variable-size, and control options are invalid here.
fn x_alignment(format: &[u8], pos: &mut usize, state: &FormatState) -> PResult<usize> {
    let mut probe = *pos;
    let opt = *format
        .get(probe)
        .ok_or_else(|| "invalid next option for option 'X'".to_string())?;
    probe += 1;
    let size = match opt {
        b'i' | b'I' => {
            let size = read_size(format, &mut probe, 4)?;
            check_int_size(opt, size)?;
            size
        }
        b'b' | b'B' | b'h' | b'H' | b'l' | b'L' | b'j' | b'J' | b'T' | b'f' | b'd' | b'n' => {
            option_size(opt, 0)?
        }
        _ => return Err("invalid next option for option 'X'".to_string()),
    };
    *pos = probe;
    Ok(size.min(state.max_align).max(1))
}

fn write_int_bytes(out: &mut Vec<u8>, value: i64, size: usize, endian: Endian) {
    let mut bytes = [if value < 0 { 0xff } else { 0 }; 16];
    for (i, byte) in bytes.iter_mut().enumerate().take(size.min(8)) {
        let bits = value as u64;
        *byte = ((bits >> (8 * i)) & 0xff) as u8;
    }
    match endian {
        Endian::Little => out.extend_from_slice(&bytes[..size]),
        Endian::Big => out.extend(bytes[..size].iter().rev()),
    }
}

fn read_int_bytes(
    data: &[u8],
    offset: usize,
    size: usize,
    endian: Endian,
    signed: bool,
) -> PResult<i64> {
    if offset + size > data.len() {
        return Err("data string too short".to_string());
    }
    if size > 8 {
        let extension = if signed
            && match endian {
                Endian::Little => data[offset + 7] & 0x80 != 0,
                Endian::Big => data[offset + size - 8] & 0x80 != 0,
            } {
            0xff
        } else {
            0
        };
        let extra = match endian {
            Endian::Little => &data[offset + 8..offset + size],
            Endian::Big => &data[offset..offset + size - 8],
        };
        if extra.iter().any(|byte| *byte != extension) {
            return Err(if size == 16 {
                "16-byte integer does not fit into Lua integer".to_string()
            } else {
                "integer does not fit into Lua integer".to_string()
            });
        }
    }
    let mut bytes = [0u8; 8];
    let slice = &data[offset..offset + size];
    match endian {
        Endian::Little => bytes[..size.min(8)].copy_from_slice(&slice[..size.min(8)]),
        Endian::Big => {
            let slice = &slice[size.saturating_sub(8)..];
            for (i, byte) in slice.iter().rev().enumerate() {
                bytes[i] = *byte;
            }
        }
    }
    let mut value: u64 = 0;
    for (i, byte) in bytes.iter().enumerate().take(size.min(8)) {
        value |= (*byte as u64) << (8 * i);
    }
    if signed && size < 8 {
        let sign_bit = 1u64 << (size * 8 - 1);
        if value & sign_bit != 0 {
            value |= !0u64 << (size * 8);
        }
    }
    Ok(value as i64)
}

fn range_check(opt: u8, value: i64, size: usize, signed: bool) -> PResult<()> {
    if size >= 8 {
        return Ok(());
    }
    let value = value as i128;
    if signed {
        let limit = 1i128 << (size * 8 - 1);
        if value < -limit || value >= limit {
            return Err(format!(
                "integer overflow for format option '{}'",
                opt as char
            ));
        }
    } else {
        let limit = 1i128 << (size * 8);
        if value < 0 || value >= limit {
            if opt == b's' {
                return Err("string length does not fit in given size".to_string());
            }
            return Err(format!(
                "unsigned overflow for format option '{}'",
                opt as char
            ));
        }
    }
    Ok(())
}

/// Computes the total byte size of a fixed-size format string (no `s`/`z`,
/// which have no fixed size). Matches `string.packsize`'s own restriction.
pub fn packsize(format: &[u8]) -> PResult<usize> {
    let mut state = FormatState::default();
    let mut pos = 0;
    let mut total = 0usize;
    while pos < format.len() {
        let opt = format[pos];
        pos += 1;
        match opt {
            b'<' => state.endian = Endian::Little,
            b'>' => state.endian = Endian::Big,
            b'=' => state.endian = native_endian(),
            b' ' => {}
            b'!' => {
                state.max_align = read_size(format, &mut pos, 8)?;
                check_max_align(state.max_align)?;
            }
            b's' | b'z' => {
                return Err(format!(
                    "variable-length format in packsize (option '{}')",
                    opt as char
                ))
            }
            b'X' => {
                let padding = align_skip(total, x_alignment(format, &mut pos, &state)?, &state);
                add_format_size(&mut total, padding)?;
            }
            b'i' | b'I' => {
                let size = read_size(format, &mut pos, 4)?;
                check_int_size(opt, size)?;
                check_option_alignment(size, &state)?;
                let field_size = align_skip(total, size, &state) + size;
                add_format_size(&mut total, field_size)?;
            }
            b'c' => {
                let size_start = pos;
                let size = read_size(format, &mut pos, 0)?;
                if pos == size_start {
                    return Err("missing size for format option 'c'".to_string());
                }
                add_format_size(&mut total, size)?;
            }
            _ => {
                let size = option_size(opt, 0)?;
                let field_size = align_skip(total, size, &state) + size;
                add_format_size(&mut total, field_size)?;
            }
        }
    }
    Ok(total)
}

pub fn pack(format: &[u8], values: &[PackValue]) -> PResult<Vec<u8>> {
    let mut state = FormatState::default();
    let mut out = Vec::new();
    let mut pos = 0;
    let mut value_idx = 0;

    let next_int = |values: &[PackValue], idx: &mut usize, opt: u8| -> PResult<i64> {
        let value = values.get(*idx).ok_or_else(|| {
            format!(
                "bad argument to 'pack' (no value for format option '{}')",
                opt as char
            )
        })?;
        *idx += 1;
        match value {
            PackValue::Int(n) => Ok(*n),
            PackValue::Num(f) if f.fract() == 0.0 => Ok(*f as i64),
            _ => Err(format!(
                "bad argument to 'pack' (number expected for format option '{}')",
                opt as char
            )),
        }
    };

    while pos < format.len() {
        let opt = format[pos];
        pos += 1;
        match opt {
            b'<' => state.endian = Endian::Little,
            b'>' => state.endian = Endian::Big,
            b'=' => state.endian = native_endian(),
            b' ' => {}
            b'!' => {
                state.max_align = read_size(format, &mut pos, 8)?;
                check_max_align(state.max_align)?;
            }
            b'x' => {
                out.push(0);
            }
            b'X' => {
                let size = x_alignment(format, &mut pos, &state)?;
                align_pad(&mut out, size, &state);
            }
            b'b' | b'B' | b'h' | b'H' | b'l' | b'L' | b'j' | b'J' | b'T' => {
                let size = option_size(opt, 0)?;
                let signed = opt.is_ascii_lowercase();
                let value = next_int(values, &mut value_idx, opt)?;
                range_check(opt, value, size, signed)?;
                align_pad(&mut out, size, &state);
                write_int_bytes(&mut out, value, size, state.endian);
            }
            b'i' | b'I' => {
                let size = read_size(format, &mut pos, 4)?;
                check_int_size(opt, size)?;
                check_option_alignment(size, &state)?;
                let signed = opt == b'i';
                let value = next_int(values, &mut value_idx, opt)?;
                range_check(opt, value, size, signed)?;
                align_pad(&mut out, size, &state);
                write_int_bytes(&mut out, value, size, state.endian);
            }
            b'f' => {
                let value = values.get(value_idx).ok_or_else(|| {
                    "bad argument to 'pack' (no value for format option 'f')".to_string()
                })?;
                value_idx += 1;
                let f = match value {
                    PackValue::Int(n) => *n as f32,
                    PackValue::Num(n) => *n as f32,
                    _ => {
                        return Err(
                            "bad argument to 'pack' (number expected for format option 'f')"
                                .to_string(),
                        )
                    }
                };
                align_pad(&mut out, 4, &state);
                let bytes = match state.endian {
                    Endian::Little => f.to_le_bytes(),
                    Endian::Big => f.to_be_bytes(),
                };
                out.extend_from_slice(&bytes);
            }
            b'd' | b'n' => {
                let value = values.get(value_idx).ok_or_else(|| {
                    format!(
                        "bad argument to 'pack' (no value for format option '{}')",
                        opt as char
                    )
                })?;
                value_idx += 1;
                let f = match value {
                    PackValue::Int(n) => *n as f64,
                    PackValue::Num(n) => *n,
                    _ => {
                        return Err(format!(
                            "bad argument to 'pack' (number expected for format option '{}')",
                            opt as char
                        ))
                    }
                };
                align_pad(&mut out, 8, &state);
                let bytes = match state.endian {
                    Endian::Little => f.to_le_bytes(),
                    Endian::Big => f.to_be_bytes(),
                };
                out.extend_from_slice(&bytes);
            }
            b's' => {
                let size = read_size(format, &mut pos, 8)?;
                check_int_size(b's', size)?;
                let value = values.get(value_idx).ok_or_else(|| {
                    "bad argument to 'pack' (no value for format option 's')".to_string()
                })?;
                value_idx += 1;
                let bytes = match value {
                    PackValue::Str(bytes) => bytes.clone(),
                    _ => {
                        return Err(
                            "bad argument to 'pack' (string expected for format option 's')"
                                .to_string(),
                        )
                    }
                };
                range_check(b's', bytes.len() as i64, size, false)?;
                align_pad(&mut out, size, &state);
                write_int_bytes(&mut out, bytes.len() as i64, size, state.endian);
                out.extend_from_slice(&bytes);
            }
            b'z' => {
                let value = values.get(value_idx).ok_or_else(|| {
                    "bad argument to 'pack' (no value for format option 'z')".to_string()
                })?;
                value_idx += 1;
                let bytes = match value {
                    PackValue::Str(bytes) => bytes.clone(),
                    _ => {
                        return Err(
                            "bad argument to 'pack' (string expected for format option 'z')"
                                .to_string(),
                        )
                    }
                };
                if bytes.contains(&0) {
                    return Err("string contains zeros".to_string());
                }
                out.extend_from_slice(&bytes);
                out.push(0);
            }
            b'c' => {
                let size_start = pos;
                let size = read_size(format, &mut pos, 0)?;
                if pos == size_start {
                    return Err("missing size for format option 'c'".to_string());
                }
                // Refuse a format that could not be materialized before
                // looking for its value argument. Lua reports this as a
                // format-length error (not a missing-value error), and this
                // also prevents an attacker-controlled format from asking
                // Rust to reserve an impractically large buffer.
                const MAX_PACKED_BYTES: usize = 64 * 1024 * 1024;
                if size > MAX_PACKED_BYTES || out.len().saturating_add(size) > MAX_PACKED_BYTES {
                    return Err("format result too long".to_string());
                }
                let value = values.get(value_idx).ok_or_else(|| {
                    "bad argument to 'pack' (no value for format option 'c')".to_string()
                })?;
                value_idx += 1;
                let bytes = match value {
                    PackValue::Str(bytes) => bytes.clone(),
                    _ => {
                        return Err(
                            "bad argument to 'pack' (string expected for format option 'c')"
                                .to_string(),
                        )
                    }
                };
                if bytes.len() > size {
                    return Err("string longer than given size".to_string());
                }
                out.extend_from_slice(&bytes);
                out.extend(std::iter::repeat_n(0u8, size - bytes.len()));
            }
            _ => return Err(format!("invalid format option '{}'", opt as char)),
        }
    }
    Ok(out)
}

/// Unpacks `format` from `data` starting at 0-based byte offset `start`.
/// Returns the unpacked values plus the next 0-based offset (mirroring
/// `lua_pattern::find`'s 0-based-offset convention; `lua_runtime.rs`
/// converts to/from Lua's 1-based positions at the call site).
pub fn unpack(format: &[u8], data: &[u8], start: usize) -> PResult<(Vec<PackValue>, usize)> {
    let mut state = FormatState::default();
    let mut pos = 0;
    let mut offset = start;
    let mut results = Vec::new();

    while pos < format.len() {
        let opt = format[pos];
        pos += 1;
        match opt {
            b'<' => state.endian = Endian::Little,
            b'>' => state.endian = Endian::Big,
            b'=' => state.endian = native_endian(),
            b' ' => {}
            b'!' => {
                state.max_align = read_size(format, &mut pos, 8)?;
                check_max_align(state.max_align)?;
            }
            b'x' => {
                if offset >= data.len() {
                    return Err("data string too short".to_string());
                }
                offset += 1;
            }
            b'X' => {
                offset += align_skip(offset, x_alignment(format, &mut pos, &state)?, &state);
            }
            b'b' | b'B' | b'h' | b'H' | b'l' | b'L' | b'j' | b'J' | b'T' => {
                let size = option_size(opt, 0)?;
                let signed = opt.is_ascii_lowercase();
                offset += align_skip(offset, size, &state);
                let value = read_int_bytes(data, offset, size, state.endian, signed)?;
                offset += size;
                results.push(PackValue::Int(value));
            }
            b'i' | b'I' => {
                let size = read_size(format, &mut pos, 4)?;
                check_int_size(opt, size)?;
                check_option_alignment(size, &state)?;
                let signed = opt == b'i';
                offset += align_skip(offset, size, &state);
                let value = read_int_bytes(data, offset, size, state.endian, signed)?;
                offset += size;
                results.push(PackValue::Int(value));
            }
            b'f' => {
                offset += align_skip(offset, 4, &state);
                if offset + 4 > data.len() {
                    return Err("data string too short".to_string());
                }
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&data[offset..offset + 4]);
                let value = match state.endian {
                    Endian::Little => f32::from_le_bytes(bytes),
                    Endian::Big => f32::from_be_bytes(bytes),
                };
                offset += 4;
                results.push(PackValue::Num(value as f64));
            }
            b'd' | b'n' => {
                offset += align_skip(offset, 8, &state);
                if offset + 8 > data.len() {
                    return Err("data string too short".to_string());
                }
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&data[offset..offset + 8]);
                let value = match state.endian {
                    Endian::Little => f64::from_le_bytes(bytes),
                    Endian::Big => f64::from_be_bytes(bytes),
                };
                offset += 8;
                results.push(PackValue::Num(value));
            }
            b's' => {
                let size = read_size(format, &mut pos, 8)?;
                check_int_size(b's', size)?;
                offset += align_skip(offset, size, &state);
                let len = read_int_bytes(data, offset, size, state.endian, false)? as usize;
                offset += size;
                if offset + len > data.len() {
                    return Err("data string too short".to_string());
                }
                results.push(PackValue::Str(data[offset..offset + len].to_vec()));
                offset += len;
            }
            b'z' => {
                let end = data[offset..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or_else(|| "unfinished string for format 'z'".to_string())?;
                results.push(PackValue::Str(data[offset..offset + end].to_vec()));
                offset += end + 1;
            }
            b'c' => {
                let size_start = pos;
                let size = read_size(format, &mut pos, 0)?;
                if pos == size_start {
                    return Err("missing size for format option 'c'".to_string());
                }
                if offset + size > data.len() {
                    return Err("data string too short".to_string());
                }
                results.push(PackValue::Str(data[offset..offset + size].to_vec()));
                offset += size;
            }
            _ => return Err(format!("invalid format option '{}'", opt as char)),
        }
    }
    Ok((results, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_integer_option() {
        for (format, value) in [
            (&b"b"[..], -12i64),
            (b"B", 250),
            (b"h", -1000),
            (b"H", 60000),
            (b"i", -70000),
            (b"I", 70000),
            (b"l", -5_000_000_000),
            (b"L", 5_000_000_000),
            (b"j", i64::MIN),
            (b"J", i64::MAX),
            (b"T", 12345),
        ] {
            let packed = pack(format, &[PackValue::Int(value)]).unwrap();
            let (unpacked, next) = unpack(format, &packed, 0).unwrap();
            assert_eq!(
                unpacked,
                vec![PackValue::Int(value)],
                "{}",
                String::from_utf8_lossy(format)
            );
            assert_eq!(next, packed.len());
        }
    }

    #[test]
    fn round_trips_sized_integers_and_endianness() {
        for format in [&b"<i4"[..], b">i4", b"<I2", b">I2", b"=i8"] {
            let packed = pack(format, &[PackValue::Int(-42)]).ok();
            if let Some(packed) = packed {
                let (unpacked, _) = unpack(format, &packed, 0).unwrap();
                assert_eq!(unpacked, vec![PackValue::Int(-42)]);
            }
        }
    }

    #[test]
    fn round_trips_floats_and_doubles() {
        let packed = pack(b"<f", &[PackValue::Num(1.5)]).unwrap();
        let (unpacked, _) = unpack(b"<f", &packed, 0).unwrap();
        assert_eq!(unpacked, vec![PackValue::Num(1.5)]);

        let packed = pack(b">d", &[PackValue::Num(3.14158)]).unwrap();
        let (unpacked, _) = unpack(b">d", &packed, 0).unwrap();
        assert_eq!(unpacked, vec![PackValue::Num(3.14158)]);
    }

    #[test]
    fn round_trips_length_prefixed_and_zero_terminated_strings() {
        let packed = pack(b"s", &[PackValue::Str(b"hello".to_vec())]).unwrap();
        let (unpacked, next) = unpack(b"s", &packed, 0).unwrap();
        assert_eq!(unpacked, vec![PackValue::Str(b"hello".to_vec())]);
        assert_eq!(next, packed.len());

        let packed = pack(b"z", &[PackValue::Str(b"world".to_vec())]).unwrap();
        assert_eq!(packed, b"world\0");
        let (unpacked, next) = unpack(b"z", &packed, 0).unwrap();
        assert_eq!(unpacked, vec![PackValue::Str(b"world".to_vec())]);
        assert_eq!(next, packed.len());
    }

    #[test]
    fn round_trips_fixed_size_strings_with_zero_padding() {
        let packed = pack(b"c8", &[PackValue::Str(b"hi".to_vec())]).unwrap();
        assert_eq!(packed.len(), 8);
        let (unpacked, _) = unpack(b"c8", &packed, 0).unwrap();
        assert_eq!(unpacked, vec![PackValue::Str(b"hi\0\0\0\0\0\0".to_vec())]);
    }

    #[test]
    fn packsize_matches_the_actual_packed_length_for_fixed_formats() {
        for format in [&b"i4i4"[..], b"<i2i8", b"!8i1i4", b"bBhHlLjJ"] {
            let size = packsize(format).unwrap();
            let packed = pack(
                format,
                &vec![PackValue::Int(1); format.iter().filter(|c| c.is_ascii_alphabetic()).count()],
            )
            .unwrap();
            assert_eq!(size, packed.len(), "{}", String::from_utf8_lossy(format));
        }
    }

    #[test]
    fn packsize_rejects_variable_size_formats() {
        assert!(packsize(b"s").is_err());
        assert!(packsize(b"z").is_err());
    }

    #[test]
    fn alignment_inserts_padding_between_fields() {
        let packed = pack(b"!4bi4", &[PackValue::Int(1), PackValue::Int(2)]).unwrap();
        assert_eq!(packed.len(), 8);
        assert_eq!(packed[0], 1);
        assert_eq!(&packed[4..8], &2i32.to_le_bytes());
    }

    #[test]
    fn x_option_aligns_to_the_following_fixed_size_option() {
        let format = b">!8bXhi4i8c1Xi8";
        let packed = pack(
            format,
            &[
                PackValue::Int(-12),
                PackValue::Int(100),
                PackValue::Int(200),
                PackValue::Str(vec![0xec]),
            ],
        )
        .unwrap();
        assert_eq!(packed.len(), packsize(format).unwrap());
        assert!(pack(b"X", &[]).is_err());
        assert!(pack(b"X i4", &[]).is_err());
        assert!(unpack(b"Xc1", b"", 0).is_err());
    }

    #[test]
    fn fixed_string_pack_rejects_a_too_long_value() {
        assert!(pack(b"c2", &[PackValue::Str(b"abc".to_vec())]).is_err());
    }

    #[test]
    fn overflowing_size_suffix_leaves_the_first_excess_digit_as_an_option() {
        let error = packsize(b"c10000000000000000000000000000000000000000").unwrap_err();
        assert!(error.contains("invalid format option '0'"), "{error}");
    }
}
