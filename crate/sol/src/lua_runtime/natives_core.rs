//! core primitives: print/type/tostring/tonumber, raw table access,
//! metatables, error/pcall/xpcall, select/next/pairs/ipairs, collectgarbage.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::util::*;
use super::*;

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
                    // error message.
                    Err(match args.get(1) {
                        Some(message) => {
                            let display =
                                String::from_utf8_lossy(&message.display_bytes()).into_owned();
                            LuaError::raised(message.clone(), display)
                        }
                        None => LuaError::new("assertion failed!"),
                    })
                }
            }
            NativeFunction::Type => Ok(vec![LuaValue::String(Rc::new(
                required(0)?.type_name().as_bytes().to_vec(),
            ))]),
            NativeFunction::ToString => {
                let value = required(0)?;
                Ok(vec![LuaValue::String(Rc::new(self.render_value(value)?))])
            }
            NativeFunction::ToNumber => {
                let value = required(0)?;
                let base = args.get(1).map(|value| self.integer(value)).transpose()?;
                let converted = match (value, base) {
                    (value @ (LuaValue::Integer(_) | LuaValue::Float(_)), None) => value,
                    (LuaValue::String(bytes), None) => match parse_lua_number(&bytes) {
                        Some(Number::Integer(value)) => LuaValue::Integer(value),
                        Some(Number::Float(value)) => LuaValue::Float(value),
                        None => LuaValue::Nil,
                    },
                    (LuaValue::String(bytes), Some(base @ 2..=36)) => {
                        let text = String::from_utf8_lossy(&bytes);
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
                self.raw_set_index(table.clone(), required(1)?, required(2)?)?;
                Ok(vec![table])
            }
            NativeFunction::RawEqual => Ok(vec![LuaValue::Bool(required(0)? == required(1)?)]),
            NativeFunction::RawLen => match required(0)? {
                LuaValue::String(value) => Ok(vec![LuaValue::Integer(value.len() as i64)]),
                LuaValue::Table(value) => Ok(vec![LuaValue::Integer(value.borrow().len() as i64)]),
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
                    LuaValue::Table(table) => Ok(vec![table
                        .borrow()
                        .metatable
                        .clone()
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
                    _ => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::SetMetatable => {
                let value = required(0)?;
                let metatable = required(1)?;
                let LuaValue::Table(table_rc) = value.clone() else {
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
                let is_weak = new_metatable
                    .as_ref()
                    .map(|meta| table_weak_mode(meta) != (false, false))
                    .unwrap_or(false);
                {
                    let mut table = table_rc.borrow_mut();
                    table.metatable = new_metatable;
                    table.version = table.version.wrapping_add(1);
                }
                if is_weak {
                    self.weak_tables.push(Rc::downgrade(&table_rc));
                }
                Ok(vec![value])
            }
            NativeFunction::Error => {
                // `error`'s message argument is optional (defaults to nil,
                // unlike most natives' required arguments) - and real Lua's
                // `luaG_errormsg` turns a nil error object into the literal
                // string "<no error object>" at the moment it is thrown, so
                // that's what `pcall`/`xpcall` observe, not a raw nil.
                let value = args.first().cloned().unwrap_or(LuaValue::Nil);
                Err(match &value {
                    LuaValue::Nil => {
                        let text: &[u8] = b"<no error object>";
                        LuaError::raised(
                            LuaValue::String(Rc::new(text.to_vec())),
                            "<no error object>",
                        )
                    }
                    LuaValue::String(bytes) => {
                        let message = String::from_utf8_lossy(bytes).into_owned();
                        LuaError::raised(value, message)
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
                    Err(error) => Ok(vec![LuaValue::Bool(false), error.into_lua_value()]),
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
                        let handled = self
                            .call(handler, vec![error.into_lua_value()])?
                            .into_iter()
                            .next()
                            .unwrap_or(LuaValue::Nil);
                        Ok(vec![LuaValue::Bool(false), handled])
                    }
                }
            }
            NativeFunction::Select => {
                let selector = required(0)?;
                if selector == LuaValue::String(Rc::new(b"#".to_vec())) {
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
                        self.sweep_weak_tables();
                        self.collect_cycles();
                        Ok(vec![LuaValue::Integer(0)])
                    }
                    b"step" => {
                        self.sweep_weak_tables();
                        self.collect_cycles();
                        // No real incremental stepping exists - each "step"
                        // call already runs a full trial-deletion cycle
                        // collection pass, so it always finishes a
                        // collection cycle immediately (unlike real Lua,
                        // where finishing a cycle can take many steps).
                        Ok(vec![LuaValue::Bool(true)])
                    }
                    // No real incremental/generational collector mode
                    // difference exists yet ("collect" and "step" above
                    // already always run a full trial-deletion cycle
                    // collection pass) - but real Lua's `lua_gc` still
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
                        Ok(vec![LuaValue::String(Rc::new(
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
                LuaValue::String(bytes) => Ok(bytes.as_ref().clone()),
                _ => Err(LuaError::new("'__tostring' must return a string")),
            };
        }
        if let LuaValue::Table(table) = &value {
            let metatable = table.borrow().metatable.clone();
            if let Some(metatable) = metatable {
                let name = metatable
                    .borrow()
                    .get(&LuaValue::String(Rc::new(b"__name".to_vec())))?;
                if let LuaValue::String(name) = name {
                    return Ok(format!(
                        "{}: 0x{:x}",
                        String::from_utf8_lossy(&name),
                        value.identity_address().unwrap()
                    )
                    .into_bytes());
                }
            }
        }
        Ok(value.display_bytes())
    }
}
