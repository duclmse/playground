//! `LuaRuntime` construction/configuration and top-level entry points:
//! `new`/`with_limits`/`with_budgets`/`with_capabilities*`, standard-library
//! bootstrap (`install_base`/`preload`), and the `run*`/`take_output` methods
//! that drive a whole program to completion.

use std::collections::{HashMap, HashSet};

use crate::ast::Program;
use crate::lua_bytecode::Compiler;

use super::util::{package_config_bytes, seeded_random_state};
use super::*;

impl LuaRuntime {
    pub fn new() -> Self {
        Self::with_budgets(1_000_000, 1_000, 64 * 1024 * 1024)
    }

    pub fn with_limits(instruction_budget: u64, max_call_depth: usize) -> Self {
        Self::with_budgets(instruction_budget, max_call_depth, 64 * 1024 * 1024)
    }

    pub fn with_budgets(
        instruction_budget: u64,
        max_call_depth: usize,
        allocation_budget: usize,
    ) -> Self {
        let mut heap = sol_core::Heap::new(Capabilities::default());
        let c_registry = heap.alloc_table();
        let c_registry_root = heap.add_root(sol_core::Value::object(c_registry));
        let canonical_heap = Rc::new(RefCell::new(heap));
        let mut runtime = Self {
            canonical_heap: canonical_heap.clone(),
            c_registry,
            _c_registry_root: c_registry_root,
            c_next_reference: 1,
            globals: Globals::root(canonical_heap),
            output: Vec::new(),
            instructions_remaining: instruction_budget,
            call_depth: 0,
            max_call_depth,
            allocation_remaining: allocation_budget,
            allocation_budget,
            capabilities: Capabilities::default(),
            module_sources: HashMap::new(),
            loading_modules: HashSet::new(),
            package_loaded: Rc::new(RefCell::new(LuaTable::default())),
            package_table: Rc::new(RefCell::new(LuaTable::default())),
            start_time: std::time::Instant::now(),
            weak_tables: Vec::new(),
            gc_tables: RefCell::new(Vec::new()),
            gc_closures: RefCell::new(Vec::new()),
            charge_ledger: RefCell::new(HashMap::new()),
            coroutine_stack: Vec::new(),
            main_coroutine: LuaCoroutine::main_thread(),
            gc_stress: false,
            instructions_since_gc: 0,
            regs_pool: Vec::new(),
            cells_pool: Vec::new(),
            values_pool: Vec::new(),
            random_state: seeded_random_state(0x534f_4c55_4152_554e, 0),
            frames: Vec::new(),
            pending_frame_label: None,
            pending_error_stack: None,
            prototype_ids: HashMap::new(),
            chunk_sources: HashMap::new(),
            default_chunk_name: None,
            dumped_protos: HashMap::new(),
            function_registry: sol_core::FunctionRegistry::default(),
            native_bridges: HashMap::new(),
            c_functions: HashMap::new(),
            c_function_ids: HashMap::new(),
            c_light_userdata: RefCell::new(HashMap::new()),
            c_light_userdata_reverse: RefCell::new(HashMap::new()),
            file_userdata: HashSet::new(),
            c_warning_function: None,
            c_warning_data: std::ptr::null_mut(),
            c_thread_states: RefCell::new(HashMap::new()),
            c_continuations: RefCell::new(HashMap::new()),
            c_upvalue_identities: RefCell::new(HashMap::new()),
            native_libraries: Vec::new(),
            native_bridge_ids: HashMap::new(),
            next_native_function: 0,
            string_metatable: Rc::new(RefCell::new(LuaTable::default())),
            number_metatable: None,
            boolean_metatable: None,
            nil_metatable: None,
            gc_mode: "incremental",
            gc_running: true,
            gc_pause: 100,
            gc_stepmul: 100,
            current_locale: "C".to_string(),
            tmpname_counter: 0,
            io_stdout: Rc::new(RefCell::new(LuaTable::default())),
            io_stderr: Rc::new(RefCell::new(LuaTable::default())),
            default_output: RefCell::new(LuaValue::Nil),
            open_files: RefCell::new(HashMap::new()),
            active_hook: None,
            running_hook: false,
        };
        *runtime.default_output.borrow_mut() = LuaValue::Table(runtime.io_stdout.clone());
        runtime
            .globals
            .define("_G", runtime.globals.as_value(), false);
        runtime.install_base();
        runtime
    }

    /// Like `new`, but with an explicit host capability profile instead of
    /// the sandboxed-by-default one. Embedders that want `os`/`io`/`debug`/
    /// native-module surfaces enabled (e.g. the `sol` CLI, which is a
    /// trusted native tool, not the browser sandbox) opt in here rather than
    /// having those capabilities on by default for every embedder.
    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        let mut runtime = Self::new();
        runtime.capabilities = capabilities;
        runtime.canonical_heap.borrow_mut().capabilities = capabilities;
        runtime
    }

    /// Like `with_capabilities`, but also lets the caller override the
    /// sandboxed-by-default resource budgets (`LuaRuntime::new`'s
    /// 1,000,000-instruction / 1,000-call-depth / 64MiB defaults are sized
    /// for untrusted embedded code, not a trusted native CLI running real
    /// workloads).
    pub fn with_capabilities_and_budgets(
        capabilities: Capabilities,
        instruction_budget: u64,
        max_call_depth: usize,
        allocation_budget: usize,
    ) -> Self {
        let mut runtime = Self::with_budgets(instruction_budget, max_call_depth, allocation_budget);
        runtime.capabilities = capabilities;
        runtime.canonical_heap.borrow_mut().capabilities = capabilities;
        runtime
    }

    /// Allocates full userdata in the canonical heap and returns a precisely
    /// rooted Lua handle. This is the production path used by the C API.
    pub fn new_userdata(&self, size: usize) -> LuaValue {
        self.new_userdata_with_uservalues(size, 0)
    }

    pub fn new_userdata_with_uservalues(&self, size: usize, user_values: usize) -> LuaValue {
        LuaValue::Userdata(CanonicalUserdata::allocate_with_uservalues(
            self.canonical_heap.clone(),
            size,
            user_values,
        ))
    }

    /// Enable the deterministic package capability and register one module.
    /// Module names are exact byte strings; no filesystem search or native
    /// loader is consulted.
    pub fn add_module(&mut self, name: impl AsRef<[u8]>, source: impl AsRef<[u8]>) {
        self.capabilities.package = true;
        self.module_sources
            .insert(name.as_ref().to_vec(), source.as_ref().to_vec());
    }

    pub fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    /// `require`'s deterministic module loader only knows about explicitly
    /// registered sources (`add_module`), so without this a standard-library
    /// `require("os")`/`require("debug")`/etc. - completely ordinary in real
    /// Lua, where the standard libraries are always preloaded in
    /// `package.loaded` regardless of any sandboxing - would incorrectly
    /// need the `package` capability just to see a table that's already a
    /// global. Seeding `package_loaded` here lets `require`'s existing
    /// already-loaded check (which runs before the capability gate) resolve
    /// these without touching the loader at all.
    fn preload(&self, name: &[u8], value: LuaValue) {
        self.package_loaded
            .borrow_mut()
            .set(LuaValue::String(self.intern_str(name.to_vec())), value)
            .unwrap();
    }

    fn install_base(&mut self) {
        self.globals.define(
            "print",
            LuaValue::NativeFunction(NativeFunction::Print),
            true,
        );
        self.globals.define(
            "assert",
            LuaValue::NativeFunction(NativeFunction::Assert),
            true,
        );
        self.globals
            .define("type", LuaValue::NativeFunction(NativeFunction::Type), true);
        self.globals.define(
            "tostring",
            LuaValue::NativeFunction(NativeFunction::ToString),
            true,
        );
        self.globals.define(
            "tonumber",
            LuaValue::NativeFunction(NativeFunction::ToNumber),
            true,
        );
        self.globals.define(
            "rawget",
            LuaValue::NativeFunction(NativeFunction::RawGet),
            true,
        );
        self.globals.define(
            "rawset",
            LuaValue::NativeFunction(NativeFunction::RawSet),
            true,
        );
        self.globals.define(
            "rawequal",
            LuaValue::NativeFunction(NativeFunction::RawEqual),
            true,
        );
        self.globals.define(
            "rawlen",
            LuaValue::NativeFunction(NativeFunction::RawLen),
            true,
        );
        self.globals.define(
            "getmetatable",
            LuaValue::NativeFunction(NativeFunction::GetMetatable),
            true,
        );
        self.globals.define(
            "setmetatable",
            LuaValue::NativeFunction(NativeFunction::SetMetatable),
            true,
        );
        self.globals.define(
            "error",
            LuaValue::NativeFunction(NativeFunction::Error),
            true,
        );
        self.globals.define(
            "pcall",
            LuaValue::NativeFunction(NativeFunction::PCall),
            true,
        );
        self.globals.define(
            "xpcall",
            LuaValue::NativeFunction(NativeFunction::XCall),
            true,
        );
        self.globals.define(
            "select",
            LuaValue::NativeFunction(NativeFunction::Select),
            true,
        );
        self.globals
            .define("next", LuaValue::NativeFunction(NativeFunction::Next), true);
        self.globals.define(
            "pairs",
            LuaValue::NativeFunction(NativeFunction::Pairs),
            true,
        );
        self.globals.define(
            "ipairs",
            LuaValue::NativeFunction(NativeFunction::IPairs),
            true,
        );
        self.globals.define(
            "require",
            LuaValue::NativeFunction(NativeFunction::Require),
            true,
        );
        self.globals.define(
            "collectgarbage",
            LuaValue::NativeFunction(NativeFunction::CollectGarbage),
            true,
        );
        self.globals
            .define("load", LuaValue::NativeFunction(NativeFunction::Load), true);
        self.globals.define(
            "loadstring",
            LuaValue::NativeFunction(NativeFunction::Load),
            true,
        );
        self.globals.define(
            "dofile",
            LuaValue::NativeFunction(NativeFunction::DoFile),
            true,
        );
        let string = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("len", NativeFunction::StringLen),
            ("byte", NativeFunction::StringByte),
            ("char", NativeFunction::StringChar),
            ("sub", NativeFunction::StringSub),
            ("lower", NativeFunction::StringLower),
            ("upper", NativeFunction::StringUpper),
            ("reverse", NativeFunction::StringReverse),
            ("rep", NativeFunction::StringRep),
            ("dump", NativeFunction::StringDump),
            ("find", NativeFunction::StringFind),
            ("match", NativeFunction::StringMatch),
            ("gmatch", NativeFunction::StringGMatch),
            ("gsub", NativeFunction::StringGSub),
            ("format", NativeFunction::StringFormat),
            ("pack", NativeFunction::StringPack),
            ("unpack", NativeFunction::StringUnpack),
            ("packsize", NativeFunction::StringPackSize),
        ] {
            string
                .borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.globals
            .define("string", LuaValue::Table(string.clone()), true);
        self.preload(b"string", LuaValue::Table(string.clone()));
        self.string_metatable
            .borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"__index".to_vec())),
                LuaValue::Table(string),
            )
            .unwrap();

        let table = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("concat", NativeFunction::TableConcat),
            ("insert", NativeFunction::TableInsert),
            ("remove", NativeFunction::TableRemove),
            ("pack", NativeFunction::TablePack),
            ("unpack", NativeFunction::TableUnpack),
            ("sort", NativeFunction::TableSort),
            ("create", NativeFunction::TableCreate),
            ("move", NativeFunction::TableMove),
        ] {
            table
                .borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.globals
            .define("table", LuaValue::Table(table.clone()), true);
        self.preload(b"table", LuaValue::Table(table));

        let math = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("abs", NativeFunction::MathAbs),
            ("floor", NativeFunction::MathFloor),
            ("ceil", NativeFunction::MathCeil),
            ("min", NativeFunction::MathMin),
            ("max", NativeFunction::MathMax),
            ("tointeger", NativeFunction::MathToInteger),
            ("type", NativeFunction::MathType),
            ("sqrt", NativeFunction::MathSqrt),
            ("sin", NativeFunction::MathSin),
            ("cos", NativeFunction::MathCos),
            ("tan", NativeFunction::MathTan),
            ("exp", NativeFunction::MathExp),
            ("log", NativeFunction::MathLog),
            ("acos", NativeFunction::MathAcos),
            ("asin", NativeFunction::MathAsin),
            ("atan", NativeFunction::MathAtan),
            ("deg", NativeFunction::MathDeg),
            ("rad", NativeFunction::MathRad),
            ("fmod", NativeFunction::MathFmod),
            ("modf", NativeFunction::MathModf),
            ("ult", NativeFunction::MathUlt),
            ("frexp", NativeFunction::MathFrexp),
            ("ldexp", NativeFunction::MathLdexp),
            ("random", NativeFunction::MathRandom),
            ("randomseed", NativeFunction::MathRandomSeed),
        ] {
            math.borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        for (name, value) in [
            ("pi", LuaValue::Float(std::f64::consts::PI)),
            ("huge", LuaValue::Float(f64::INFINITY)),
            ("maxinteger", LuaValue::Integer(i64::MAX)),
            ("mininteger", LuaValue::Integer(i64::MIN)),
        ] {
            math.borrow_mut()
                .set(LuaValue::String(self.intern_str(name.as_bytes().to_vec())), value)
                .unwrap();
        }
        self.globals
            .define("math", LuaValue::Table(math.clone()), true);
        self.preload(b"math", LuaValue::Table(math));

        let utf8 = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("len", NativeFunction::Utf8Len),
            ("char", NativeFunction::Utf8Char),
            ("codepoint", NativeFunction::Utf8Codepoint),
            ("offset", NativeFunction::Utf8Offset),
            ("codes", NativeFunction::Utf8Codes),
        ] {
            utf8.borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        // Lua 5.5's lutf8lib.c publishes this exact byte pattern. Keep it as
        // bytes: the leading NUL and the non-UTF-8 range endpoints are valid
        // Lua string data even though they are not valid Rust UTF-8 text.
        utf8.borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"charpattern".to_vec())),
                LuaValue::String(self.intern_str(b"[\0-\x7f\xc2-\xfd][\x80-\xbf]*".to_vec())),
            )
            .unwrap();
        self.globals
            .define("utf8", LuaValue::Table(utf8.clone()), true);
        self.preload(b"utf8", LuaValue::Table(utf8));

        let package = self.package_table.clone();
        {
            let mut package_mut = package.borrow_mut();
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"loaded".to_vec())),
                    LuaValue::Table(self.package_loaded.clone()),
                )
                .unwrap();
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"preload".to_vec())),
                    LuaValue::Table(Rc::new(RefCell::new(LuaTable::default()))),
                )
                .unwrap();
            // Sol installs nothing on the host (no `sol`-managed share/lib
            // prefix, no system package manager) - `./?.lua`/`./?/init.lua`
            // are the only honest default templates: "next to the running
            // script", matching real Lua's own trailing fallback templates
            // after its compiled-in install-prefix ones.
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"path".to_vec())),
                    LuaValue::String(self.intern_str(b"./?.lua;./?/init.lua".to_vec())),
                )
                .unwrap();
            // Kept empty by default so sandboxed states never advertise a
            // host loader. Native embedders/CLI hosts may set this after
            // opting into `Capabilities::native_modules`.
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"cpath".to_vec())),
                    LuaValue::String(self.intern_str(Vec::new())),
                )
                .unwrap();
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"config".to_vec())),
                    LuaValue::String(self.intern_str(package_config_bytes())),
                )
                .unwrap();
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"searchpath".to_vec())),
                    LuaValue::NativeFunction(NativeFunction::PackageSearchPath),
                )
                .unwrap();
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"loadlib".to_vec())),
                    LuaValue::NativeFunction(NativeFunction::PackageLoadLib),
                )
                .unwrap();
            // Keep these as ordinary table entries: `require` deliberately
            // reads and calls the current contents so embedders can replace,
            // remove, or reorder searchers exactly as in Lua.
            let mut searchers = LuaTable::default();
            for (index, searcher) in [
                NativeFunction::PackageSearcherPreload,
                NativeFunction::PackageSearcherLua,
                NativeFunction::PackageSearcherC,
                NativeFunction::PackageSearcherCRoot,
            ]
            .into_iter()
            .enumerate()
            {
                searchers
                    .set(
                        LuaValue::Integer(index as i64 + 1),
                        LuaValue::NativeFunction(searcher),
                    )
                    .unwrap();
            }
            package_mut
                .set(
                    LuaValue::String(self.intern_str(b"searchers".to_vec())),
                    LuaValue::Table(Rc::new(RefCell::new(searchers))),
                )
                .unwrap();
        }
        // Unlike most other standard-library globals, `package` is left
        // reassignable (not constant): real Lua's own globals - `package`
        // included - are ordinary global variables, and
        // `lua-5.5.1-tests/attrib.lua`'s "testing preload" section
        // temporarily reassigns the `package` global itself
        // (`package = {}`, later restored) around a `require` call, which
        // must succeed rather than raise "attempt to assign to const
        // variable 'package'". `require`/`package.searchpath` still close
        // over the original table object via `self.package_table`
        // regardless of what the global currently points to (see
        // `package_table`'s doc comment), so this reassignment is otherwise
        // inert to this runtime's own behavior.
        self.globals
            .define("package", LuaValue::Table(package.clone()), false);
        self.preload(b"package", LuaValue::Table(package));
        self.preload(b"_G", self.globals.as_value());

        let os = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("time", NativeFunction::OsTime),
            ("clock", NativeFunction::OsClock),
            ("difftime", NativeFunction::OsDifftime),
            ("date", NativeFunction::OsDate),
            ("getenv", NativeFunction::OsGetenv),
            ("exit", NativeFunction::OsExit),
            ("remove", NativeFunction::OsRemove),
            ("setlocale", NativeFunction::OsSetlocale),
            ("tmpname", NativeFunction::OsTmpname),
        ] {
            os.borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.globals.define("os", LuaValue::Table(os.clone()), true);
        self.preload(b"os", LuaValue::Table(os));

        let io = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("write", NativeFunction::IoWrite),
            ("read", NativeFunction::IoRead),
            ("input", NativeFunction::IoInput),
            ("output", NativeFunction::IoOutput),
            ("close", NativeFunction::FileClose),
        ] {
            io.borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.io_stdout
            .borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"write".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileWrite),
            )
            .unwrap();
        self.io_stdout
            .borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"close".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileClose),
            )
            .unwrap();
        io.borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"stdout".to_vec())),
                LuaValue::Table(self.io_stdout.clone()),
            )
            .unwrap();
        self.io_stderr
            .borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"write".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileWrite),
            )
            .unwrap();
        self.io_stderr
            .borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"close".to_vec())),
                LuaValue::NativeFunction(NativeFunction::FileClose),
            )
            .unwrap();
        io.borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"stderr".to_vec())),
                LuaValue::Table(self.io_stderr.clone()),
            )
            .unwrap();
        // Stable default-input handle. File handles are full userdata, not
        // tables, so `rawlen` and numeric-for diagnostics retain Lua's type
        // distinction without exposing host storage to ordinary indexing.
        let stdin = self.new_userdata(0);
        if let LuaValue::Userdata(userdata) = &stdin {
            self.file_userdata.insert(userdata.object_id());
            let metatable = CanonicalTable::allocate(self.canonical_heap.clone());
            let mut heap = self.canonical_heap.borrow_mut();
            let name_key = heap.alloc_string(b"__name");
            let name_value = heap.alloc_string(b"FILE*");
            heap.table_set(
                metatable.object_id(),
                sol_core::Value::object(name_key),
                sol_core::Value::object(name_value),
            )
            .expect("fresh canonical file metatable accepts __name");
            // Matches real Lua's `liolib.c` `metameth` table, which installs
            // the *same* `f_gc` C function under both `__gc` and `__close`
            // (`__close`'s to-be-closed-variable path and an explicit
            // `getmetatable(io.stdin).__gc()` call are meant to reach
            // identical argument-checking behavior - see `FileGc`'s doc
            // comment in `natives_os_io.rs`). Real Lua's `lua_pushcclosure`
            // pushes the same function pointer both times, so `mt.__gc ==
            // mt.__close`; Sol's `CFunction`/`NativeCallable` equality is
            // object-identity-based (`value.rs`), so this must reuse one
            // allocated callable object for both keys rather than allocating
            // twice.
            let gc_key = heap.alloc_string(b"__gc");
            let close_key = heap.alloc_string(b"__close");
            let gc_native = heap.alloc_native_callable(0, NativeFunction::FileGc as u32, Vec::new());
            heap.table_set(
                metatable.object_id(),
                sol_core::Value::object(gc_key),
                sol_core::Value::object(gc_native),
            )
            .expect("fresh canonical file metatable accepts __gc");
            heap.table_set(
                metatable.object_id(),
                sol_core::Value::object(close_key),
                sol_core::Value::object(gc_native),
            )
            .expect("fresh canonical file metatable accepts __close");
            heap.set_metatable(userdata.object_id(), Some(metatable.object_id()))
                .expect("fresh canonical file userdata accepts a metatable");
        }
        io.borrow_mut()
            .set(
                LuaValue::String(self.intern_str(b"stdin".to_vec())),
                stdin,
            )
            .unwrap();
        let coroutine = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("create", NativeFunction::CoroutineCreate),
            ("resume", NativeFunction::CoroutineResume),
            ("yield", NativeFunction::CoroutineYield),
            ("status", NativeFunction::CoroutineStatus),
            ("wrap", NativeFunction::CoroutineWrap),
            ("running", NativeFunction::CoroutineRunning),
            ("isyieldable", NativeFunction::CoroutineIsYieldable),
            ("close", NativeFunction::CoroutineClose),
        ] {
            coroutine
                .borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.globals
            .define("coroutine", LuaValue::Table(coroutine.clone()), true);
        self.preload(b"coroutine", LuaValue::Table(coroutine));

        self.globals.define("io", LuaValue::Table(io.clone()), true);
        self.preload(b"io", LuaValue::Table(io));

        let debug = Rc::new(RefCell::new(LuaTable::default()));
        for (name, function) in [
            ("getupvalue", NativeFunction::DebugGetupvalue),
            ("upvalueid", NativeFunction::DebugUpvalueid),
            ("upvaluejoin", NativeFunction::DebugUpvaluejoin),
            ("setupvalue", NativeFunction::DebugSetupvalue),
            ("getinfo", NativeFunction::DebugGetinfo),
            ("getmetatable", NativeFunction::DebugGetmetatable),
            ("setmetatable", NativeFunction::DebugSetmetatable),
            ("traceback", NativeFunction::DebugTraceback),
            ("sethook", NativeFunction::DebugSethook),
            ("gethook", NativeFunction::DebugGethook),
            ("setuservalue", NativeFunction::DebugSetuservalue),
        ] {
            debug
                .borrow_mut()
                .set(
                    LuaValue::String(self.intern_str(name.as_bytes().to_vec())),
                    LuaValue::NativeFunction(function),
                )
                .unwrap();
        }
        self.globals
            .define("debug", LuaValue::Table(debug.clone()), true);
        self.preload(b"debug", LuaValue::Table(debug));
    }

    /// Registers `name` (real Lua's `@`-prefixed filename convention, e.g.
    /// `@path/to/file.lua`) as the chunk name every top-level function this
    /// runtime subsequently compiles (`load_in_globals`) reports through
    /// `debug.getinfo`'s `source`/`short_src` fields. Embedders that never
    /// call this get today's existing behavior: no `chunk_sources` entry for
    /// the program's own top-level compile, so those fields are simply
    /// omitted.
    pub fn set_chunk_name(&mut self, name: Vec<u8>) {
        self.default_chunk_name = Some(Rc::new(name));
    }

    pub fn run(&mut self, program: &Program) -> LuaResult<LuaValue> {
        let globals = self.globals.clone();
        self.run_in_globals(program, &globals, &HashSet::new(), &HashMap::new())
    }

    pub fn run_with_natives(
        &mut self,
        program: &Program,
        native_names: &HashSet<String>,
        natives: HashMap<String, LuaValue>,
    ) -> LuaResult<LuaValue> {
        let globals = self.globals.clone();
        self.run_in_globals(program, &globals, native_names, &natives)
    }

    pub fn run_with_natives_and_plan(
        &mut self,
        program: &Program,
        native_names: &HashSet<String>,
        natives: HashMap<String, LuaValue>,
        optimization_plan: &crate::typeck::inference::OptimizationPlan,
    ) -> LuaResult<LuaValue> {
        let globals = self.globals.clone();
        self.load_in_globals(
            program,
            &globals,
            native_names,
            &natives,
            Some(optimization_plan),
        )?;
        let main = globals.get("main");
        let values = self.call(main, Vec::new())?;
        Ok(values.into_iter().next().unwrap_or(LuaValue::Nil))
    }

    /// Loads generic prototypes and registered native bindings without
    /// invoking `main`. Mixed-tier runners use this to let specialized
    /// bytecode enter dynamic functions through semantic adapter slots.
    pub fn load_with_natives(
        &mut self,
        program: &Program,
        native_names: &HashSet<String>,
        natives: HashMap<String, LuaValue>,
    ) -> LuaResult<()> {
        let globals = self.globals.clone();
        self.load_in_globals(program, &globals, native_names, &natives, None)
    }

    /// Publish a statically loaded module namespace through Lua's ordinary
    /// `package.loaded` cache. The table reuses the exact closure/native
    /// values installed in the root globals, so `import` and `require` do not
    /// instantiate separate module objects or callable identities.
    pub fn preload_namespace_module(&mut self, name: &str, exports: &[String]) -> LuaResult<()> {
        self.charge_allocation(std::mem::size_of::<LuaTable>())?;
        let table = self.track_table(Rc::new(RefCell::new(LuaTable::default())));
        for export in exports {
            let value = self.globals.get(&format!("{name}.{export}"));
            if value == LuaValue::Nil {
                continue;
            }
            table
                .borrow_mut()
                .set(LuaValue::String(self.intern_str(export.as_bytes().to_vec())), value)?;
        }
        self.preload(name.as_bytes(), LuaValue::Table(table));
        Ok(())
    }

    /// Compiles each top-level function that isn't in `native_names` to a
    /// `Proto` and binds it in `globals` (top-level functions have no
    /// enclosing Lua function, so they capture no upvalues), seeds `natives`'
    /// bridge bindings, then calls `main`. A name in `native_names` but not
    /// in `natives` is native code with no dynamic caller - nothing dynamic
    /// can reach it, so it's simply skipped rather than compiled or bound.
    pub(super) fn run_in_globals(
        &mut self,
        program: &Program,
        globals: &Globals,
        native_names: &HashSet<String>,
        natives: &HashMap<String, LuaValue>,
    ) -> LuaResult<LuaValue> {
        self.load_in_globals(program, globals, native_names, natives, None)?;
        let main = globals.get("main");
        let values = self.call(main, Vec::new())?;
        Ok(values.into_iter().next().unwrap_or(LuaValue::Nil))
    }

    fn load_in_globals(
        &mut self,
        program: &Program,
        globals: &Globals,
        native_names: &HashSet<String>,
        natives: &HashMap<String, LuaValue>,
        optimization_plan: Option<&crate::typeck::inference::OptimizationPlan>,
    ) -> LuaResult<()> {
        for function in &program.functions {
            if native_names.contains(&function.name) {
                continue;
            }
            let proto = match optimization_plan {
                Some(plan) => Compiler::compile_top_level_with_plan(function, plan),
                None => Compiler::compile_top_level(function),
            }
            .map_err(LuaError::new)?;
            if let Some(chunk_name) = self.default_chunk_name.clone() {
                self.register_chunk_source(&proto, &chunk_name);
            }
            self.charge_allocation(std::mem::size_of::<LuaClosure>())?;
            let closure = self.track_closure(Rc::new(LuaClosure {
                proto,
                upvals: RefCell::new(Vec::new()),
                globals: globals.clone(),
            }));
            globals.define(&function.name, LuaValue::Closure(closure), false);
        }
        for (name, value) in natives {
            let value = match value {
                LuaValue::Native(bridge) => {
                    LuaValue::RegisteredNative(self.register_native_bridge(bridge.clone())?)
                }
                value => value.clone(),
            };
            globals.define(name, value, false);
        }
        Ok(())
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn add(arguments: *const u64, count: i64) -> u64 {
        assert_eq!(count, 2);
        let arguments = unsafe { std::slice::from_raw_parts(arguments, count as usize) };
        arguments[0].wrapping_add(arguments[1])
    }

    #[test]
    fn native_bindings_are_registered_before_entering_the_value_graph() {
        let source = br#"
            function add(a, b) return a + b end
            function main() return add(20, 22) end
        "#;
        let program = crate::parser::parse_lua(crate::lexer::lex_bytes(source).unwrap()).unwrap();
        let mut runtime = LuaRuntime::new();
        let mut native_names = HashSet::new();
        native_names.insert("add".to_owned());
        let mut natives = HashMap::new();
        natives.insert(
            "add".to_owned(),
            LuaValue::Native(Rc::new(NativeBridge {
                name: "add".to_owned(),
                ptr: add as *const u8,
                params: vec![BridgeScalar::I64, BridgeScalar::I64],
                ret: BridgeScalar::I64,
            })),
        );

        assert_eq!(
            runtime
                .run_with_natives(&program, &native_names, natives)
                .unwrap(),
            LuaValue::Integer(42)
        );
        assert!(matches!(
            runtime.globals.get("add"),
            LuaValue::RegisteredNative(_)
        ));
        assert_eq!(runtime.native_bridges.len(), 1);
    }
}
