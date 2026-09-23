//! the utf8 library.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::util::*;
use super::*;

impl LuaRuntime {
    pub(super) fn call_native_utf8(
        &mut self,
        function: NativeFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let required = |index: usize| {
            args.get(index).cloned().ok_or_else(|| {
                LuaError::new(format!(
                    "bad argument #{} to '{}' (value expected)",
                    index + 1,
                    function.name()
                ))
            })
        };
        match function {
            NativeFunction::Utf8Len => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                let start = relative_position(
                    args.get(1)
                        .map(|value| self.integer(value))
                        .transpose()?
                        .unwrap_or(1),
                    bytes.len(),
                );
                let end = relative_position(
                    args.get(2)
                        .map(|value| self.integer(value))
                        .transpose()?
                        .unwrap_or(-1),
                    bytes.len(),
                );
                if start < 1 || start > bytes.len() as i64 + 1 {
                    return Err(LuaError::new("initial position out of bounds"));
                }
                if end > bytes.len() as i64 {
                    return Err(LuaError::new("final position out of bounds"));
                }
                let lax = args.get(3).is_some_and(LuaValue::truthy);
                let mut position = (start - 1) as usize;
                let final_position = end - 1;
                let mut count = 0;
                while (position as i64) <= final_position {
                    let Some((_, next)) = decode_lua_utf8(bytes, position, !lax) else {
                        return Ok(vec![LuaValue::Nil, LuaValue::Integer(position as i64 + 1)]);
                    };
                    position = next;
                    count += 1;
                }
                Ok(vec![LuaValue::Integer(count)])
            }
            NativeFunction::Utf8Char => {
                let mut output = Vec::new();
                for value in &args {
                    let codepoint = self.integer(value)?;
                    if codepoint < 0 || !encode_lua_utf8(codepoint as u32, &mut output) {
                        return Err(LuaError::new("value out of range for utf8.char"));
                    }
                }
                self.charge_allocation(output.len())?;
                Ok(vec![LuaValue::String(Rc::new(output))])
            }
            NativeFunction::Utf8Codepoint => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                let start = relative_position(
                    args.get(1)
                        .map(|value| self.integer(value))
                        .transpose()?
                        .unwrap_or(1),
                    bytes.len(),
                );
                let end = relative_position(
                    args.get(2)
                        .map(|value| self.integer(value))
                        .transpose()?
                        .unwrap_or(start),
                    bytes.len(),
                );
                if start < 1 || end > bytes.len() as i64 {
                    return Err(LuaError::new("out of bounds"));
                }
                let lax = args.get(3).is_some_and(LuaValue::truthy);
                let mut values = Vec::new();
                let mut position = (start - 1) as usize;
                while position < end as usize {
                    let (codepoint, next) = decode_lua_utf8(bytes, position, !lax)
                        .ok_or_else(|| LuaError::new("invalid UTF-8 code"))?;
                    values.push(LuaValue::Integer(codepoint as i64));
                    position = next;
                }
                Ok(values)
            }
            NativeFunction::Utf8Offset => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                let mut count = self.integer(&required(1)?)?;
                let default = if count >= 0 {
                    1
                } else {
                    bytes.len() as i64 + 1
                };
                let position = relative_position(
                    args.get(2)
                        .map(|value| self.integer(value))
                        .transpose()?
                        .unwrap_or(default),
                    bytes.len(),
                );
                if position < 1 || position > bytes.len() as i64 + 1 {
                    return Err(LuaError::new("position out of bounds"));
                }
                let mut position = (position - 1) as usize;
                if count == 0 {
                    while position > 0
                        && position < bytes.len()
                        && utf8_continuation(bytes[position])
                    {
                        position -= 1;
                    }
                } else {
                    if position < bytes.len() && utf8_continuation(bytes[position]) {
                        return Err(LuaError::new("initial position is a continuation byte"));
                    }
                    if count < 0 {
                        while count < 0 && position > 0 {
                            position -= 1;
                            while position > 0 && utf8_continuation(bytes[position]) {
                                position -= 1;
                            }
                            count += 1;
                        }
                    } else {
                        count -= 1;
                        while count > 0 && position < bytes.len() {
                            position += 1;
                            while position < bytes.len() && utf8_continuation(bytes[position]) {
                                position += 1;
                            }
                            count -= 1;
                        }
                    }
                }
                if count != 0 {
                    return Ok(vec![LuaValue::Nil]);
                }
                let start = position;
                if position < bytes.len() && bytes[position] & 0x80 != 0 {
                    if utf8_continuation(bytes[position]) {
                        return Err(LuaError::new("initial position is a continuation byte"));
                    }
                    while position + 1 < bytes.len() && utf8_continuation(bytes[position + 1]) {
                        position += 1;
                    }
                }
                Ok(vec![
                    LuaValue::Integer(start as i64 + 1),
                    LuaValue::Integer(position as i64 + 1),
                ])
            }
            NativeFunction::Utf8Codes => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                if bytes.first().is_some_and(|byte| utf8_continuation(*byte)) {
                    return Err(LuaError::new("invalid UTF-8 code"));
                }
                let lax = args.get(1).is_some_and(LuaValue::truthy);
                Ok(vec![
                    LuaValue::NativeFunction(if lax {
                        NativeFunction::Utf8IteratorLax
                    } else {
                        NativeFunction::Utf8IteratorStrict
                    }),
                    input,
                    LuaValue::Integer(0),
                ])
            }
            NativeFunction::Utf8IteratorStrict | NativeFunction::Utf8IteratorLax => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                let previous = self.integer(&required(1)?)?;
                if previous < 0 {
                    return Ok(Vec::new());
                }
                let mut position = previous as usize;
                if position < bytes.len() {
                    while position < bytes.len() && utf8_continuation(bytes[position]) {
                        position += 1;
                    }
                }
                if position >= bytes.len() {
                    return Ok(Vec::new());
                }
                let strict = function == NativeFunction::Utf8IteratorStrict;
                let (codepoint, next) = decode_lua_utf8(bytes, position, strict)
                    .ok_or_else(|| LuaError::new("invalid UTF-8 code"))?;
                if next < bytes.len() && utf8_continuation(bytes[next]) {
                    return Err(LuaError::new("invalid UTF-8 code"));
                }
                Ok(vec![
                    LuaValue::Integer(position as i64 + 1),
                    LuaValue::Integer(codepoint as i64),
                ])
            }
            _ => unreachable!("call_native_utf8 received a non-utf8 NativeFunction"),
        }
    }
}
