//! core primitives: print/type/tostring/tonumber, raw table access,
//! metatables, error/pcall/xpcall, select/next/pairs/ipairs, collectgarbage.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::dispatch::describe_register;
use super::frame::{Frame, Pending};
use super::util::*;
use super::*;
use crate::lua_bytecode::{Proto, Reg};

impl LuaRuntime {
    pub(super) fn call_native_core(
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
            NativeFunction::Print => {
                for (index, value) in args.iter().enumerate() {
                    if index != 0 {
                        self.output.push(b'\t');
                    }
                    let rendered = self.render_value(value.clone())?;
                    self.output.extend(rendered);
                }
                self.output.push(b'\n');
                Ok(Vec::new())
            }
            NativeFunction::Assert => {
                let value = required(0)?;
                if value.truthy() {
                    Ok(args)
                } else {
                    // Real Lua's `assert` re-raises the explicit `message`
                    // argument verbatim if given (preserving its exact type,
                    // e.g. a table), and only falls back to the literal
                    // string "assertion failed!" when it's omitted - it
                    // never uses the (falsy) condition value itself as the
                    // error message. Both cases are a plain tail call into
                    // `luaB_error` in real Lua (`lbaselib.c`'s
                    // `luaB_assert`), so a string message - explicit or the
                    // default - gets the same level-1 position prefix
                    // `error()` adds (see `where_prefix`), using assert's own
                    // caller as the position since the C-level tail call
                    // shares one Lua-visible call frame.
                    let prefix = self.where_prefix(1).unwrap_or_default();
                    Err(match args.get(1) {
                        Some(LuaValue::String(bytes)) => {
                            let message =
                                format!("{prefix}{}", String::from_utf8_lossy(bytes.as_bytes()));
                            LuaError::raised(
                                LuaValue::String(self.fresh_str(message.clone().into_bytes())),
                                message,
                            )
                        }
                        // An explicit `nil` message (as opposed to an
                        // omitted one, handled by the `None` arm below)
                        // still reaches `luaB_error` in real Lua, whose
                        // `luaG_errormsg` turns a nil error object into the
                        // literal string "<no error object>" - see
                        // `NativeFunction::Error`'s identical `LuaValue::Nil`
                        // arm just above this match.
                        Some(LuaValue::Nil) => {
                            let text: &[u8] = b"<no error object>";
                            LuaError::raised(
                                LuaValue::String(self.intern_str(text.to_vec())),
                                "<no error object>",
                            )
                        }
                        Some(message) => {
                            let display =
                                String::from_utf8_lossy(&message.display_bytes()).into_owned();
                            LuaError::raised(message.clone(), display)
                        }
                        None => {
                            let message = format!("{prefix}assertion failed!");
                            LuaError::raised(
                                LuaValue::String(self.fresh_str(message.clone().into_bytes())),
                                message,
                            )
                        }
                    })
                }
            }
            NativeFunction::Type => Ok(vec![LuaValue::String(self.intern_str(
                required(0)?.type_name().as_bytes().to_vec(),
            ))]),
            NativeFunction::ToString => {
                let value = required(0)?;
                let rendered = self.render_value(value)?;
                Ok(vec![LuaValue::String(self.fresh_str(rendered))])
            }
            NativeFunction::ToNumber => {
                let value = required(0)?;
                let base = args.get(1).map(|value| self.integer(value)).transpose()?;
                let converted = match (value, base) {
                    (value @ (LuaValue::Integer(_) | LuaValue::Float(_)), None) => value,
                    (LuaValue::String(bytes), None) => match parse_lua_number(bytes.as_bytes()) {
                        Some(Number::Integer(value)) => LuaValue::Integer(value),
                        Some(Number::Float(value)) => LuaValue::Float(value),
                        None => LuaValue::Nil,
                    },
                    (LuaValue::String(bytes), Some(base @ 2..=36)) => {
                        let text = String::from_utf8_lossy(bytes.as_bytes());
                        let text = text.trim();
                        let (negative, digits) = text
                            .strip_prefix('-')
                            .map(|digits| (true, digits))
                            .unwrap_or((false, text));
                        match i64::from_str_radix(digits, base as u32) {
                            Ok(value) => LuaValue::Integer(if negative {
                                value.wrapping_neg()
                            } else {
                                value
                            }),
                            Err(_) => LuaValue::Nil,
                        }
                    }
                    (_, Some(base)) if !(2..=36).contains(&base) => {
                        return Err(LuaError::new("base out of range"));
                    }
                    _ => LuaValue::Nil,
                };
                Ok(vec![converted])
            }
            NativeFunction::RawGet => self
                .raw_index(required(0)?, required(1)?)
                .map(|value| vec![value]),
            NativeFunction::RawSet => {
                let table = required(0)?;
                self.raw_set_index(table.clone(), required(1)?, required(2)?, None)?;
                Ok(vec![table])
            }
            NativeFunction::RawEqual => Ok(vec![LuaValue::Bool(required(0)? == required(1)?)]),
            NativeFunction::RawLen => match required(0)? {
                LuaValue::String(value) => Ok(vec![LuaValue::Integer(value.len() as i64)]),
                LuaValue::Table(value) => Ok(vec![LuaValue::Integer(self.table_len(value) as i64)]),
                value => Err(LuaError::new(format!(
                    "attempt to get length of a {} value",
                    value.type_name()
                ))),
            },
            NativeFunction::GetMetatable => {
                let value = required(0)?;
                if let Some(protected) = self.metamethod(&value, b"__metatable")? {
                    return Ok(vec![protected]);
                }
                match value {
                    LuaValue::Table(table) => Ok(vec![self
                        .table_metatable(table)
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil)]),
                    LuaValue::String(_) => Ok(vec![LuaValue::Table(self.string_metatable.clone())]),
                    LuaValue::Integer(_) | LuaValue::Float(_) => Ok(vec![self
                        .number_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil)]),
                    LuaValue::Bool(_) => Ok(vec![self
                        .boolean_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil)]),
                    LuaValue::Nil => Ok(vec![self
                        .nil_metatable
                        .clone()
                        .map(LuaValue::Table)
                        .unwrap_or(LuaValue::Nil)]),
                    LuaValue::Userdata(userdata) => {
                        let metatable = {
                            let heap = self.canonical_heap.borrow();
                            heap.userdata(userdata.object_id())
                                .ok()
                                .and_then(|value| value.metatable)
                        };
                        Ok(vec![metatable
                            .map(|metatable| {
                                LuaValue::CanonicalTable(CanonicalTable::root_existing(
                                    self.canonical_heap.clone(),
                                    metatable,
                                ))
                            })
                            .unwrap_or(LuaValue::Nil)])
                    }
                    _ => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::SetMetatable => {
                let value = required(0)?;
                let metatable = required(1)?;
                let LuaValue::Table(table) = value else {
                    return Err(LuaError::new("setmetatable expects a table"));
                };
                if self.metamethod(&value, b"__metatable")?.is_some() {
                    return Err(LuaError::new("cannot change a protected metatable"));
                }
                let new_metatable = match metatable {
                    LuaValue::Nil => None,
                    LuaValue::Table(meta) => Some(meta),
                    _ => return Err(LuaError::new("metatable must be a table or nil")),
                };
                self.table_set_metatable(table, new_metatable)?;
                self.table_sync_weak_mode(table)?;
                Ok(vec![value])
            }
            NativeFunction::Error => {
                // `error`'s message argument is optional (defaults to nil,
                // unlike most natives' required arguments) - and real Lua's
                // `luaG_errormsg` turns a nil error object into the literal
                // string "<no error object>" at the moment it is thrown, so
                // that's what `pcall`/`xpcall` observe, not a raw nil.
                let value = args.first().cloned().unwrap_or(LuaValue::Nil);
                // `level` (default 1) selects which active frame's position
                // real Lua's `luaL_where` reports: level 1 is the function
                // that called `error`, i.e. the topmost frame already on
                // `self.frames` (this native call's own caller, pushed back
                // before dispatch - same convention `DebugGetinfo` documents
                // above). Only a string message gets a position prefix
                // (`luaB_error`'s `lua_type(L, 1) == LUA_TSTRING` check) and
                // only when `level > 0`.
                let level = match args.get(1) {
                    Some(value) => coerce_integer(value)?,
                    None => 1,
                };
                Err(match &value {
                    LuaValue::Nil => {
                        let text: &[u8] = b"<no error object>";
                        LuaError::raised(
                            LuaValue::String(self.intern_str(text.to_vec())),
                            "<no error object>",
                        )
                    }
                    LuaValue::String(bytes) => {
                        let message = String::from_utf8_lossy(bytes.as_bytes()).into_owned();
                        let message = match self.where_prefix(level) {
                            Some(prefix) => format!("{prefix}{message}"),
                            None => message,
                        };
                        LuaError::raised(
                            LuaValue::String(self.fresh_str(message.clone().into_bytes())),
                            message,
                        )
                    }
                    other => {
                        let message = format!("(error object is a {} value)", other.type_name());
                        LuaError::raised(value, message)
                    }
                })
            }
            NativeFunction::PCall => {
                let function = required(0)?;
                match self.call(function, args.into_iter().skip(1).collect()) {
                    Ok(mut values) => {
                        values.insert(0, LuaValue::Bool(true));
                        Ok(values)
                    }
                    Err(error) => Ok(vec![
                        LuaValue::Bool(false),
                        error.into_lua_value(&self.canonical_heap),
                    ]),
                }
            }
            NativeFunction::XCall => {
                let function = required(0)?;
                let handler = required(1)?;
                let extra_args = args.into_iter().skip(2).collect();
                match self.call(function, extra_args) {
                    Ok(mut values) => {
                        values.insert(0, LuaValue::Bool(true));
                        Ok(values)
                    }
                    Err(error) => {
                        let error_value = error.into_lua_value(&self.canonical_heap);
                        let handled = self
                            .call(handler, vec![error_value])?
                            .into_iter()
                            .next()
                            .unwrap_or(LuaValue::Nil);
                        Ok(vec![LuaValue::Bool(false), handled])
                    }
                }
            }
            NativeFunction::Select => {
                let selector = required(0)?;
                if selector == LuaValue::String(self.intern_str(b"#".to_vec())) {
                    return Ok(vec![LuaValue::Integer(args.len().saturating_sub(1) as i64)]);
                }
                let index = self.integer(&selector)?;
                if index == 0 {
                    return Err(LuaError::new(
                        "bad argument #1 to 'select' (index out of range)",
                    ));
                }
                let start = if index > 0 {
                    index as usize
                } else {
                    (args.len() as i64 + index).max(1) as usize
                };
                Ok(args.into_iter().skip(start).collect())
            }
            NativeFunction::Next => {
                let table = required(0)?;
                let key = args.get(1).cloned().unwrap_or(LuaValue::Nil);
                self.next(table, key)
            }
            NativeFunction::Pairs => {
                let table = required(0)?;
                if let Some(method) = self.metamethod(&table, b"__pairs")? {
                    return self.call(method, vec![table]);
                }
                self.expect_table(&table)?;
                Ok(vec![
                    LuaValue::NativeFunction(NativeFunction::Next),
                    table,
                    LuaValue::Nil,
                ])
            }
            NativeFunction::IPairs => {
                let table = required(0)?;
                self.expect_table(&table)?;
                Ok(vec![
                    LuaValue::NativeFunction(NativeFunction::IPairsIterator),
                    table,
                    LuaValue::Integer(0),
                ])
            }
            NativeFunction::IPairsIterator => {
                let table = required(0)?;
                let next = self.integer(&required(1)?)?.wrapping_add(1);
                let value = self.index_get(table, LuaValue::Integer(next))?;
                if value == LuaValue::Nil {
                    Ok(vec![LuaValue::Nil])
                } else {
                    Ok(vec![LuaValue::Integer(next), value])
                }
            }
            NativeFunction::CollectGarbage => {
                let option = match args.first() {
                    Some(value) => self.string(value)?.to_vec(),
                    None => b"collect".to_vec(),
                };
                match option.as_slice() {
                    b"count" => {
                        let used = self.live_heap_bytes() as f64 / 1024.0;
                        Ok(vec![LuaValue::Float(used)])
                    }
                    b"collect" => {
                        self.collect_garbage();
                        Ok(vec![LuaValue::Integer(0)])
                    }
                    b"step" => {
                        self.collect_garbage();
                        // No real incremental stepping exists - each "step"
                        // call already runs a full `collect_major_with_roots`
                        // pass, so it always finishes a collection cycle
                        // immediately (unlike real Lua, where finishing a
                        // cycle can take many steps).
                        Ok(vec![LuaValue::Bool(true)])
                    }
                    // No real incremental/generational collector mode
                    // difference exists yet ("collect" and "step" above
                    // already always run a full major collection pass) -
                    // but real Lua's `lua_gc` still
                    // returns the *previous* mode name when switching, and
                    // scripts assert on it, so that bookkeeping is tracked
                    // for real even though it doesn't change behavior.
                    b"incremental" | b"generational" => {
                        let previous = self.gc_mode;
                        self.gc_mode = if option.as_slice() == b"incremental" {
                            "incremental"
                        } else {
                            "generational"
                        };
                        Ok(vec![LuaValue::String(self.intern_str(
                            previous.as_bytes().to_vec(),
                        ))])
                    }
                    b"stop" => {
                        self.gc_running = false;
                        Ok(vec![LuaValue::Integer(0)])
                    }
                    b"restart" => {
                        self.gc_running = true;
                        Ok(vec![LuaValue::Integer(0)])
                    }
                    b"isrunning" => Ok(vec![LuaValue::Bool(self.gc_running)]),
                    b"param" => {
                        let param = args.get(1).map(|value| self.string(value)).transpose()?;
                        let is_pause = match param.as_deref() {
                            Some(b"pause") => true,
                            Some(b"stepmul") => false,
                            // Other real Lua GC params ("stepsize",
                            // "minormul", "majorminor", "minormajor") have
                            // no effect on Sol's collector either, but are
                            // accepted and echoed back as 0 rather than
                            // erroring, same tolerance as "incremental"/
                            // "generational" above.
                            Some(_) => return Ok(vec![LuaValue::Integer(0)]),
                            None => {
                                return Err(LuaError::new(
                                    "bad argument #2 to 'collectgarbage' (value expected)",
                                ));
                            }
                        };
                        let new_value = args.get(2).map(|value| self.integer(value)).transpose()?;
                        let slot = if is_pause {
                            &mut self.gc_pause
                        } else {
                            &mut self.gc_stepmul
                        };
                        let previous = *slot;
                        if let Some(new_value) = new_value {
                            *slot = new_value;
                        }
                        Ok(vec![LuaValue::Integer(previous)])
                    }
                    _ => Err(LuaError::new(format!(
                        "invalid option '{}' to 'collectgarbage'",
                        String::from_utf8_lossy(&option)
                    ))),
                }
            }
            _ => unreachable!("call_native_core received a non-core NativeFunction"),
        }
    }

    pub(super) fn render_value(&mut self, value: LuaValue) -> LuaResult<Vec<u8>> {
        if let Some(method) = self.metamethod(&value, b"__tostring")? {
            let rendered = self
                .call(method, vec![value])?
                .into_iter()
                .next()
                .unwrap_or(LuaValue::Nil);
            return match rendered {
                LuaValue::String(bytes) => Ok(bytes.as_bytes().to_vec()),
                _ => Err(LuaError::new("'__tostring' must return a string")),
            };
        }
        if let LuaValue::Table(table) = &value {
            let metatable = self.table_metatable(*table);
            if let Some(metatable) = metatable {
                let name = self.table_get(
                    metatable,
                    &LuaValue::String(self.intern_str(b"__name".to_vec())),
                )?;
                if let LuaValue::String(name) = name {
                    return Ok(format!(
                        "{}: 0x{:x}",
                        String::from_utf8_lossy(name.as_bytes()),
                        value.identity_address().unwrap()
                    )
                    .into_bytes());
                }
            }
        }
        Ok(value.display_bytes())
    }

    /// Real Lua's `luaL_where`: the "{short_src}:{line}: " position prefix
    /// for the frame `level` levels up the call stack (level 1 is this
    /// native call's own caller, matching `DebugGetinfo`'s numeric-level
    /// convention above), or `None` when that frame has no line info - a
    /// native/C frame, a level past the bottom of the stack, or a Lua frame
    /// whose chunk has no registered source - exactly `luaL_where`'s own
    /// silent fallback to an empty prefix.
    fn where_prefix(&self, level: i64) -> Option<String> {
        if level <= 0 {
            return None;
        }
        let mut remaining = level;
        let mut found = None;
        for frame in self.frames.iter().rev() {
            remaining -= 1;
            if remaining == 0 {
                found = Some(frame);
                break;
            }
        }
        let Frame::Lua(lua_frame) = found? else {
            return None;
        };
        let line = lua_frame
            .proto
            .source_map
            .location(lua_frame.header.pc)
            .map(|location| location.line as i64)?;
        if line <= 0 {
            return None;
        }
        let source = self
            .chunk_sources
            .get(&(Rc::as_ptr(&lua_frame.proto) as usize))?;
        let short_src = Self::short_src(source);
        Some(format!("{}:{line}: ", String::from_utf8_lossy(&short_src)))
    }

    /// Real Lua's `luaG_addinfo`/`luaG_runerror`: the "{short_src}:{line}: "
    /// position prefix a runtime error the VM itself synthesizes (arithmetic
    /// on the wrong type, calling a non-callable, indexing nil, and so on)
    /// picks up automatically at the innermost Lua frame it's raised from -
    /// distinct from `where_prefix`, which only covers explicit `error()`/
    /// `assert()` calls (both always carry `LuaError::value`, so the
    /// dispatch-loop unwind site that calls this only does so when `value`
    /// is `None`, i.e. never for those). A frame whose proto has no source
    /// map at all (`string.dump(f, true)`'s stripped debug info, or
    /// `load(string.dump(...))`'s reload of it) has no line to report, so
    /// real Lua substitutes the literal two-character "?:?: " for both the
    /// source and line fields (`ldebug.c`'s "no source available" fallback)
    /// rather than omitting the prefix.
    pub(super) fn runtime_error_prefix(&self, proto: &Rc<Proto>, pc: u32) -> Option<String> {
        if proto.source_map.is_empty() {
            return Some("?:?: ".to_string());
        }
        let line = proto.source_map.location(pc)?.line as i64;
        if line <= 0 {
            return None;
        }
        let source = self.chunk_sources.get(&(Rc::as_ptr(proto) as usize))?;
        let short_src = Self::short_src(source);
        Some(format!("{}:{line}: ", String::from_utf8_lossy(&short_src)))
    }

    /// One `debug.traceback` frame entry's position, in real Lua's own
    /// `source:line:` shape (`lauxlib.c`'s `lastlevel`/`luaL_traceback`,
    /// which formats each frame as `"%s:%d:"` before appending an
    /// `" in ..."` descriptor) - reusing `runtime_error_prefix`'s
    /// `chunk_sources` lookup so scripts that parse a traceback with a
    /// `":(%d+):"`-style pattern (as real Lua's own manual examples do)
    /// find the same colon-delimited shape here. Falls back to a bare
    /// `"line {line}"` (no colons) when the source can't be resolved,
    /// matching this call's prior behavior for that case.
    pub(super) fn traceback_frame_label(&self, proto: &Rc<Proto>, line: u32) -> String {
        match self.chunk_sources.get(&(Rc::as_ptr(proto) as usize)) {
            Some(source) => {
                let short_src = Self::short_src(source);
                format!("{}:{line}:", String::from_utf8_lossy(&short_src))
            }
            None => format!("line {line}"),
        }
    }

    /// Real Lua's `funcnamefromcode` (`ldebug.c`): how the frame at `level`
    /// was *referred to by its caller* at the call site - resolved from the
    /// caller's own bytecode via `describe_register`, not from the callee's
    /// declared name. `level` uses the same 1-based, `DebugGetinfo`-
    /// matching convention as `where_prefix` (level 1 is the topmost
    /// frame). Returns `None` when there is no caller frame (the outermost
    /// frame on the stack), the caller is a native frame, or the caller
    /// isn't paused on an ordinary call (`Pending::Call` - a tail call
    /// reuses the caller's own frame instead of leaving one behind to
    /// inspect, so it has no call-site register to resolve from) - matching
    /// real Lua's own `nil`/`""` fallback whenever it can't determine a
    /// name.
    pub(super) fn call_site_name(&self, level: i64) -> Option<(&'static str, String)> {
        if level <= 0 {
            return None;
        }
        let mut remaining = level + 1;
        let mut found = None;
        for frame in self.frames.iter().rev() {
            remaining -= 1;
            if remaining == 0 {
                found = Some(frame);
                break;
            }
        }
        let Frame::Lua(caller) = found? else {
            return None;
        };
        let Pending::Call { base, .. } = caller.pending else {
            return None;
        };
        describe_register(&caller.proto, caller.header.pc as usize, base as Reg)
    }
}
