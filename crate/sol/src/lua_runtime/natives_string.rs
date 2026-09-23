//! the string library, Lua pattern matching (find/match/gmatch/gsub),
//! string.format, and string.pack/unpack/packsize.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::format::*;
use super::frame::*;
use super::util::*;
use super::*;

impl LuaRuntime {
    pub(super) fn call_native_string(
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
            NativeFunction::StringLen => Ok(vec![LuaValue::Integer(
                self.string(&required(0)?)?.len() as i64,
            )]),
            NativeFunction::StringByte => {
                let input = required(0)?;
                let bytes = self.string(&input)?;
                let start = args
                    .get(1)
                    .filter(|value| **value != LuaValue::Nil)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(1);
                let end = args
                    .get(2)
                    .filter(|value| **value != LuaValue::Nil)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(start);
                let Some((start, end)) = byte_range(start, end, bytes.len()) else {
                    return Ok(Vec::new());
                };
                Ok(bytes[start..=end]
                    .iter()
                    .map(|byte| LuaValue::Integer(i64::from(*byte)))
                    .collect())
            }
            NativeFunction::StringChar => {
                let mut bytes = Vec::with_capacity(args.len());
                for value in &args {
                    let value = self.integer(value)?;
                    let byte = u8::try_from(value)
                        .map_err(|_| LuaError::new("value out of range for string.char"))?;
                    bytes.push(byte);
                }
                self.charge_allocation(bytes.len())?;
                Ok(vec![LuaValue::String(Rc::new(bytes))])
            }
            NativeFunction::StringLower => Ok(vec![LuaValue::String(Rc::new(
                self.string(&required(0)?)?
                    .iter()
                    .map(u8::to_ascii_lowercase)
                    .collect(),
            ))]),
            NativeFunction::StringUpper => Ok(vec![LuaValue::String(Rc::new(
                self.string(&required(0)?)?
                    .iter()
                    .map(u8::to_ascii_uppercase)
                    .collect(),
            ))]),
            NativeFunction::StringReverse => {
                let mut value = self.string(&required(0)?)?.to_vec();
                value.reverse();
                Ok(vec![LuaValue::String(Rc::new(value))])
            }
            NativeFunction::StringRep => {
                let value = self.string(&required(0)?)?.to_vec();
                let count = self.integer(&required(1)?)?;
                let separator = args
                    .get(2)
                    .map(|value| self.string(value).map(Vec::from))
                    .transpose()?
                    .unwrap_or_default();
                if count <= 0 {
                    return Ok(vec![LuaValue::String(Rc::new(Vec::new()))]);
                }
                let count = count as usize;
                // A huge `count` (e.g. `math.maxinteger`) must raise a Lua
                // error rather than attempt an unbounded allocation/loop -
                // real Lua rejects this the same way (`string.rep("a",
                // math.maxinteger)` errors "resulting string too large"
                // instead of hanging). This cap is independent of (and
                // checked before) the embedder-configurable allocation
                // budget: real Lua's own size ceiling isn't configurable
                // either, and `lua-5.5.1-tests/strings.lua` specifically
                // checks for the literal "too large" wording, not a generic
                // budget-exhaustion error.
                let total = value
                    .len()
                    .checked_add(separator.len())
                    .and_then(|per_unit| per_unit.checked_mul(count))
                    .and_then(|total| total.checked_sub(separator.len()))
                    .filter(|&total| total <= isize::MAX as usize)
                    .ok_or_else(|| LuaError::new("resulting string too large"))?;
                self.charge_allocation(total)?;
                let mut output = Vec::with_capacity(total);
                for index in 0..count {
                    if index != 0 {
                        output.extend(&separator);
                    }
                    output.extend(&value);
                }
                Ok(vec![LuaValue::String(Rc::new(output))])
            }
            NativeFunction::StringDump => {
                // The body is still a same-process handle (see
                // `dumped_protos`), but it follows Lua 5.5's complete binary
                // header rather than exposing a private nine-byte prefix.
                // Include source metadata and pooled strings in the opaque
                // payload too: native dumps write each once, and this keeps
                // byte-oriented dump consumers (including the corpus's
                // string-reuse check) observably compatible.
                let closure = match required(0)? {
                    LuaValue::Closure(closure) => closure,
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'dump' (Lua function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                let key = Rc::as_ptr(&closure.proto) as usize;
                self.dumped_protos.insert(key, closure.proto.clone());
                let mut payload = Vec::new();
                if let Some(source) = self.chunk_sources.get(&key) {
                    payload.extend_from_slice(source);
                }
                append_dump_constants(&closure.proto, &mut payload);
                let mut bytes = super::natives_load::lua55_binary_chunk_header();
                bytes.extend_from_slice(super::natives_load::SOL_DUMP_MAGIC);
                bytes.extend_from_slice(&(key as u64).to_le_bytes());
                bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
                bytes.extend_from_slice(&payload);
                Ok(vec![LuaValue::String(Rc::new(bytes))])
            }
            NativeFunction::StringSub => {
                let input = required(0)?;
                let value = self.string(&input)?;
                let start = self.integer(&required(1)?)?;
                let end = args
                    .get(2)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(value.len() as i64);
                let bytes = byte_range(start, end, value.len())
                    .map(|(start, end)| value[start..=end].to_vec())
                    .unwrap_or_default();
                Ok(vec![LuaValue::String(Rc::new(bytes))])
            }
            NativeFunction::StringFind => {
                let input = required(0)?;
                let source = self.string(&input)?.to_vec();
                let pattern = self.string(&required(1)?)?.to_vec();
                let init_arg = args.get(2).map(|value| self.integer(value)).transpose()?;
                let plain = args.get(3).map(|value| value.truthy()).unwrap_or(false);
                let Some(init) = resolve_init(init_arg, source.len()) else {
                    return Ok(vec![LuaValue::Nil]);
                };
                if plain {
                    return Ok(match find_plain(&source, &pattern, init) {
                        Some((start, end)) => vec![
                            LuaValue::Integer(start as i64 + 1),
                            LuaValue::Integer(end as i64),
                        ],
                        None => vec![LuaValue::Nil],
                    });
                }
                match crate::lua_pattern::find(&source, &pattern, init).map_err(LuaError::new)? {
                    Some(m) => {
                        let mut result = vec![
                            LuaValue::Integer(m.start as i64 + 1),
                            LuaValue::Integer(m.end as i64),
                        ];
                        result.extend(self.capture_values(
                            &source,
                            (m.start, m.end),
                            &m.captures,
                            false,
                        ));
                        Ok(result)
                    }
                    None => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::StringMatch => {
                let input = required(0)?;
                let source = self.string(&input)?.to_vec();
                let pattern = self.string(&required(1)?)?.to_vec();
                let init_arg = args.get(2).map(|value| self.integer(value)).transpose()?;
                let Some(init) = resolve_init(init_arg, source.len()) else {
                    return Ok(vec![LuaValue::Nil]);
                };
                match crate::lua_pattern::find(&source, &pattern, init).map_err(LuaError::new)? {
                    Some(m) => {
                        Ok(self.capture_values(&source, (m.start, m.end), &m.captures, true))
                    }
                    None => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::StringGMatch => {
                let source = self.string(&required(0)?)?.to_vec();
                let pattern = self.string(&required(1)?)?.to_vec();
                let init_arg = args.get(2).map(|value| self.integer(value)).transpose()?;
                let init = resolve_init(init_arg, source.len()).unwrap_or(source.len() + 1);
                self.charge_allocation(source.len() + pattern.len())?;
                let state = GMatchState {
                    source: Rc::new(source),
                    pattern: Rc::new(pattern),
                    position: init,
                    last_end: None,
                };
                Ok(vec![LuaValue::GMatchIterator(Rc::new(RefCell::new(state)))])
            }
            NativeFunction::StringGSub => {
                let source_value = required(0)?;
                let source = match source_value {
                    LuaValue::String(source) => source,
                    value => Rc::new(self.string(&value)?.to_vec()),
                };
                let pattern = self.string(&required(1)?)?.to_vec();
                let repl = required(2)?;
                if !matches!(
                    repl,
                    LuaValue::String(_)
                        | LuaValue::Integer(_)
                        | LuaValue::Float(_)
                        | LuaValue::Table(_)
                        | LuaValue::Closure(_)
                        | LuaValue::NativeFunction(_)
                        | LuaValue::RegisteredNative(_)
                        | LuaValue::GMatchIterator(_)
                ) {
                    return Err(LuaError::new(
                        "bad argument #3 to 'gsub' (string/function/table expected)",
                    ));
                }
                let max = args
                    .get(3)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .map(|value| value.max(0) as usize)
                    .unwrap_or(usize::MAX);
                let (anchored, body) = match pattern.first() {
                    Some(b'^') => (true, pattern[1..].to_vec()),
                    _ => (false, pattern.clone()),
                };
                let (output, count) = self.gsub_run_sync(source, &body, anchored, max, &repl)?;
                Ok(vec![output, LuaValue::Integer(count as i64)])
            }
            NativeFunction::StringFormat => {
                let format = self.string(&required(0)?)?.to_vec();
                let values = args[1..].to_vec();
                let mut arg_index = 0usize;
                let mut output = Vec::new();
                let mut i = 0usize;
                while i < format.len() {
                    let byte = format[i];
                    if byte != b'%' {
                        output.push(byte);
                        i += 1;
                        continue;
                    }
                    i += 1;
                    let spec_start = i;
                    while matches!(
                        format.get(i),
                        Some(b'-') | Some(b'+') | Some(b' ') | Some(b'#') | Some(b'0')
                    ) {
                        i += 1;
                    }
                    while matches!(format.get(i), Some(byte) if byte.is_ascii_digit()) {
                        i += 1;
                    }
                    if format.get(i) == Some(&b'.') {
                        i += 1;
                        while matches!(format.get(i), Some(byte) if byte.is_ascii_digit()) {
                            i += 1;
                        }
                    }
                    let Some(&conversion) = format.get(i) else {
                        return Err(LuaError::new("invalid conversion to 'format'"));
                    };
                    let spec = format[spec_start..i].to_vec();
                    i += 1;
                    if conversion == b'%' {
                        output.push(b'%');
                        continue;
                    }
                    let value = values.get(arg_index).cloned().ok_or_else(|| {
                        LuaError::new(format!(
                            "bad argument #{} to 'format' (no value)",
                            arg_index + 2
                        ))
                    })?;
                    arg_index += 1;
                    output.extend(self.format_one(&spec, conversion, value)?);
                }
                self.charge_allocation(output.len())?;
                Ok(vec![LuaValue::String(Rc::new(output))])
            }
            NativeFunction::StringPack => {
                let format = self.string(&required(0)?)?.to_vec();
                // Fixed-result call sites may carry trailing Lua nil padding
                // in their register slice. They are absent arguments, not
                // values supplied to string.pack: preserving them here makes
                // a malformed `X i17` format report a missing-value type
                // error before the format parser can reject its invalid size.
                let provided = args
                    .iter()
                    .skip(1)
                    .rev()
                    .skip_while(|value| matches!(value, LuaValue::Nil))
                    .count();
                let mut values = Vec::with_capacity(provided);
                for value in args.iter().skip(1).take(provided) {
                    values.push(match value {
                        LuaValue::Integer(n) => crate::lua_pack::PackValue::Int(*n),
                        LuaValue::Float(n) => crate::lua_pack::PackValue::Num(*n),
                        LuaValue::String(bytes) => {
                            crate::lua_pack::PackValue::Str(bytes.as_ref().clone())
                        }
                        other => {
                            return Err(LuaError::new(format!(
                                "bad argument to 'pack' (number or string expected, got {})",
                                other.type_name()
                            )))
                        }
                    });
                }
                let packed = crate::lua_pack::pack(&format, &values).map_err(LuaError::new)?;
                self.charge_allocation(packed.len())?;
                Ok(vec![LuaValue::String(Rc::new(packed))])
            }
            NativeFunction::StringUnpack => {
                let format = self.string(&required(0)?)?.to_vec();
                let data = self.string(&required(1)?)?.to_vec();
                let start = match args.get(2) {
                    Some(value) => {
                        let position = self.integer(value)?;
                        let offset = if position > 0 {
                            position as i128 - 1
                        } else if position < 0 {
                            data.len() as i128 + position as i128
                        } else {
                            -1
                        };
                        if offset < 0 || offset > data.len() as i128 {
                            return Err(LuaError::new("initial position out of string"));
                        }
                        offset as usize
                    }
                    None => 0,
                };
                let (values, next) =
                    crate::lua_pack::unpack(&format, &data, start).map_err(LuaError::new)?;
                let mut results: Vec<LuaValue> = values
                    .into_iter()
                    .map(|value| match value {
                        crate::lua_pack::PackValue::Int(n) => LuaValue::Integer(n),
                        crate::lua_pack::PackValue::Num(n) => LuaValue::Float(n),
                        crate::lua_pack::PackValue::Str(bytes) => LuaValue::String(Rc::new(bytes)),
                    })
                    .collect();
                results.push(LuaValue::Integer(next as i64 + 1));
                Ok(results)
            }
            NativeFunction::StringPackSize => {
                let format = self.string(&required(0)?)?.to_vec();
                let size = crate::lua_pack::packsize(&format).map_err(LuaError::new)?;
                Ok(vec![LuaValue::Integer(size as i64)])
            }
            _ => unreachable!("call_native_string received a non-string NativeFunction"),
        }
    }

    pub(super) fn call_gmatch_iterator(
        &mut self,
        state: RcRef<GMatchState>,
    ) -> LuaResult<Vec<LuaValue>> {
        let (source, pattern) = {
            let state = state.borrow();
            (state.source.clone(), state.pattern.clone())
        };
        let mut position = state.borrow().position;
        let last_end = state.borrow().last_end;
        while position <= source.len() {
            match crate::lua_pattern::match_at(&source, &pattern, position)
                .map_err(LuaError::new)?
            {
                Some(m) if Some(m.end) != last_end => {
                    let mut guard = state.borrow_mut();
                    guard.position = m.end;
                    guard.last_end = Some(m.end);
                    drop(guard);
                    return Ok(self.capture_values(&source, (m.start, m.end), &m.captures, true));
                }
                _ => position += 1,
            }
        }
        state.borrow_mut().position = source.len() + 1;
        Ok(vec![LuaValue::Nil])
    }

    fn capture_values(
        &self,
        source: &[u8],
        whole: (usize, usize),
        captures: &[crate::lua_pattern::Capture],
        whole_if_empty: bool,
    ) -> Vec<LuaValue> {
        if captures.is_empty() {
            if whole_if_empty {
                vec![LuaValue::String(Rc::new(source[whole.0..whole.1].to_vec()))]
            } else {
                Vec::new()
            }
        } else {
            captures
                .iter()
                .map(|capture| match capture {
                    crate::lua_pattern::Capture::Str(start, end) => {
                        LuaValue::String(Rc::new(source[*start..*end].to_vec()))
                    }
                    crate::lua_pattern::Capture::Position(position) => {
                        LuaValue::Integer(*position as i64)
                    }
                })
                .collect()
        }
    }

    fn gsub_replacement(
        &mut self,
        source: &[u8],
        m: &crate::lua_pattern::Match,
        repl: &LuaValue,
    ) -> LuaResult<Vec<u8>> {
        let whole = source[m.start..m.end].to_vec();
        match repl {
            LuaValue::String(template) => {
                expand_gsub_template(template, source, &whole, &m.captures)
            }
            LuaValue::Integer(_) | LuaValue::Float(_) => {
                expand_gsub_template(&repl.display_bytes(), source, &whole, &m.captures)
            }
            LuaValue::Table(table) => {
                let key = self
                    .capture_values(source, (m.start, m.end), &m.captures, true)
                    .into_iter()
                    .next()
                    .unwrap_or(LuaValue::Nil);
                let value = match self.index_resolve(LuaValue::Table(table.clone()), key)? {
                    IndexResolution::Value(value) => value,
                    IndexResolution::Call { method, args } => self
                        .call(method, args)?
                        .into_iter()
                        .next()
                        .unwrap_or(LuaValue::Nil),
                };
                gsub_result_value(value, &whole)
            }
            LuaValue::Closure(_)
            | LuaValue::NativeFunction(_)
            | LuaValue::RegisteredNative(_)
            | LuaValue::GMatchIterator(_)
            | LuaValue::CoroutineWrapper(_) => {
                let call_args = self.capture_values(source, (m.start, m.end), &m.captures, true);
                let value = self
                    .call(repl.clone(), call_args)?
                    .into_iter()
                    .next()
                    .unwrap_or(LuaValue::Nil);
                gsub_result_value(value, &whole)
            }
            _ => Err(LuaError::new(
                "bad argument #3 to 'gsub' (string/function/table expected)",
            )),
        }
    }

    pub(super) fn gsub_run_sync(
        &mut self,
        source: Rc<Vec<u8>>,
        body: &[u8],
        anchored: bool,
        max: usize,
        repl: &LuaValue,
    ) -> LuaResult<(LuaValue, usize)> {
        let mut output = Vec::new();
        let mut count = 0usize;
        let mut pos = 0usize;
        let mut last_match_end = None;
        while count < max {
            match crate::lua_pattern::match_at(&source, body, pos).map_err(LuaError::new)? {
                Some(m) if Some(m.end) != last_match_end => {
                    count += 1;
                    last_match_end = Some(m.end);
                    let replacement = self.gsub_replacement(&source, &m, repl)?;
                    self.charge_allocation(replacement.len())?;
                    output.extend_from_slice(&replacement);
                    if m.end > pos {
                        pos = m.end;
                    } else {
                        if pos < source.len() {
                            output.push(source[pos]);
                        }
                        pos += 1;
                    }
                }
                Some(_) | None => {
                    if pos < source.len() {
                        output.push(source[pos]);
                        pos += 1;
                    } else {
                        break;
                    }
                }
            }
            if anchored || pos > source.len() {
                break;
            }
        }
        if pos < source.len() {
            output.extend_from_slice(&source[pos..]);
        }
        let output = if count == 0 {
            LuaValue::String(source)
        } else {
            LuaValue::String(Rc::new(output))
        };
        Ok((output, count))
    }

    pub(super) fn gsub_step(
        &mut self,
        state: &mut GsubState,
        resume: Option<LuaValue>,
    ) -> LuaResult<GsubOutcome> {
        if let Some(value) = resume {
            let (start, end) = state
                .pending
                .take()
                .expect("gsub_step: resumed with no pending match");
            if let Some(done) = self.gsub_accept_value(state, start, end, value)? {
                return Ok(done);
            }
        }
        while state.count < state.max {
            match crate::lua_pattern::match_at(&state.source, &state.body, state.pos)
                .map_err(LuaError::new)?
            {
                Some(m) if Some(m.end) != state.last_match_end => {
                    state.count += 1;
                    state.last_match_end = Some(m.end);
                    let replacement_args =
                        self.capture_values(&state.source, (m.start, m.end), &m.captures, true);
                    if matches!(state.repl, LuaValue::Table(_)) {
                        let key = replacement_args.into_iter().next().unwrap_or(LuaValue::Nil);
                        match self.index_resolve(state.repl.clone(), key)? {
                            IndexResolution::Value(value) => {
                                if let Some(done) =
                                    self.gsub_accept_value(state, m.start, m.end, value)?
                                {
                                    return Ok(done);
                                }
                                continue;
                            }
                            IndexResolution::Call { method, args } => {
                                state.pending = Some((m.start, m.end));
                                return Ok(GsubOutcome::NeedsCall {
                                    callee: method,
                                    args,
                                });
                            }
                        }
                    } else {
                        state.pending = Some((m.start, m.end));
                        return Ok(GsubOutcome::NeedsCall {
                            callee: state.repl.clone(),
                            args: replacement_args,
                        });
                    }
                }
                Some(_) | None => {
                    if state.pos < state.source.len() {
                        state.output.push(state.source[state.pos]);
                        state.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            if state.anchored || state.pos > state.source.len() {
                break;
            }
        }
        if state.pos < state.source.len() {
            state.output.extend_from_slice(&state.source[state.pos..]);
        }
        Ok(self.finish_gsub(state))
    }

    fn gsub_accept_value(
        &mut self,
        state: &mut GsubState,
        start: usize,
        end: usize,
        value: LuaValue,
    ) -> LuaResult<Option<GsubOutcome>> {
        state.changed |= !matches!(value, LuaValue::Nil | LuaValue::Bool(false));
        let replacement = gsub_result_value(value, &state.source[start..end])?;
        self.charge_allocation(replacement.len())?;
        state.output.extend_from_slice(&replacement);
        if end > state.pos {
            state.pos = end;
        } else {
            if state.pos < state.source.len() {
                state.output.push(state.source[state.pos]);
            }
            state.pos += 1;
        }
        if state.anchored || state.pos > state.source.len() {
            if state.pos < state.source.len() {
                state.output.extend_from_slice(&state.source[state.pos..]);
            }
            return Ok(Some(self.finish_gsub(state)));
        }
        Ok(None)
    }

    fn finish_gsub(&self, state: &mut GsubState) -> GsubOutcome {
        let output = if state.changed {
            LuaValue::String(Rc::new(std::mem::take(&mut state.output)))
        } else {
            LuaValue::String(state.source.clone())
        };
        GsubOutcome::Done(output, state.count)
    }

    fn format_one(&mut self, spec: &[u8], conversion: u8, value: LuaValue) -> LuaResult<Vec<u8>> {
        if spec.len() > 32 {
            return Err(LuaError::new("format too long"));
        }
        let (minus, plus, space, hash, zero, width, precision) = parse_format_spec(spec);
        if width.is_some_and(|width| width > 99)
            || precision.is_some_and(|precision| precision > 99)
        {
            return Err(LuaError::new("invalid conversion to 'format'"));
        }
        let invalid_modifiers = match conversion {
            b'q' if !spec.is_empty() => Some("cannot have modifiers"),
            b'c' if zero || hash || plus || space || precision.is_some() => {
                Some("invalid conversion to 'format'")
            }
            b's' if zero || hash || plus || space => Some("invalid conversion to 'format'"),
            b'd' | b'i' if hash => Some("invalid conversion to 'format'"),
            b'p' if zero || hash || plus || space || precision.is_some() => {
                Some("invalid conversion to 'format'")
            }
            _ => None,
        };
        if let Some(error) = invalid_modifiers {
            return Err(LuaError::new(error));
        }
        let text: Vec<u8> = match conversion {
            b'd' | b'i' => {
                let n = self.integer(&value)?;
                let mut digits = n.unsigned_abs().to_string();
                if let Some(p) = precision {
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                let sign = if n < 0 {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                format!("{sign}{digits}").into_bytes()
            }
            b'u' => {
                let n = self.integer(&value)? as u64;
                let mut digits = if n == 0 && precision == Some(0) {
                    String::new()
                } else {
                    n.to_string()
                };
                if let Some(precision) = precision {
                    while digits.len() < precision {
                        digits.insert(0, '0');
                    }
                }
                digits.into_bytes()
            }
            b'x' | b'X' => {
                let n = self.integer(&value)? as u64;
                let mut digits = if conversion == b'x' {
                    format!("{n:x}")
                } else {
                    format!("{n:X}")
                };
                if let Some(p) = precision {
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                if hash && n != 0 {
                    digits = format!("{}{digits}", if conversion == b'x' { "0x" } else { "0X" });
                }
                digits.into_bytes()
            }
            b'o' => {
                let n = self.integer(&value)? as u64;
                let mut digits = format!("{n:o}");
                if let Some(precision) = precision {
                    while digits.len() < precision {
                        digits.insert(0, '0');
                    }
                }
                if hash && !digits.starts_with('0') {
                    digits.insert(0, '0');
                }
                digits.into_bytes()
            }
            b'c' => vec![self.integer(&value)? as u8],
            b'f' => {
                let n = number_as_f64(&value)?;
                let precision = precision.unwrap_or(6);
                let mut digits = format!("{:.*}", precision, n.abs());
                if hash && precision == 0 {
                    digits.push('.');
                }
                let sign = if n.is_sign_negative() {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                format!("{sign}{digits}").into_bytes()
            }
            b'e' | b'E' => {
                let n = number_as_f64(&value)?;
                let rendered =
                    to_c_exponential(n.abs(), precision.unwrap_or(6), conversion == b'E');
                let sign = if n.is_sign_negative() {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                format!("{sign}{rendered}").into_bytes()
            }
            b'g' | b'G' => {
                let n = number_as_f64(&value)?;
                let rendered = format_general(
                    n.abs(),
                    precision.unwrap_or(6).max(1),
                    conversion == b'G',
                    hash,
                );
                let sign = if n.is_sign_negative() {
                    "-"
                } else if plus {
                    "+"
                } else if space {
                    " "
                } else {
                    ""
                };
                format!("{sign}{rendered}").into_bytes()
            }
            b'a' | b'A' => format_hex_float(
                number_as_f64(&value)?,
                conversion == b'A',
                precision,
                plus,
                space,
            ),
            b's' => {
                if matches!(&value, LuaValue::String(bytes) if bytes.contains(&0))
                    && (width.is_some() || precision.is_some())
                {
                    return Err(LuaError::new("string contains zeros"));
                }
                let mut rendered = self.render_value(value)?;
                if let Some(p) = precision {
                    rendered.truncate(p);
                }
                rendered
            }
            b'p' => value
                .identity_address()
                .map(|pointer| format!("0x{pointer:x}").into_bytes())
                .unwrap_or_else(|| b"(null)".to_vec()),
            b'q' => quote_value(&value)?,
            other => {
                return Err(LuaError::new(format!(
                    "invalid conversion '%{}' to 'format'",
                    other as char
                )))
            }
        };
        Ok(pad_format(
            text,
            width,
            minus,
            zero && !matches!(conversion, b's' | b'q' | b'c'),
        ))
    }
}

fn append_dump_constants(proto: &crate::lua_bytecode::Proto, out: &mut Vec<u8>) {
    for constant in &proto.consts {
        if let crate::lua_bytecode::Const::Str(bytes) = constant {
            out.extend_from_slice(bytes);
        }
    }
    for nested in &proto.nested {
        append_dump_constants(nested, out);
    }
}
