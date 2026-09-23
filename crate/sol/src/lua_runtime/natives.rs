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
            NativeFunction::DebugUpvalueid
            | NativeFunction::DebugUpvaluejoin
            | NativeFunction::DebugSetupvalue
            | NativeFunction::DebugGetinfo
            | NativeFunction::DebugGetmetatable
            | NativeFunction::DebugSetmetatable
            | NativeFunction::DebugTraceback
            | NativeFunction::DebugSethook
            | NativeFunction::DebugGethook
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
            | NativeFunction::OsRemove
            | NativeFunction::OsSetlocale
            | NativeFunction::OsTmpname
            | NativeFunction::IoRead => self.call_native_os_io(function, args),
            NativeFunction::CoroutineCreate
            | NativeFunction::CoroutineResume
            | NativeFunction::CoroutineYield
            | NativeFunction::CoroutineStatus
            | NativeFunction::CoroutineWrap
            | NativeFunction::CoroutineRunning
            | NativeFunction::CoroutineIsYieldable
            | NativeFunction::CoroutineClose => self.call_native_coroutine(function, args),
            NativeFunction::DebugUpvalueid
            | NativeFunction::DebugUpvaluejoin
            | NativeFunction::DebugSetupvalue
            | NativeFunction::DebugGetinfo
            | NativeFunction::DebugGetmetatable
            | NativeFunction::DebugSetmetatable
            | NativeFunction::DebugTraceback
            | NativeFunction::DebugSethook
            | NativeFunction::DebugGethook => self.call_native_debug(function, args),
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
