//! the os and io libraries.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::util::*;
use super::*;

impl LuaRuntime {
    pub(super) fn call_native_os_io(
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
            NativeFunction::OsTime => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or(0);
                Ok(vec![LuaValue::Integer(now)])
            }
            NativeFunction::OsClock => Ok(vec![LuaValue::Float(
                self.start_time.elapsed().as_secs_f64(),
            )]),
            NativeFunction::OsDifftime => {
                let t2 = number_as_f64(&required(0)?)?;
                let t1 = number_as_f64(&required(1)?)?;
                Ok(vec![LuaValue::Float(t2 - t1)])
            }
            NativeFunction::OsDate => {
                let format = match args.first() {
                    Some(value) => self.string(value)?.to_vec(),
                    None => b"%c".to_vec(),
                };
                let format = format
                    .strip_prefix(b"!")
                    .map(<[u8]>::to_vec)
                    .unwrap_or(format);
                let time = match args.get(1) {
                    Some(value) => self.integer(value)?,
                    None => std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_secs() as i64)
                        .unwrap_or(0),
                };
                let days = time.div_euclid(86_400);
                let secs_of_day = time.rem_euclid(86_400);
                let (year, month, day) = civil_from_days(days);
                let hour = (secs_of_day / 3600) as u32;
                let minute = ((secs_of_day % 3600) / 60) as u32;
                let second = (secs_of_day % 60) as u32;
                let wday = (days.rem_euclid(7) + 4).rem_euclid(7) as u32; // 0 = Sunday
                let yday = (days - days_from_civil(year, 1, 1) + 1) as u32;

                if format == b"*t" {
                    let mut table = LuaTable::default();
                    for (key, value) in [
                        ("year", LuaValue::Integer(year)),
                        ("month", LuaValue::Integer(month as i64)),
                        ("day", LuaValue::Integer(day as i64)),
                        ("hour", LuaValue::Integer(hour as i64)),
                        ("min", LuaValue::Integer(minute as i64)),
                        ("sec", LuaValue::Integer(second as i64)),
                        ("wday", LuaValue::Integer(wday as i64 + 1)),
                        ("yday", LuaValue::Integer(yday as i64)),
                        ("isdst", LuaValue::Bool(false)),
                    ] {
                        table
                            .set(LuaValue::String(Rc::new(key.as_bytes().to_vec())), value)
                            .unwrap();
                    }
                    return Ok(vec![LuaValue::Table(
                        self.track_table(Rc::new(RefCell::new(table))),
                    )]);
                }

                const WEEKDAYS: [&str; 7] = [
                    "Sunday",
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                ];
                const MONTHS: [&str; 12] = [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];
                let wday_name = WEEKDAYS[wday as usize];
                let month_name = MONTHS[(month - 1) as usize];
                let mut rendered = String::new();
                let mut chars = format.iter().copied();
                while let Some(byte) = chars.next() {
                    if byte != b'%' {
                        rendered.push(byte as char);
                        continue;
                    }
                    match chars.next() {
                        Some(b'Y') => rendered.push_str(&year.to_string()),
                        Some(b'y') => rendered.push_str(&format!("{:02}", year.rem_euclid(100))),
                        Some(b'm') => rendered.push_str(&format!("{month:02}")),
                        Some(b'd') => rendered.push_str(&format!("{day:02}")),
                        Some(b'H') => rendered.push_str(&format!("{hour:02}")),
                        Some(b'M') => rendered.push_str(&format!("{minute:02}")),
                        Some(b'S') => rendered.push_str(&format!("{second:02}")),
                        Some(b'p') => rendered.push_str(if hour < 12 { "AM" } else { "PM" }),
                        Some(b'A') => rendered.push_str(wday_name),
                        Some(b'a') => rendered.push_str(&wday_name[..3]),
                        Some(b'B') => rendered.push_str(month_name),
                        Some(b'b') => rendered.push_str(&month_name[..3]),
                        Some(b'j') => rendered.push_str(&format!("{yday:03}")),
                        Some(b'c') => rendered.push_str(&format!(
                            "{} {} {day:2} {hour:02}:{minute:02}:{second:02} {year}",
                            &wday_name[..3],
                            &month_name[..3],
                        )),
                        Some(b'%') => rendered.push('%'),
                        Some(other) => {
                            rendered.push('%');
                            rendered.push(other as char);
                        }
                        None => rendered.push('%'),
                    }
                }
                self.charge_allocation(rendered.len())?;
                Ok(vec![LuaValue::String(Rc::new(rendered.into_bytes()))])
            }
            NativeFunction::OsGetenv => {
                let name = self.string(&required(0)?)?.to_vec();
                let name = String::from_utf8_lossy(&name).into_owned();
                match std::env::var(&name) {
                    Ok(value) => Ok(vec![LuaValue::String(Rc::new(value.into_bytes()))]),
                    Err(_) => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::OsExit => {
                let code = match args.first() {
                    None | Some(LuaValue::Bool(true)) => 0,
                    Some(LuaValue::Bool(false)) => 1,
                    Some(value) => self.integer(value)? as i32,
                };
                std::process::exit(code);
            }
            NativeFunction::IoWrite => {
                let target = self.default_output.borrow().clone();
                self.write_values_to_target(&target, &args)?;
                Ok(vec![target])
            }
            NativeFunction::FileWrite => {
                let handle = required(0)?;
                self.write_values_to_target(&handle, &args[1..])?;
                Ok(vec![handle])
            }
            NativeFunction::IoOutput => match args.first() {
                None => Ok(vec![self.default_output.borrow().clone()]),
                Some(LuaValue::String(path)) => {
                    if !self.capabilities.filesystem {
                        return Err(LuaError::new(
                            "filesystem capability is disabled; enable it explicitly",
                        ));
                    }
                    let handle = self.open_output_file(path)?;
                    *self.default_output.borrow_mut() = handle.clone();
                    Ok(vec![handle])
                }
                Some(handle @ LuaValue::Table(_)) => {
                    *self.default_output.borrow_mut() = handle.clone();
                    Ok(vec![handle.clone()])
                }
                Some(other) => Err(LuaError::new(format!(
                    "bad argument #1 to 'output' (string expected, got {})",
                    other.type_name()
                ))),
            },
            NativeFunction::FileClose => {
                let handle = match args.first() {
                    Some(value) => value.clone(),
                    None => self.default_output.borrow().clone(),
                };
                if let LuaValue::Table(table) = &handle {
                    let key = Rc::as_ptr(table) as usize;
                    if let Some(file) = self.open_files.borrow_mut().remove(&key) {
                        use std::io::Write;
                        file.borrow_mut()
                            .flush()
                            .map_err(|error| LuaError::new(format!("{error}")))?;
                    }
                }
                Ok(vec![LuaValue::Bool(true)])
            }
            NativeFunction::OsRemove => {
                let path = self.string(&required(0)?)?.to_vec();
                let real_path =
                    bytes_to_path(&path).ok_or_else(|| LuaError::new("invalid file name"))?;
                match std::fs::remove_file(&real_path) {
                    Ok(()) => Ok(vec![LuaValue::Bool(true)]),
                    Err(error) => Ok(vec![
                        LuaValue::Nil,
                        LuaValue::String(Rc::new(
                            format!("{}: {error}", String::from_utf8_lossy(&path)).into_bytes(),
                        )),
                    ]),
                }
            }
            NativeFunction::OsSetlocale => {
                // Sol has no real locale-sensitive collation/character
                // classification (string comparison and `%a`/`%l`/`%u`
                // pattern classes are always plain-ASCII/byte-oriented, as
                // required elsewhere in this crate), so this only tracks a
                // bookkeeping current-locale name rather than actually
                // switching any OS-level locale. Real Lua's `os.setlocale`:
                // a nil/absent `locale` argument means "just query", "" (or
                // "C") means the portable default locale (always available),
                // and any other named locale requires OS/libc support Sol
                // doesn't have, so it fails - the same outcome real Lua
                // would have on a system where that named locale isn't
                // installed (which `strings.lua`'s `trylocale` helper is
                // itself written to tolerate).
                let locale = match args.first() {
                    None | Some(LuaValue::Nil) => None,
                    Some(value) => Some(self.string(value)?.to_vec()),
                };
                match locale {
                    None => Ok(vec![LuaValue::String(Rc::new(
                        self.current_locale.clone().into_bytes(),
                    ))]),
                    Some(name) if name.is_empty() || name == b"C" => {
                        self.current_locale = "C".to_string();
                        Ok(vec![LuaValue::String(Rc::new(b"C".to_vec()))])
                    }
                    Some(_) => Ok(vec![LuaValue::Nil]),
                }
            }
            NativeFunction::OsTmpname => {
                // Real Lua's `os.tmpname` (`tmpnam`/`mkstemp` under the
                // hood) returns the name of an already-created, empty,
                // unique temporary file - callers are expected to
                // `os.remove` it themselves when done (see `verybig.lua`'s
                // `local file = os.tmpname(); io.output(file); ...;
                // os.remove(file)`). `std::env::temp_dir` plus a
                // process-and-counter-unique suffix mirrors that: the file
                // is actually created here (not just a name computed) so a
                // subsequent `io.output(file)` can open it for writing
                // exactly like a real freshly-`mkstemp`'d file.
                self.tmpname_counter += 1;
                let path = std::env::temp_dir().join(format!(
                    "lua_{}_{}.tmp",
                    std::process::id(),
                    self.tmpname_counter
                ));
                std::fs::File::create(&path).map_err(|error| {
                    LuaError::new(format!("unable to generate a unique filename: {error}"))
                })?;
                Ok(vec![LuaValue::String(Rc::new(
                    path.to_string_lossy().into_owned().into_bytes(),
                ))])
            }
            NativeFunction::IoRead => {
                let format = match args.first() {
                    Some(value) => self.string(value)?.to_vec(),
                    None => b"l".to_vec(),
                };
                let format = format
                    .strip_prefix(b"*")
                    .map(<[u8]>::to_vec)
                    .unwrap_or(format);
                use std::io::BufRead;
                let stdin = std::io::stdin();
                match format.as_slice() {
                    b"l" | b"L" => {
                        let mut line = String::new();
                        let bytes_read = stdin
                            .lock()
                            .read_line(&mut line)
                            .map_err(|error| LuaError::new(format!("io.read: {error}")))?;
                        if bytes_read == 0 {
                            return Ok(vec![LuaValue::Nil]);
                        }
                        if format == b"l" {
                            while line.ends_with(['\n', '\r']) {
                                line.pop();
                            }
                        }
                        self.charge_allocation(line.len())?;
                        Ok(vec![LuaValue::String(Rc::new(line.into_bytes()))])
                    }
                    b"a" => {
                        let mut buffer = String::new();
                        std::io::Read::read_to_string(&mut stdin.lock(), &mut buffer)
                            .map_err(|error| LuaError::new(format!("io.read: {error}")))?;
                        self.charge_allocation(buffer.len())?;
                        Ok(vec![LuaValue::String(Rc::new(buffer.into_bytes()))])
                    }
                    b"n" => {
                        let mut line = String::new();
                        let bytes_read = stdin
                            .lock()
                            .read_line(&mut line)
                            .map_err(|error| LuaError::new(format!("io.read: {error}")))?;
                        if bytes_read == 0 {
                            return Ok(vec![LuaValue::Nil]);
                        }
                        let trimmed = line.trim();
                        match (trimmed.parse::<i64>(), trimmed.parse::<f64>()) {
                            (Ok(value), _) => Ok(vec![LuaValue::Integer(value)]),
                            (_, Ok(value)) => Ok(vec![LuaValue::Float(value)]),
                            _ => Ok(vec![LuaValue::Nil]),
                        }
                    }
                    _ => Err(LuaError::new(format!(
                        "invalid format '{}' to 'io.read'",
                        String::from_utf8_lossy(&format)
                    ))),
                }
            }
            _ => unreachable!("call_native_os_io received a non-os_io NativeFunction"),
        }
    }

    fn write_values_to_target(&mut self, target: &LuaValue, values: &[LuaValue]) -> LuaResult<()> {
        let mut bytes = Vec::new();
        for value in values {
            match value {
                LuaValue::String(rendered) => bytes.extend_from_slice(rendered),
                LuaValue::Integer(_) | LuaValue::Float(_) => {
                    bytes.extend(value.display_bytes());
                }
                other => {
                    return Err(LuaError::new(format!(
                        "invalid argument to 'write' (string expected, got {})",
                        other.type_name()
                    )))
                }
            }
        }
        if let LuaValue::Table(table) = target {
            let key = Rc::as_ptr(table) as usize;
            if let Some(file) = self.open_files.borrow().get(&key).cloned() {
                use std::io::Write;
                file.borrow_mut()
                    .write_all(&bytes)
                    .map_err(|error| LuaError::new(format!("{error}")))?;
                return Ok(());
            }
            if Rc::as_ptr(table) == Rc::as_ptr(&self.io_stderr) {
                use std::io::Write;
                std::io::stderr()
                    .write_all(&bytes)
                    .map_err(|error| LuaError::new(format!("{error}")))?;
                return Ok(());
            }
        }
        self.output.extend(bytes);
        Ok(())
    }

    fn open_output_file(&mut self, path: &Rc<Vec<u8>>) -> LuaResult<LuaValue> {
        let real_path = bytes_to_path(path).ok_or_else(|| LuaError::new("invalid file name"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&real_path)
            .map_err(|error| {
                LuaError::new(format!("{}: {error}", String::from_utf8_lossy(path)))
            })?;
        let handle = Rc::new(RefCell::new(LuaTable::default()));
        handle
            .borrow_mut()
            .set(
                LuaValue::String(Rc::new(b"write".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileWrite),
            )
            .unwrap();
        handle
            .borrow_mut()
            .set(
                LuaValue::String(Rc::new(b"close".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileClose),
            )
            .unwrap();
        let handle = self.track_table(handle);
        let key = Rc::as_ptr(&handle) as usize;
        self.open_files
            .borrow_mut()
            .insert(key, Rc::new(RefCell::new(file)));
        Ok(LuaValue::Table(handle))
    }
}
