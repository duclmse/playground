// Object registry - hands out stable ids for tables/functions/threads/
// userdata so the UI can lazily expand them (debug-protocol.md's
// "Reference identity (ObjectRegistry)": `a` and `b` aliasing the same
// table must serialize as the same id, and self-referential tables
// (`t.self = t`) must not infinite-loop the serializer). Values are
// `ctx.stash()`ed (piccolo's `DynamicRoot` mechanism - see registry.rs)
// so holding an id keeps the underlying GC object alive and its identity
// stable, rather than just capturing a pointer that GC could otherwise
// invalidate once nothing else references it.

use gc_arena::Gc;
use vm::{
    Context, Function, StashedClosure, StashedTable, StashedThread, StashedUserData, Table, Value,
};

use crate::display_value;

use super::Variable;

enum StashedRef {
    Table(StashedTable),
    // Function/Thread/UserData are stashed (rooted, kept alive, given a
    // stable id) but never read back out - only `Table` has a getter
    // (`get_table_entries`/`get_metatable`) today, per `marshal_value`'s
    // doc comment. The `#[allow(dead_code)]`s are for that: the variants
    // exist so `ObjectRegistry::register` can hand out a real, stable
    // reference for these kinds too (ready for a future
    // `get_function_info`/`get_thread_info`), not because this is
    // unfinished plumbing left by accident.
    Function(#[allow(dead_code)] StashedClosure),
    Thread(#[allow(dead_code)] StashedThread),
    UserData(#[allow(dead_code)] StashedUserData),
}

#[derive(Default)]
pub(super) struct ObjectRegistry {
    next_id: u32,
    objects: std::collections::HashMap<u32, StashedRef>,
    by_ptr: std::collections::HashMap<usize, u32>,
}

impl ObjectRegistry {
    /// Clears all handed-out ids. Called on every new "stopped" event
    /// (breakpoint/step/exception) - matches standard DAP behavior where a
    /// `variablesReference` is only valid until the next stop, so nothing
    /// depends on old ids surviving a resume.
    pub(super) fn reset(&mut self) {
        self.objects.clear();
        self.by_ptr.clear();
        self.next_id = 0;
    }

    /// Registers `value` (must be Table/Function/Thread/UserData - the only
    /// "expandable" `LuaValue` kinds) and returns its id, reusing the
    /// existing id if this exact object was already registered since the
    /// last `reset()` - this is what gives aliased references (`local b =
    /// a`) the same id rather than two different ones.
    pub(super) fn register<'gc>(&mut self, ctx: Context<'gc>, value: Value<'gc>) -> Option<u32> {
        let ptr = object_ptr(value)?;
        if let Some(&id) = self.by_ptr.get(&ptr) {
            return Some(id);
        }
        let stashed = match value {
            Value::Table(t) => StashedRef::Table(ctx.stash(t)),
            Value::Function(Function::Closure(c)) => StashedRef::Function(ctx.stash(c)),
            Value::Function(Function::Callback(_)) => return None, // host callbacks aren't expandable
            Value::Thread(t) => StashedRef::Thread(ctx.stash(t)),
            Value::UserData(u) => StashedRef::UserData(ctx.stash(u)),
            _ => return None,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.objects.insert(id, stashed);
        self.by_ptr.insert(ptr, id);
        Some(id)
    }

    pub(super) fn table<'gc>(&self, ctx: Context<'gc>, id: u32) -> Option<Table<'gc>> {
        match self.objects.get(&id)? {
            StashedRef::Table(t) => Some(ctx.fetch(t)),
            _ => None,
        }
    }
}

/// Stable pointer identity for the "expandable" `Value` kinds, used as the
/// `ObjectRegistry`'s aliasing key. gc-arena's `Gc<T>` is non-moving, so this
/// pointer stays valid for the object's lifetime (see risks.md's fork notes
/// for why that assumption is safe here).
fn object_ptr(value: Value) -> Option<usize> {
    match value {
        Value::Table(t) => Some(Gc::as_ptr(t.into_inner()) as usize),
        Value::Function(Function::Closure(c)) => Some(Gc::as_ptr(c.into_inner()) as usize),
        Value::Thread(t) => Some(Gc::as_ptr(t.into_inner()) as usize),
        Value::UserData(u) => Some(Gc::as_ptr(u.into_inner()) as usize),
        _ => None,
    }
}

fn value_type_name(value: Value) -> &'static str {
    value.type_name()
}

/// Builds the `LuaValue` shape from docs/debug-protocol.md#value--inspector-model
/// (`type`/`display`/`expandable`/`reference`) for one Lua value.
pub(super) fn marshal_value<'gc>(
    ctx: Context<'gc>,
    registry: &mut ObjectRegistry,
    value: Value<'gc>,
) -> Variable {
    let reference = registry.register(ctx, value);
    // Only tables are actually drillable via `get_table_entries`/
    // `get_metatable` right now - functions/threads/userdata are still
    // *registered* (for identity and to keep them alive; see
    // `ObjectRegistry`'s doc comment) so a future `get_function_info`/
    // `get_thread_info` etc. can use the same reference scheme, but the UI
    // shouldn't render them as expandable until such a getter exists. See
    // docs/phase-4-8-implementation.md.
    let expandable = matches!(value, Value::Table(_)) && reference.is_some();
    Variable {
        name: std::string::String::new(),
        value_type: value_type_name(value).to_string(),
        display: display_value(value),
        expandable,
        reference,
    }
}
