//! Native (Rust-implemented) Lua standard-library function dispatch
//! (`call_native`): the capability pre-check, the outer dispatch table
//! that routes each `NativeFunction` to its category's implementation
//! (`natives_core.rs`, `natives_string.rs`, `natives_table.rs`,
//! `natives_math.rs`, `natives_utf8.rs`, `natives_os_io.rs`,
//! `natives_coroutine.rs`, `natives_debug.rs`, `natives_load.rs`), and a
//! few small coercion helpers used across every category.

use super::util::*;
use super::*;

impl LuaRuntime {
    pub(super) fn call_native(
        &mut self,
        function: NativeFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let denied_capability = match function {
            NativeFunction::OsTime | NativeFunction::OsClock if !self.capabilities.clock => {
                Some("clock")
            }
            NativeFunction::OsDate if args.get(1).is_none() && !self.capabilities.clock => {
                Some("clock")
            }
            NativeFunction::OsGetenv if !self.capabilities.environment => Some("environment"),
            NativeFunction::OsExit if !self.capabilities.process => Some("process"),
            NativeFunction::IoRead if !self.capabilities.stdin => Some("stdin"),
            NativeFunction::IoWrite | NativeFunction::FileWrite if !self.capabilities.stdout => {
                Some("stdout")
            }
            // `io.output()`/`file:close()` with no path/real file involved
            // (querying the current handle, or closing the stdout handle)
            // don't need `filesystem` - only actually opening a real file
            // (`io.output(path)`) does, checked inside that arm itself so
            // the no-argument forms stay available under `stdout` alone.
            NativeFunction::OsRemove | NativeFunction::OsTmpname
                if !self.capabilities.filesystem =>
            {
                Some("filesystem")
            }
            NativeFunction::DebugGetupvalue
            | NativeFunction::DebugUpvalueid
            | NativeFunction::DebugUpvaluejoin
            | NativeFunction::DebugSetupvalue
            | NativeFunction::DebugGetinfo
            | NativeFunction::DebugGetmetatable
            | NativeFunction::DebugSetmetatable
            | NativeFunction::DebugTraceback
            | NativeFunction::DebugSethook
            | NativeFunction::DebugGethook
            | NativeFunction::DebugSetuservalue
                if !self.capabilities.debug =>
            {
                Some("debug")
            }
            _ => None,
        };
        if let Some(capability) = denied_capability {
            return Err(LuaError::new(format!(
                "{capability} capability is disabled; enable it explicitly"
            )));
        }
        match function {
            NativeFunction::Print
            | NativeFunction::Assert
            | NativeFunction::Type
            | NativeFunction::ToString
            | NativeFunction::ToNumber
            | NativeFunction::RawGet
            | NativeFunction::RawSet
            | NativeFunction::RawEqual
            | NativeFunction::RawLen
            | NativeFunction::GetMetatable
            | NativeFunction::SetMetatable
            | NativeFunction::Error
            | NativeFunction::PCall
            | NativeFunction::XCall
            | NativeFunction::Select
            | NativeFunction::Next
            | NativeFunction::Pairs
            | NativeFunction::IPairs
            | NativeFunction::IPairsIterator
            | NativeFunction::CollectGarbage => self.call_native_core(function, args),
            NativeFunction::StringLen
            | NativeFunction::StringByte
            | NativeFunction::StringChar
            | NativeFunction::StringLower
            | NativeFunction::StringUpper
            | NativeFunction::StringReverse
            | NativeFunction::StringRep
            | NativeFunction::StringDump
            | NativeFunction::StringSub
            | NativeFunction::StringFind
            | NativeFunction::StringMatch
            | NativeFunction::StringGMatch
            | NativeFunction::StringGSub
            | NativeFunction::StringFormat
            | NativeFunction::StringPack
            | NativeFunction::StringUnpack
            | NativeFunction::StringPackSize => self.call_native_string(function, args),
            NativeFunction::TableConcat
            | NativeFunction::TableInsert
            | NativeFunction::TableRemove
            | NativeFunction::TablePack
            | NativeFunction::TableUnpack
            | NativeFunction::TableSort
            | NativeFunction::TableMove
            | NativeFunction::TableCreate => self.call_native_table(function, args),
            NativeFunction::MathAbs
            | NativeFunction::MathFloor
            | NativeFunction::MathCeil
            | NativeFunction::MathMin
            | NativeFunction::MathMax
            | NativeFunction::MathToInteger
            | NativeFunction::MathType
            | NativeFunction::MathSqrt
            | NativeFunction::MathSin
            | NativeFunction::MathCos
            | NativeFunction::MathTan
            | NativeFunction::MathExp
            | NativeFunction::MathLog
            | NativeFunction::MathAcos
            | NativeFunction::MathAsin
            | NativeFunction::MathDeg
            | NativeFunction::MathRad
            | NativeFunction::MathAtan
            | NativeFunction::MathFmod
            | NativeFunction::MathModf
            | NativeFunction::MathUlt
            | NativeFunction::MathFrexp
            | NativeFunction::MathLdexp
            | NativeFunction::MathRandom
            | NativeFunction::MathRandomSeed => self.call_native_math(function, args),
            NativeFunction::Utf8Len
            | NativeFunction::Utf8Char
            | NativeFunction::Utf8Codepoint
            | NativeFunction::Utf8Offset
            | NativeFunction::Utf8Codes
            | NativeFunction::Utf8IteratorStrict
            | NativeFunction::Utf8IteratorLax => self.call_native_utf8(function, args),
            NativeFunction::OsTime
            | NativeFunction::OsClock
            | NativeFunction::OsDifftime
            | NativeFunction::OsDate
            | NativeFunction::OsGetenv
            | NativeFunction::OsExit
            | NativeFunction::IoWrite
            | NativeFunction::FileWrite
            | NativeFunction::IoOutput
            | NativeFunction::FileClose
            | NativeFunction::FileGc
            | NativeFunction::OsRemove
            | NativeFunction::OsSetlocale
            | NativeFunction::OsTmpname
            | NativeFunction::IoRead
            | NativeFunction::IoInput => self.call_native_os_io(function, args),
            NativeFunction::CoroutineCreate
            | NativeFunction::CoroutineResume
            | NativeFunction::CoroutineYield
            | NativeFunction::CoroutineStatus
            | NativeFunction::CoroutineWrap
            | NativeFunction::CoroutineRunning
            | NativeFunction::CoroutineIsYieldable
            | NativeFunction::CoroutineClose => self.call_native_coroutine(function, args),
            NativeFunction::DebugGetupvalue
            | NativeFunction::DebugUpvalueid
            | NativeFunction::DebugUpvaluejoin
            | NativeFunction::DebugSetupvalue
            | NativeFunction::DebugGetinfo
            | NativeFunction::DebugGetmetatable
            | NativeFunction::DebugSetmetatable
            | NativeFunction::DebugTraceback
            | NativeFunction::DebugSethook
            | NativeFunction::DebugGethook
            | NativeFunction::DebugSetuservalue => self.call_native_debug(function, args),
            NativeFunction::Require
            | NativeFunction::PackageSearchPath
            | NativeFunction::PackageSearcherPreload
            | NativeFunction::PackageSearcherLua
            | NativeFunction::PackageSearcherC
            | NativeFunction::PackageSearcherCRoot
            | NativeFunction::PackageLoadLib
            | NativeFunction::Load
            | NativeFunction::DoFile => self.call_native_load(function, args),
        }
    }

    pub(super) fn string<'a>(&self, value: &'a LuaValue) -> LuaResult<&'a [u8]> {
        match value {
            LuaValue::String(value) => Ok(value),
            _ => Err(LuaError::new("string expected")),
        }
    }

    pub(super) fn integer(&self, value: &LuaValue) -> LuaResult<i64> {
        coerce_integer(value)
    }

    /// Real Lua's `luaL_checklstring` (`lauxlib.c`) reports a type-mismatched
    /// argument as `"bad argument #N to 'FUNCTION' (string expected, got
    /// TYPE)"`, not the bare `"string expected"` `string()` produces on its
    /// own - `annotate_bad_argument_error` (`dispatch.rs`) needs this exact
    /// shape to further rewrite a method call's failing self argument into
    /// `"calling 'FUNCTION' on bad self (...)"` (`lua-5.5.1-tests/errors.lua`'s
    /// `aaa:sub()` test).
    pub(super) fn checked_string<'a>(
        &self,
        value: &'a LuaValue,
        index: usize,
        function: &str,
    ) -> LuaResult<&'a [u8]> {
        self.string(value).map_err(|_| {
            LuaError::new(format!(
                "bad argument #{} to '{}' (string expected, got {})",
                index + 1,
                function,
                self.error_type_label(value)
            ))
        })
    }

    /// The `checked_integer` counterpart of `checked_string`, matching real
    /// Lua's `luaL_checkinteger`. A non-integral float keeps `coerce_integer`'s
    /// own "number has no integer representation" wording (real Lua reports
    /// this exact phrase too), rather than the generic "number expected, got
    /// TYPE" used for every other mismatch.
    pub(super) fn checked_integer(
        &self,
        value: &LuaValue,
        index: usize,
        function: &str,
    ) -> LuaResult<i64> {
        self.integer(value).map_err(|err| {
            let extra = if err.message == "number has no integer representation" {
                err.message
            } else {
                format!("number expected, got {}", self.error_type_label(value))
            };
            LuaError::new(format!(
                "bad argument #{} to '{}' ({extra})",
                index + 1,
                function
            ))
        })
    }

    pub(super) fn values_table(&self, values: &[LuaValue]) -> LuaValue {
        let mut table = LuaTable::default();
        for (index, value) in values.iter().enumerate() {
            table
                .set(LuaValue::Integer(index as i64 + 1), value.clone())
                .unwrap();
        }
        LuaValue::Table(self.track_table(Rc::new(RefCell::new(table))))
    }
}
