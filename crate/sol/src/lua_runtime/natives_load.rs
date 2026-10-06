//! load/dofile/require and package module loading.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use crate::ast::Function;
use crate::lua_bytecode::{Compiler, Proto};

use super::util::*;
use super::*;

/// The fixed prefix specified by Lua 5.5's `ldump.c`. Keeping this separate
/// from Sol's process-local dump envelope lets `string.unpack` clients and
/// `load`'s malformed-header checks see the same ABI shape as a native Lua
/// binary chunk.
pub(super) fn lua55_binary_chunk_header() -> Vec<u8> {
    let mut header = Vec::with_capacity(40);
    header.extend_from_slice(b"\x1bLua");
    header.push(0x55); // LUAC_VERSION
    header.push(0); // LUAC_FORMAT
    header.extend_from_slice(b"\x19\x93\r\n\x1a\n"); // LUAC_DATA
    header.push(std::mem::size_of::<i32>() as u8);
    header.extend_from_slice(&(-0x5678i32).to_ne_bytes()); // LUAC_INT
    header.push(4); // sizeof(Instruction)
    header.extend_from_slice(&0x1234_5678u32.to_ne_bytes()); // LUAC_INSTRUCTION
    header.push(std::mem::size_of::<i64>() as u8);
    header.extend_from_slice(&(-0x5678i64).to_ne_bytes()); // LUAC_INT
    header.push(std::mem::size_of::<f64>() as u8);
    header.extend_from_slice(&(-370.5f64).to_ne_bytes()); // LUAC_NUM
    header
}

pub(super) const SOL_DUMP_MAGIC: &[u8; 8] = b"SolDmp\0\0";

impl LuaRuntime {
    pub(super) fn call_native_load(
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
            NativeFunction::Require => self.require(required(0)?),
            NativeFunction::PackageSearchPath => {
                let name = self.string(&required(0)?)?.to_vec();
                let path = self.string(&required(1)?)?.to_vec();
                let sep = match args.get(2) {
                    Some(LuaValue::Nil) | None => b".".to_vec(),
                    Some(value) => self.string(value)?.to_vec(),
                };
                let dirsep = match args.get(3) {
                    Some(LuaValue::Nil) | None => default_dirsep(),
                    Some(value) => self.string(value)?.to_vec(),
                };
                match self.search_path_candidates(&name, &path, &sep, &dirsep) {
                    Ok(found) => Ok(vec![LuaValue::String(self.fresh_str(found))]),
                    Err(message) => Ok(vec![LuaValue::Nil, LuaValue::String(self.fresh_str(message))]),
                }
            }
            NativeFunction::PackageSearcherPreload => {
                let name = self.string(&required(0)?)?.to_vec();
                let key = LuaValue::String(self.intern_str(name.clone()));
                let preload_key = LuaValue::String(self.intern_str(b"preload"));
                let preload = self.table_get(self.package_table, &preload_key)?;
                if let LuaValue::Table(preload_table) = preload {
                    let loader = self.table_get(preload_table, &key)?;
                    if loader != LuaValue::Nil {
                        return Ok(vec![
                            loader,
                            LuaValue::String(self.intern_str(b":preload:")),
                        ]);
                    }
                }
                let mut message = b"\n\tno field package.preload['".to_vec();
                message.extend_from_slice(&name);
                message.extend_from_slice(b"']");
                Ok(vec![LuaValue::String(self.fresh_str(message))])
            }
            NativeFunction::PackageSearcherLua => {
                let name = self.string(&required(0)?)?.to_vec();
                match self.require_search_field(&name, b"path")? {
                    Ok(filename) => {
                        let real_path = bytes_to_path(&filename)
                            .ok_or_else(|| LuaError::new("invalid file name"))?;
                        let source = std::fs::read(&real_path).map_err(|error| {
                            LuaError::new(format!(
                                "cannot open '{}': {error}",
                                String::from_utf8_lossy(&filename)
                            ))
                        })?;
                        let closure = self.compile_chunk(&source, None).map_err(|error| {
                            LuaError::new(format!(
                                "error loading module '{}' from file '{}':\n\t{}",
                                String::from_utf8_lossy(&name),
                                String::from_utf8_lossy(&filename),
                                error
                            ))
                        })?;
                        Ok(vec![closure, LuaValue::String(self.fresh_str(filename))])
                    }
                    Err(message) => Ok(vec![LuaValue::String(self.fresh_str(message))]),
                }
            }
            NativeFunction::PackageSearcherC => {
                let name = self.string(&required(0)?)?.to_vec();
                self.search_native_loader(&name, &name)
            }
            NativeFunction::PackageSearcherCRoot => {
                let name = self.string(&required(0)?)?.to_vec();
                let Some(dot) = name.iter().position(|byte| *byte == b'.') else {
                    return Ok(Vec::new());
                };
                self.search_native_loader(&name[..dot], &name)
            }
            NativeFunction::PackageLoadLib => {
                let path = self.string(&required(0)?)?.to_vec();
                let symbol = self.string(&required(1)?)?.to_vec();
                if !self.capabilities.native_modules {
                    return Ok(vec![
                        LuaValue::Nil,
                        LuaValue::String(self.intern_str(b"native module capability is disabled")),
                        LuaValue::String(self.intern_str(b"absent")),
                    ]);
                }
                if symbol == b"*" {
                    return match c_api::load_native_library(&path) {
                        Ok(library) => {
                            self.native_libraries.push(library);
                            Ok(vec![LuaValue::Bool(true)])
                        }
                        Err(error) => Ok(vec![
                            LuaValue::Nil,
                            LuaValue::String(self.fresh_str(error.into_bytes())),
                            LuaValue::String(self.intern_str(b"open")),
                        ]),
                    };
                }
                match c_api::load_native_callable(&path, &symbol) {
                    Ok((library, function)) => {
                        let callable = self.register_c_function(function)?;
                        self.native_libraries.push(library);
                        Ok(vec![LuaValue::CFunction(CanonicalCFunction::allocate(
                            self.canonical_heap.clone(),
                            callable,
                            Vec::new(),
                        ))])
                    }
                    Err(error) => Ok(vec![
                        LuaValue::Nil,
                        LuaValue::String(self.fresh_str(error.into_bytes())),
                        LuaValue::String(self.intern_str(
                            if bytes_to_path(&path).is_some_and(|path| path.exists()) {
                                b"init".to_vec()
                            } else {
                                b"open".to_vec()
                            },
                        )),
                    ]),
                }
            }
            NativeFunction::Load => {
                let chunk_arg = required(0)?;
                let env = match args.get(3) {
                    Some(LuaValue::Nil) | None => None,
                    Some(value) => Some(value.clone()),
                };
                // Real Lua's `luaB_load` (`lbaselib.c`) first tries
                // `lua_tolstring` on argument #1, which succeeds not only for
                // an actual string but also for a number (auto-coerced) -
                // only once that fails does it fall back to treating
                // argument #1 as a reader function, via
                // `luaL_checktype(L, 1, LUA_TFUNCTION)`. A value that is
                // neither a string/number nor a function fails that
                // checktype *before* `lua_load`'s protected parser ever
                // starts, so - unlike every other failure path below - it is
                // a genuine raised Lua error, not one `load` catches and
                // reports as `nil, message` (matches the existing
                // `bad argument #1 to '<name>'` convention used elsewhere in
                // this dispatch, e.g. `coroutine.create`).
                let source: Vec<u8> = match &chunk_arg {
                    LuaValue::String(bytes) => bytes.as_bytes().to_vec(),
                    LuaValue::Integer(_) | LuaValue::Float(_) => chunk_arg.display_bytes(),
                    LuaValue::Closure(_)
                    | LuaValue::NativeFunction(_)
                    | LuaValue::Native(_)
                    | LuaValue::RegisteredNative(_) => {
                        // The generic reader protocol (`lbaselib.c`'s
                        // `generic_reader`, called in a loop by `lua_load`
                        // through `lzio.c`'s `luaZ_fill`): call the reader
                        // with no arguments, take its first return value,
                        // and keep going until a call returns nil or an
                        // empty string - `luaZ_fill` treats a NULL buffer
                        // (nil) and a zero-length one (`""`) identically as
                        // end-of-input - concatenating every piece in
                        // between into the full chunk source. Both the
                        // reader raising an error and it returning a
                        // non-string, non-nil value happen *inside*
                        // `lua_load`'s protected parser in real Lua (unlike
                        // the checktype above), so both are caught here too
                        // and reported through `load`'s normal `nil,
                        // message` return rather than propagated as a raised
                        // error.
                        let mut buffer = Vec::new();
                        loop {
                            let mut results = match self.call(chunk_arg.clone(), Vec::new()) {
                                Ok(results) => results,
                                Err(error) => {
                                    return Ok(vec![
                                        LuaValue::Nil,
                                        error.into_lua_value(&self.canonical_heap),
                                    ]);
                                }
                            };
                            let piece = if results.is_empty() {
                                LuaValue::Nil
                            } else {
                                results.remove(0)
                            };
                            match piece {
                                LuaValue::Nil => break,
                                LuaValue::String(bytes) => {
                                    if bytes.is_empty() {
                                        break;
                                    }
                                    buffer.extend_from_slice(bytes.as_bytes());
                                }
                                LuaValue::Integer(_) | LuaValue::Float(_) => {
                                    buffer.extend_from_slice(&piece.display_bytes());
                                }
                                _ => {
                                    return Ok(vec![
                                        LuaValue::Nil,
                                        LuaValue::String(self.intern_str(
                                            b"reader function must return a string",
                                        )),
                                    ]);
                                }
                            }
                        }
                        buffer
                    }
                    other => {
                        return Err(LuaError::new(format!(
                            "bad argument #1 to 'load' (function expected, got {})",
                            other.type_name()
                        )));
                    }
                };
                // `luaB_load` (`lbaselib.c`): the chunkname (arg #2) is
                // `luaL_optstring(L, 2, s)` when loading from a string/number
                // (defaults to the source itself), or
                // `luaL_optstring(L, 2, "=(load)")` when loading from a
                // reader function. This raw string is exactly what
                // `debug.getinfo(...).source` reports later - no `@`/`=`
                // prefix is added implicitly here.
                let chunkname: Vec<u8> = match args.get(1) {
                    Some(LuaValue::String(bytes)) => bytes.as_bytes().to_vec(),
                    Some(value @ (LuaValue::Integer(_) | LuaValue::Float(_))) => {
                        value.display_bytes()
                    }
                    _ => match &chunk_arg {
                        LuaValue::String(_) | LuaValue::Integer(_) | LuaValue::Float(_) => {
                            source.clone()
                        }
                        _ => b"=(load)".to_vec(),
                    },
                };
                // `lauxlib.c`'s `checkmode` (called from `lua_load`'s
                // reader-driven parse, `lstate.c`/`lundump.c`): the mode
                // string (arg #3, default `"bt"`) restricts whether a text
                // or a binary chunk (one starting with the Lua binary-chunk
                // signature byte `0x1B`, matching real Lua's
                // `LUA_SIGNATURE[0]` - see `string.dump`/`dumped_protos`) is
                // accepted, independent of `load`'s other checks.
                let mode: Vec<u8> = match args.get(2) {
                    Some(LuaValue::String(bytes)) => bytes.as_bytes().to_vec(),
                    Some(LuaValue::Nil) | None => b"bt".to_vec(),
                    Some(value) => value.display_bytes(),
                };
                let is_binary_chunk = source.first() == Some(&0x1B);
                let (modename, allowed): (&[u8], bool) = if is_binary_chunk {
                    (b"binary", mode.contains(&b'b'))
                } else {
                    (b"text", mode.contains(&b't'))
                };
                if !allowed {
                    let mut message = b"attempt to load a ".to_vec();
                    message.extend_from_slice(modename);
                    message.extend_from_slice(b" chunk (mode is '");
                    message.extend_from_slice(&mode);
                    message.extend_from_slice(b"')");
                    return Ok(vec![LuaValue::Nil, LuaValue::String(self.fresh_str(message))]);
                }
                let result = if is_binary_chunk {
                    self.load_binary_chunk(&source, env, Some(Rc::new(chunkname)))
                } else {
                    // `load()` never skips a leading `#` shebang line - that
                    // skip is `lauxlib.c`'s `luaL_loadfilex`-only behavior
                    // (`skipcomment`), not something `lua_load`/`llex.c`
                    // itself does, so a string like `load("#=1")` must be
                    // parsed as real code starting with `#`, not skipped.
                    self.compile_chunk_named(&source, env, Some(Rc::new(chunkname)), false)
                };
                match result {
                    Ok(closure) => Ok(vec![closure]),
                    Err(error) => Ok(vec![
                        LuaValue::Nil,
                        LuaValue::String(self.fresh_str(error.into_bytes())),
                    ]),
                }
            }
            NativeFunction::DoFile => {
                let name = self.string(&required(0)?)?.to_vec();
                // Real Lua's `dofile(filename)` (`lauxlib.c`'s
                // `luaL_dofile`/`luaL_loadfile`) always reads straight from
                // the given filesystem path - unlike `require`, it does no
                // `package.path` searching. Sol's in-memory
                // `module_sources` map (gated on the `package` capability,
                // for embedders that pre-register modules without a real
                // filesystem) is checked first so registered names keep
                // working under `Capabilities::SANDBOX`, but when the name
                // isn't registered there and the `filesystem` capability is
                // granted, fall back to a real read - this is what lets
                // `lua-5.5.1-tests/verybig.lua` write a generated program to
                // `os.tmpname()` and `dofile` it back.
                let source = if self.capabilities.package {
                    self.module_sources.get(&name).cloned()
                } else {
                    None
                };
                let source = match source {
                    Some(source) => source,
                    None if self.capabilities.filesystem => {
                        let real_path = bytes_to_path(&name)
                            .ok_or_else(|| LuaError::new("invalid file name"))?;
                        std::fs::read(&real_path).map_err(|error| {
                            LuaError::new(format!(
                                "cannot open '{}': {error}",
                                String::from_utf8_lossy(&name)
                            ))
                        })?
                    }
                    // No `filesystem` capability and no chance of ever
                    // having consulted `module_sources` either (`package`
                    // is off) - there is no way this call could ever
                    // succeed, so say so plainly, matching this crate's
                    // usual capability-denial wording (and preserved for
                    // `Capabilities::SANDBOX`, which has both off).
                    None if !self.capabilities.package => {
                        return Err(LuaError::new(
                            "package capability is disabled; register an in-memory module explicitly",
                        ));
                    }
                    None => {
                        return Err(LuaError::new(format!(
                            "dofile: module '{}' not found in the deterministic loader",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                };
                let closure = self.compile_chunk(&source, None).map_err(LuaError::new)?;
                self.call(closure, Vec::new())
            }
            _ => unreachable!("call_native_load received a non-load NativeFunction"),
        }
    }

    pub(super) fn compile_chunk(
        &mut self,
        source: &[u8],
        env: Option<LuaValue>,
    ) -> Result<LuaValue, String> {
        // File-oriented caller (`require`'s Lua searcher, `dofile`) - real
        // Lua's `luaL_loadfilex` (`lauxlib.c`'s `skipcomment`) always skips a
        // leading `#` shebang line for these.
        self.compile_chunk_named(source, env, None, true)
    }

    pub(super) fn compile_chunk_named(
        &mut self,
        source: &[u8],
        env: Option<LuaValue>,
        chunkname: Option<Rc<Vec<u8>>>,
        skip_shebang: bool,
    ) -> Result<LuaValue, String> {
        let format_error = |error: String| match &chunkname {
            Some(chunkname) => format_chunk_diagnostic(chunkname, source, &error),
            None => error,
        };
        let tokens = if skip_shebang {
            crate::lexer::lex_bytes(source).map_err(&format_error)?
        } else {
            crate::lexer::lex_bytes_no_shebang(source).map_err(&format_error)?
        };
        let program = crate::parser::parse_lua(tokens).map_err(&format_error)?;
        let chunk_globals = match env {
            Some(value) => Globals::from_value(self, value),
            None => self.globals.snapshot_for_load(self),
        };
        let mut main_function: Option<Function> = None;
        for function in &program.functions {
            if function.name == "main" {
                main_function = Some(function.clone());
                continue;
            }
            let proto = Compiler::compile_top_level(function).map_err(&format_error)?;
            if let Some(chunkname) = &chunkname {
                self.register_chunk_source(&proto, chunkname);
            }
            let closure = self
                .new_closure(proto, Vec::new(), chunk_globals.clone(), None)
                .map_err(|error| error.message)?;
            chunk_globals.define(self, &function.name, LuaValue::Closure(closure), false);
        }
        let body = main_function.unwrap_or_else(|| Function {
            name: "main".into(),
            is_chunk: true,
            source_file: None,
            params: Vec::new(),
            param_annotations: Vec::new(),
            vararg: false,
            vararg_name: None,
            return_type: None,
            body: Vec::new(),
            line: 1,
            end_line: 1,
            is_global_decl: false,
            source_span: crate::diagnostic::SourceSpan::new(0, 0, 1, 1),
        });
        let proto = Compiler::compile_top_level(&body).map_err(format_error)?;
        if let Some(chunkname) = &chunkname {
            self.register_chunk_source(&proto, chunkname);
        }
        let closure = self
            .new_closure(proto, Vec::new(), chunk_globals, None)
            .map_err(|error| error.message)?;
        Ok(LuaValue::Closure(closure))
    }

    fn load_binary_chunk(
        &mut self,
        source: &[u8],
        env: Option<LuaValue>,
        chunkname: Option<Rc<Vec<u8>>>,
    ) -> Result<LuaValue, String> {
        let header = lua55_binary_chunk_header();
        if source.len() < header.len() {
            return Err("truncated binary chunk".to_string());
        }
        if !source.starts_with(&header) {
            return Err("bad header in precompiled chunk".to_string());
        }
        let envelope_len = header
            .len()
            .checked_add(SOL_DUMP_MAGIC.len())
            .and_then(|len| len.checked_add(std::mem::size_of::<u64>() * 3))
            .expect("fixed dump envelope fits in usize");
        if source.len() < envelope_len {
            return Err("truncated binary chunk".to_string());
        }
        if &source[header.len()..header.len() + SOL_DUMP_MAGIC.len()] != SOL_DUMP_MAGIC {
            return Err("bad header in precompiled chunk".to_string());
        }
        let key_offset = header.len() + SOL_DUMP_MAGIC.len();
        let key = source
            .get(key_offset..key_offset + std::mem::size_of::<u64>())
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or_else(|| "bad header in precompiled chunk".to_string())?;
        let source_len_offset = key_offset + std::mem::size_of::<u64>();
        let source_len = source
            .get(source_len_offset..source_len_offset + std::mem::size_of::<u64>())
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| "bad binary chunk source size".to_string())?;
        let payload_len_offset = source_len_offset + std::mem::size_of::<u64>();
        let payload_len = source
            .get(payload_len_offset..payload_len_offset + std::mem::size_of::<u64>())
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or_else(|| "truncated binary chunk".to_string())?;
        let expected_len = envelope_len
            .checked_add(usize::try_from(payload_len).map_err(|_| "bad binary chunk size")?)
            .ok_or_else(|| "bad binary chunk size".to_string())?;
        if source.len() < expected_len {
            return Err("truncated binary chunk".to_string());
        }
        if source.len() != expected_len {
            return Err("bad binary chunk size".to_string());
        }
        if source_len > usize::try_from(payload_len).map_err(|_| "bad binary chunk size")? {
            return Err("bad binary chunk source size".to_string());
        }
        let proto = self
            .dumped_protos
            .get(&(key as usize))
            .cloned()
            .ok_or_else(|| "bad header in precompiled chunk".to_string())?;
        if source_len != 0 {
            let source_start = envelope_len;
            let source_end = source_start + source_len;
            self.register_chunk_source(&proto, &Rc::new(source[source_start..source_end].to_vec()));
        } else if let Some(chunkname) = &chunkname {
            self.register_chunk_source(&proto, chunkname);
        }
        let chunk_globals = match env {
            Some(value) => Globals::from_value(self, value),
            None => self.globals.snapshot_for_load(self),
        };
        // A dumped Lua function retains its upvalue *descriptors* but not
        // their captured values. `load` creates one nil cell per descriptor
        // (Lua callers can then discover/rebind the final implicit `_ENV`
        // slot through debug.getupvalue/setupvalue).
        let upvals = {
            let mut heap = self.canonical_heap.borrow_mut();
            (0..proto.upvals.len())
                .map(|_| heap.alloc_upvalue(sol_core::Value::NIL, None))
                .collect()
        };
        let closure = self
            .new_closure(proto, upvals, chunk_globals, None)
            .map_err(|error| error.message)?;
        Ok(LuaValue::Closure(closure))
    }

    pub(super) fn register_chunk_source(&mut self, proto: &Rc<Proto>, chunkname: &Rc<Vec<u8>>) {
        self.chunk_sources
            .insert(Rc::as_ptr(proto) as usize, chunkname.clone());
        for nested in &proto.nested {
            self.register_chunk_source(nested, chunkname);
        }
    }

    fn require(&mut self, name: LuaValue) -> LuaResult<Vec<LuaValue>> {
        let name = self.string(&name)?.to_vec();
        let key = LuaValue::String(self.intern_str(name.clone()));
        let loaded = self.table_get(self.package_loaded, &key)?;
        // Real Lua's `require` treats `package.loaded[name]` as "already
        // loaded" only when it is truthy (`lua_toboolean`), not merely
        // non-nil: a module whose loader legitimately cached `false` (e.g.
        // `lua-5.5.1-tests/attrib.lua`'s "default option" test, where the
        // loaded chunk returns `AA == false`) is treated as *not* loaded
        // and reloaded on the next `require` of the same name, exactly
        // like a module that was never loaded at all.
        if loaded.truthy() {
            return Ok(vec![loaded]);
        }
        if self.capabilities.package {
            if let Some(source) = self.module_sources.get(&name).cloned() {
                return self.require_module_source(name, key, source);
            }
        }
        self.require_search(name, key)
    }

    fn require_module_source(
        &mut self,
        name: Vec<u8>,
        key: LuaValue,
        source: Vec<u8>,
    ) -> LuaResult<Vec<LuaValue>> {
        // Mark before execution so a cyclic require observes a deterministic
        // partial-initialization sentinel instead of recursively reloading.
        self.table_set(self.package_loaded, key.clone(), LuaValue::Bool(true))?;
        self.loading_modules.insert(name.clone());
        let result = (|| {
            let program =
                crate::parser::parse_lua(crate::lexer::lex_bytes(&source).map_err(LuaError::new)?)
                    .map_err(LuaError::new)?;
            let base = self.globals.clone();
            let module = Globals::module(&base, self);
            module.define(
                self,
                "_NAME",
                LuaValue::String(self.intern_str(name.clone())),
                true,
            );
            self.run_in_globals(&program, &module, &HashSet::new(), &HashMap::new())
        })();
        self.loading_modules.remove(&name);

        match result {
            Ok(LuaValue::Nil) => Ok(vec![LuaValue::Bool(true)]),
            Ok(value) => {
                self.table_set(self.package_loaded, key, value.clone())?;
                Ok(vec![value])
            }
            Err(error) => {
                self.table_set(self.package_loaded, key, LuaValue::Nil)?;
                Err(error.at(&format!("module '{}'", String::from_utf8_lossy(&name))))
            }
        }
    }

    pub(super) fn prepare_debug_module(&mut self, name: &[u8], source: &[u8]) -> LuaResult<LuaValue> {
        let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).map_err(LuaError::new)?)
            .map_err(LuaError::new)?;
        let module = Globals::module(&self.globals, self);
        module.define(self, "_NAME", LuaValue::String(self.intern_str(name)), true);
        let root = self.encode_value(&module.as_value())?;
        self.pinned_roots.push(vec![root]);
        let source_name = self.debug_module_names.get(name).cloned()
            .unwrap_or_else(|| format!("@{}.lua", String::from_utf8_lossy(name).replace('.', "/")).into_bytes());
        let previous_name = self.default_chunk_name.replace(Rc::new(source_name));
        let result = self.load_in_globals(&program, &module, &HashSet::new(), &HashMap::new(), None);
        self.default_chunk_name = previous_name;
        self.pinned_roots.pop();
        result?;
        Ok(module.get(self, "main"))
    }

    pub(super) fn discard_native_cont(&mut self, cont: super::frame::NativeCont) {
        if let super::frame::NativeCont::Require { name, key } = cont {
            self.loading_modules.remove(&name);
            let _ = self.table_set(self.package_loaded, key, LuaValue::Nil);
        }
    }

    fn require_search(&mut self, name: Vec<u8>, key: LuaValue) -> LuaResult<Vec<LuaValue>> {
        let searchers_key = LuaValue::String(self.intern_str(b"searchers"));
        let searchers = self.table_get(self.package_table, &searchers_key)?;
        let LuaValue::Table(searchers) = searchers else {
            return Err(LuaError::new("'package.searchers' must be a table"));
        };
        let mut errors = Vec::new();
        let mut index = 1_i64;
        loop {
            let searcher = self.table_get(searchers, &LuaValue::Integer(index))?;
            if searcher == LuaValue::Nil {
                break;
            }
            let results = self.call(searcher, vec![LuaValue::String(self.intern_str(name.clone()))])?;
            match results.first() {
                Some(loader) if loader.is_callable() => {
                    let extra = results.get(1).cloned().unwrap_or(LuaValue::Nil);
                    return self.require_call_loader(name, key, loader.clone(), extra);
                }
                Some(LuaValue::String(message)) => errors.extend_from_slice(message.as_bytes()),
                _ => {}
            }
            index += 1;
        }

        Err(LuaError::new(format!(
            "module '{}' not found:{}",
            String::from_utf8_lossy(&name),
            String::from_utf8_lossy(&errors)
        )))
    }

    fn search_native_loader(
        &mut self,
        path_name: &[u8],
        symbol_name: &[u8],
    ) -> LuaResult<Vec<LuaValue>> {
        let filename = match self.require_search_field(path_name, b"cpath")? {
            Ok(filename) => filename,
            Err(message) => return Ok(vec![LuaValue::String(self.fresh_str(message))]),
        };
        if !self.capabilities.native_modules {
            return Err(LuaError::new("native module capability is disabled"));
        }
        let mut symbol = b"luaopen_".to_vec();
        let symbol_name = symbol_name
            .split(|byte| *byte == b'-')
            .next()
            .unwrap_or(symbol_name);
        symbol.extend(
            symbol_name
                .iter()
                .map(|byte| if *byte == b'.' { b'_' } else { *byte }),
        );
        let (library, function) =
            c_api::load_native_callable(&filename, &symbol).map_err(|error| {
                LuaError::new(format!(
                    "error loading module '{}' from file '{}':\n\t{}",
                    String::from_utf8_lossy(symbol_name),
                    String::from_utf8_lossy(&filename),
                    error
                ))
            })?;
        let callable = self.register_c_function(function)?;
        self.native_libraries.push(library);
        Ok(vec![
            LuaValue::CFunction(CanonicalCFunction::allocate(
                self.canonical_heap.clone(),
                callable,
                Vec::new(),
            )),
            LuaValue::String(self.fresh_str(filename)),
        ])
    }

    fn require_search_field(
        &mut self,
        name: &[u8],
        field: &[u8],
    ) -> LuaResult<Result<Vec<u8>, Vec<u8>>> {
        let field_key = LuaValue::String(self.intern_str(field));
        let value = self.table_get(self.package_table, &field_key)?;
        let path = match value {
            LuaValue::String(bytes) => bytes,
            _ => {
                return Err(LuaError::new(format!(
                    "'package.{}' must be a string",
                    String::from_utf8_lossy(field)
                )));
            }
        };
        let dirsep = default_dirsep();
        Ok(self.search_path_candidates(name, path.as_bytes(), b".", &dirsep))
    }

    fn require_call_loader(
        &mut self,
        name: Vec<u8>,
        key: LuaValue,
        loader: LuaValue,
        extra: LuaValue,
    ) -> LuaResult<Vec<LuaValue>> {
        let mut values = self.call(loader, vec![LuaValue::String(self.intern_str(name)), extra.clone()])?;
        let returned = values.drain(..).next().unwrap_or(LuaValue::Nil);
        if returned != LuaValue::Nil {
            self.table_set(self.package_loaded, key.clone(), returned)?;
        }
        let current = self.table_get(self.package_loaded, &key)?;
        let result = if current == LuaValue::Nil {
            self.table_set(self.package_loaded, key, LuaValue::Bool(true))?;
            LuaValue::Bool(true)
        } else {
            current
        };
        Ok(vec![result, extra])
    }

    fn search_path_candidates(
        &self,
        name: &[u8],
        path: &[u8],
        sep: &[u8],
        dirsep: &[u8],
    ) -> Result<Vec<u8>, Vec<u8>> {
        let adjusted_name = bytes_replace_all(name, sep, dirsep);
        let mut message = Vec::new();
        for template in path.split(|byte| *byte == b';').filter(|t| !t.is_empty()) {
            let candidate = bytes_replace_all(template, b"?", &adjusted_name);
            let readable = self.capabilities.filesystem
                && bytes_to_path(&candidate).is_some_and(|path| std::fs::File::open(path).is_ok());
            if readable {
                return Ok(candidate);
            }
            message.extend_from_slice(b"\n\tno file '");
            message.extend_from_slice(&candidate);
            message.push(b'\'');
        }
        Err(message)
    }
}

fn format_chunk_diagnostic(chunkname: &[u8], source: &[u8], diagnostic: &str) -> String {
    let Some(rest) = diagnostic.strip_prefix("line ") else {
        return diagnostic.to_string();
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return diagnostic.to_string();
    }
    let line = &rest[..digits];
    let Some(message_start) = rest.find(": ") else {
        return diagnostic.to_string();
    };
    let parser_message = &rest[message_start + 2..];
    let line = if is_eof_table_diagnostic(parser_message) {
        (source.iter().filter(|byte| **byte == b'\n').count() + 1).to_string()
    } else {
        line.to_string()
    };
    let message = lua_syntax_message(source, parser_message);
    format!("{}:{line}: {message}", display_chunk_name(chunkname))
}

fn is_eof_table_diagnostic(message: &str) -> bool {
    // `expect(&Token::RBrace)`'s own end-of-input arm now appends the same
    // `near <eof>` suffix every other Lua-mode parser error carries (see
    // `Parser::lua_near_suffix`), so it must be stripped here too before
    // matching against the exact wording this special case rewrites -
    // `self.error(...)`'s own "unexpected end of input inside table
    // constructor" site never carries that suffix, so stripping it is a
    // no-op for that variant.
    let message = message.strip_suffix(" near <eof>").unwrap_or(message);
    matches!(
        message.strip_suffix(" [EPARSE001]"),
        Some("expected RBrace, found end of input" | "unexpected end of input inside table constructor")
    )
}

/// The typed parser deliberately retains richer editor diagnostics than Lua,
/// but `load` exposes Lua's source-compatibility diagnostics. Translate the
/// EOF table-constructor form here because only the chunk boundary still has
/// the complete source needed to identify the EOF line and the opening brace.
fn lua_syntax_message<'a>(source: &[u8], message: &'a str) -> std::borrow::Cow<'a, str> {
    // See `is_eof_table_diagnostic`'s comment - `expect(&Token::RBrace)`'s
    // end-of-input arm now appends its own `near <eof>` suffix, which must
    // be peeled off (along with the diagnostic code) before matching the
    // exact wording this rewrites into Lua's own "'}' expected ... near
    // <eof>" phrasing. Stripped only into a local `stripped` copy used for
    // the match, not into `message` itself - every other Lua-mode parser
    // error also legitimately ends in a `near '...'`/`near <eof>` suffix of
    // its own (see `Parser::lua_near_suffix`), which must survive unchanged
    // on the early `Cow::Borrowed(message)` return below for every message
    // that isn't one of these two exact, pre-suffix-stripping strings.
    const NEAR_EOF_SUFFIX: &str = " near <eof>";
    const PARSER_SUFFIX: &str = " [EPARSE001]";
    let stripped = message.strip_suffix(NEAR_EOF_SUFFIX).unwrap_or(message);
    let stripped = stripped.strip_suffix(PARSER_SUFFIX).unwrap_or(stripped);
    if !matches!(
        stripped,
        "expected RBrace, found end of input" | "unexpected end of input inside table constructor"
    ) {
        return std::borrow::Cow::Borrowed(message);
    }
    let opening_line = source
        .iter()
        .rposition(|byte| *byte == b'{')
        .map(|offset| {
            source[..offset]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count()
                + 1
        })
        .unwrap_or(1);
    std::borrow::Cow::Owned(format!(
        "'}}' expected (to close '{{' at line {opening_line}) near <eof>"
    ))
}

fn display_chunk_name(chunkname: &[u8]) -> String {
    String::from_utf8_lossy(&LuaRuntime::short_src(chunkname)).into_owned()
}
