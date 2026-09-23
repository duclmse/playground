//! String-formatting helper functions used by `natives`'s `string.format`
//! and pattern-`gsub` replacement-template handling: format-spec parsing,
//! padding, C-style exponential/general float formatting, and `%q` quoting.

use super::*;

/// Expand `%0`-`%9`/`%%` escapes in a `string.gsub` string replacement
/// template. `%0` is the whole match; `%1` falls back to the whole match too
/// when the pattern has no captures (mirroring `lstrlib.c`'s `push_onecapture`).
pub(super) fn expand_gsub_template(
    template: &[u8],
    source: &[u8],
    whole: &[u8],
    captures: &[crate::lua_pattern::Capture],
) -> LuaResult<Vec<u8>> {
    let mut out = Vec::with_capacity(template.len());
    let mut i = 0;
    while i < template.len() {
        let byte = template[i];
        if byte != b'%' {
            out.push(byte);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&marker) = template.get(i) else {
            return Err(LuaError::new("invalid use of '%' in replacement string"));
        };
        match marker {
            b'%' => out.push(b'%'),
            b'0' => out.extend_from_slice(whole),
            b'1'..=b'9' => {
                let index = (marker - b'1') as usize;
                if index >= captures.len() {
                    if index == 0 {
                        out.extend_from_slice(whole);
                    } else {
                        return Err(LuaError::new(format!(
                            "invalid capture index %{}",
                            marker as char,
                        )));
                    }
                } else {
                    match &captures[index] {
                        crate::lua_pattern::Capture::Str(start, end) => {
                            out.extend_from_slice(&source[*start..*end])
                        }
                        crate::lua_pattern::Capture::Position(position) => {
                            out.extend_from_slice(position.to_string().as_bytes())
                        }
                    }
                }
            }
            _ => return Err(LuaError::new("invalid use of '%' in replacement string")),
        }
        i += 1;
    }
    Ok(out)
}

pub(super) fn gsub_result_value(value: LuaValue, whole: &[u8]) -> LuaResult<Vec<u8>> {
    match value {
        LuaValue::Nil | LuaValue::Bool(false) => Ok(whole.to_vec()),
        LuaValue::String(bytes) => Ok(bytes.as_ref().clone()),
        value @ (LuaValue::Integer(_) | LuaValue::Float(_)) => Ok(value.display_bytes()),
        other => Err(LuaError::new(format!(
            "invalid replacement value (a {})",
            other.type_name()
        ))),
    }
}

/// Parses the flags/width/precision portion of a `string.format` conversion
/// specifier (everything between `%` and the conversion character).
pub(super) fn parse_format_spec(
    spec: &[u8],
) -> (bool, bool, bool, bool, bool, Option<usize>, Option<usize>) {
    let mut i = 0;
    let (mut minus, mut plus, mut space, mut hash, mut zero) = (false, false, false, false, false);
    while let Some(&byte) = spec.get(i) {
        match byte {
            b'-' => minus = true,
            b'+' => plus = true,
            b' ' => space = true,
            b'#' => hash = true,
            b'0' => zero = true,
            _ => break,
        }
        i += 1;
    }
    let width_start = i;
    while matches!(spec.get(i), Some(byte) if byte.is_ascii_digit()) {
        i += 1;
    }
    let width = (i > width_start).then(|| {
        std::str::from_utf8(&spec[width_start..i])
            .unwrap()
            .parse()
            .unwrap_or(usize::MAX)
    });
    let precision = if spec.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while matches!(spec.get(i), Some(byte) if byte.is_ascii_digit()) {
            i += 1;
        }
        Some(
            std::str::from_utf8(&spec[start..i])
                .unwrap_or("")
                .parse()
                .unwrap_or(0),
        )
    } else {
        None
    };
    (minus, plus, space, hash, zero, width, precision)
}

pub(super) fn pad_format(
    mut text: Vec<u8>,
    width: Option<usize>,
    minus: bool,
    zero: bool,
) -> Vec<u8> {
    let Some(width) = width else {
        return text;
    };
    if text.len() >= width {
        return text;
    }
    let fill = if zero && !minus { b'0' } else { b' ' };
    let padding = vec![fill; width - text.len()];
    if minus {
        text.extend(padding);
        text
    } else if fill == b'0' && matches!(text.first(), Some(b'-') | Some(b'+') | Some(b' ')) {
        let sign = text.remove(0);
        let mut result = vec![sign];
        result.extend(padding);
        result.extend(text);
        result
    } else {
        let mut result = padding;
        result.extend(text);
        result
    }
}

/// Renders `n` as `d.ddde±dd` (C `%e` style: signed exponent, minimum two
/// digits), unlike Rust's built-in `{:e}` which omits the sign/padding.
pub(super) fn to_c_exponential(n: f64, precision: usize, upper: bool) -> String {
    let formatted = format!("{:.*e}", precision, n);
    let (mantissa, exponent) = formatted
        .split_once('e')
        .expect("exponential format always contains 'e'");
    let exponent: i32 = exponent
        .parse()
        .expect("exponent is always a valid integer");
    let e_char = if upper { 'E' } else { 'e' };
    format!(
        "{mantissa}{e_char}{}{:02}",
        if exponent < 0 { "-" } else { "+" },
        exponent.abs()
    )
}

pub(super) fn strip_trailing_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    let trimmed = text.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

/// Approximates C `%g`/`%G`: chooses `%e`-style or `%f`-style based on the
/// decimal exponent and significant-digit precision, trimming trailing
/// zeros unless the `#` flag is set.
pub(super) fn format_general(n: f64, precision: usize, upper: bool, hash: bool) -> String {
    if n == 0.0 {
        return if hash {
            format!("0.{}", "0".repeat(precision.saturating_sub(1)))
        } else {
            "0".to_string()
        };
    }
    let exponent = n.abs().log10().floor() as i32;
    if exponent < -4 || exponent >= precision as i32 {
        let rendered = to_c_exponential(n, precision.saturating_sub(1), upper);
        if hash {
            rendered
        } else {
            let (mantissa, exp) = rendered.split_once(if upper { 'E' } else { 'e' }).unwrap();
            format!(
                "{}{}{}",
                strip_trailing_zeros(mantissa),
                if upper { 'E' } else { 'e' },
                exp
            )
        }
    } else {
        let decimals = (precision as i32 - 1 - exponent).max(0) as usize;
        let rendered = format!("{:.*}", decimals, n);
        if hash {
            rendered
        } else {
            strip_trailing_zeros(&rendered)
        }
    }
}

pub(super) fn quote_value(value: &LuaValue) -> LuaResult<Vec<u8>> {
    let quoted = match value {
        LuaValue::String(bytes) => {
            let mut out = Vec::with_capacity(bytes.len() + 2);
            out.push(b'"');
            let mut iter = bytes.iter().peekable();
            while let Some(&byte) = iter.next() {
                if byte == b'"' || byte == b'\\' || byte == b'\n' {
                    out.push(b'\\');
                    out.push(byte);
                } else if byte < 0x20 || byte == 0x7f {
                    let next_is_digit = iter.peek().is_some_and(|next| next.is_ascii_digit());
                    if next_is_digit {
                        out.extend(format!("\\{byte:03}").into_bytes());
                    } else {
                        out.extend(format!("\\{byte}").into_bytes());
                    }
                } else {
                    out.push(byte);
                }
            }
            out.push(b'"');
            out
        }
        LuaValue::Nil => b"nil".to_vec(),
        LuaValue::Bool(true) => b"true".to_vec(),
        LuaValue::Bool(false) => b"false".to_vec(),
        LuaValue::Integer(i64::MIN) => b"0x8000000000000000".to_vec(),
        LuaValue::Integer(value) => value.to_string().into_bytes(),
        LuaValue::Float(value) => quote_float(*value).into_bytes(),
        _ => return Err(LuaError::new("value has no literal form")),
    };
    Ok(quoted)
}

fn quote_float(value: f64) -> String {
    if value.is_nan() {
        return "(0/0)".into();
    }
    if value == f64::INFINITY {
        return "1e9999".into();
    }
    if value == f64::NEG_INFINITY {
        return "-1e9999".into();
    }
    let bits = value.to_bits();
    let sign = if bits >> 63 == 0 { "" } else { "-" };
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    if exponent == 0 && fraction == 0 {
        return format!("{sign}0x0p+0");
    }
    let mut digits = format!("{fraction:013x}");
    while digits.ends_with('0') {
        digits.pop();
    }
    if exponent == 0 {
        format!("{sign}0x0.{digits}p-1022")
    } else {
        let fraction = if digits.is_empty() {
            String::new()
        } else {
            format!(".{digits}")
        };
        let exponent = exponent - 1023;
        format!(
            "{sign}0x1{fraction}p{}{exponent}",
            if exponent < 0 { "" } else { "+" }
        )
    }
}

pub(super) fn format_hex_float(
    value: f64,
    upper: bool,
    precision: Option<usize>,
    plus: bool,
    space: bool,
) -> Vec<u8> {
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let sign = if negative {
        "-"
    } else if plus {
        "+"
    } else if space {
        " "
    } else {
        ""
    };
    let raw = if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        "inf".to_string()
    } else {
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1_u64 << 52) - 1);
        let mut digits = format!("{fraction:013x}");
        match precision {
            Some(precision) => {
                digits.truncate(precision.min(13));
                while digits.len() < precision {
                    digits.push('0');
                }
            }
            None => {
                while digits.ends_with('0') {
                    digits.pop();
                }
            }
        }
        let fraction = if digits.is_empty() {
            String::new()
        } else {
            format!(".{digits}")
        };
        if exponent == 0 {
            let exponent = if fraction.is_empty() { 0 } else { -1022 };
            format!(
                "0x0{fraction}p{}{exponent}",
                if exponent < 0 { "" } else { "+" }
            )
        } else {
            let exponent = exponent - 1023;
            format!(
                "0x1{fraction}p{}{exponent}",
                if exponent < 0 { "" } else { "+" }
            )
        }
    };
    let rendered = format!("{sign}{raw}");
    if upper {
        rendered.to_ascii_uppercase().into_bytes()
    } else {
        rendered.into_bytes()
    }
}
