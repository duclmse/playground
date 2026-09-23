//! Runtime-facing Lua value names used in compatibility diagnostics.
//!
//! These depend on runtime-owned classifications (for example, the standard
//! file handles), so they cannot live solely on `LuaValue`. Keeping them here
//! prevents construction/bootstrap code from becoming the accidental home for
//! errors emitted by the dispatcher and native libraries.

use super::*;

impl LuaRuntime {
    /// The Lua-visible type name for diagnostics, including host-owned
    /// standard file handles whose metatable name is `FILE*`.
    pub(super) fn error_type_name(&self, value: &LuaValue) -> &'static str {
        match value {
            LuaValue::Userdata(userdata) if self.file_userdata.contains(&userdata.object_id()) => {
                "FILE*"
            }
            _ => value.type_name(),
        }
    }

    /// The name used when Lua reports an operand type. A table's `__name`
    /// customizes this label; other values use their normal type name.
    pub(super) fn error_type_label(&self, value: &LuaValue) -> String {
        if let LuaValue::Table(table) = value {
            if let Some(metatable) = &table.borrow().metatable {
                if let Ok(LuaValue::String(name)) = metatable
                    .borrow()
                    .get(&LuaValue::String(Rc::new(b"__name".to_vec())))
                {
                    return String::from_utf8_lossy(&name).into_owned();
                }
            }
        }
        self.error_type_name(value).to_string()
    }
}
