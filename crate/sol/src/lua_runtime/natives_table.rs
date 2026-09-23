//! the table library (insert/remove/concat/pack/unpack/sort/move/create).
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use crate::ast::BinaryOp;
use indexmap::IndexMap;

use super::frame::*;
use super::tim_sort::{TimSort, TimSortStep};
use super::*;

// Lua's API limits one call/result list to `LUAI_MAXSTACK` slots (one million
// in the pinned Lua 5.5 build). `table.unpack` enforces it before reading any
// values, including when the requested table range is all nil.
const MAX_LUA_RESULTS: i128 = 1_000_000;

impl LuaRuntime {
    pub(super) fn call_native_table(
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
            NativeFunction::TableConcat => {
                let table_value = required(0)?;
                self.expect_table(&table_value)?;
                let separator = args
                    .get(1)
                    .map(|value| self.string(value).map(Vec::from))
                    .transpose()?
                    .unwrap_or_default();
                let start = args
                    .get(2)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(1);
                let end = args
                    .get(3)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(self.length_of(table_value.clone())?);
                let mut output = Vec::new();
                for index in start..=end {
                    if index != start {
                        output.extend(&separator);
                    }
                    let value = self.index_get(table_value.clone(), LuaValue::Integer(index))?;
                    match value {
                        LuaValue::String(_) | LuaValue::Integer(_) | LuaValue::Float(_) => {
                            output.extend(value.display_bytes())
                        }
                        value => {
                            return Err(LuaError::new(format!(
                                "invalid value ({}) at index {index} in table for 'concat'",
                                value.type_name()
                            )))
                        }
                    }
                }
                self.charge_allocation(output.len())?;
                Ok(vec![LuaValue::String(Rc::new(output))])
            }
            NativeFunction::TableInsert => {
                // Defined in terms of the generic `t[i]` get/set (not a
                // direct splice of the internal array-part `Vec`): a table
                // can hold live integer keys - `0`, negatives, or anything
                // past the array part's contiguous prefix - in its hash
                // part, and shifting must go through those too, exactly as
                // real Lua's `lua_geti`/`lua_seti`-based implementation
                // does.
                let table_value = required(0)?;
                self.expect_table(&table_value)?;
                if args.len() != 2 && args.len() != 3 {
                    return Err(LuaError::new("wrong number of arguments to 'insert'"));
                }
                let size = self.length_of(table_value.clone())?;
                // `end` (`size + 1`, real Lua's `e`) and the bounds check use
                // wrapping arithmetic to match real Lua's `(lua_Unsigned)pos
                // - 1u <= (lua_Unsigned)e - 1u`: a `__len` metamethod can
                // report `math.maxinteger`, at which point `e` wraps around
                // to `math.mininteger` rather than erroring.
                let end = size.wrapping_add(1);
                if args.len() == 2 {
                    // No explicit position: real Lua's two-argument case
                    // sets `t[e] = v` directly with no shifting loop at all
                    // (only the three-argument case shifts), which also
                    // sidesteps the wrapped `end` ever driving a bogus loop.
                    let value = required(1)?;
                    self.index_set(table_value, LuaValue::Integer(end), value)?;
                    return Ok(Vec::new());
                }
                let position = self.integer(&required(1)?)?;
                if (position as u64).wrapping_sub(1) > (end as u64).wrapping_sub(1) {
                    return Err(LuaError::new(
                        "bad argument #2 to 'insert' (position out of bounds)",
                    ));
                }
                let value = required(2)?;
                let mut cursor = size;
                while cursor >= position {
                    let moved = self.index_get(table_value.clone(), LuaValue::Integer(cursor))?;
                    self.index_set(
                        table_value.clone(),
                        LuaValue::Integer(cursor.wrapping_add(1)),
                        moved,
                    )?;
                    cursor = cursor.wrapping_sub(1);
                }
                self.index_set(table_value, LuaValue::Integer(position), value)?;
                Ok(Vec::new())
            }
            NativeFunction::TableRemove => {
                // See `TableInsert`: goes through generic get/set so a
                // removal that lands on a hash-resident integer key (e.g.
                // `table.remove(a)` on a table whose only entry is `a[0]`,
                // where `#a == 0` so the default position is `0`) behaves
                // like real Lua instead of only ever touching the array
                // part.
                let table_value = required(0)?;
                self.expect_table(&table_value)?;
                let size = self.length_of(table_value.clone())?;
                let position = args
                    .get(1)
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(size);
                // Real Lua only bounds-checks an explicitly given position;
                // the default (`position == size`) is used unchecked even
                // when `size == 0`, which is what lets `table.remove(a)`
                // observe `a[0]` on such a table.
                if position != size && !(1..=size + 1).contains(&position) {
                    return Err(LuaError::new(
                        "bad argument #2 to 'remove' (position out of bounds)",
                    ));
                }
                let removed = self.index_get(table_value.clone(), LuaValue::Integer(position))?;
                let mut cursor = position;
                while cursor < size {
                    let next =
                        self.index_get(table_value.clone(), LuaValue::Integer(cursor + 1))?;
                    self.index_set(table_value.clone(), LuaValue::Integer(cursor), next)?;
                    cursor += 1;
                }
                self.index_set(table_value, LuaValue::Integer(cursor), LuaValue::Nil)?;
                Ok(vec![removed])
            }
            NativeFunction::TablePack => {
                self.charge_allocation(std::mem::size_of::<LuaTable>())?;
                let value = self.values_table(&args);
                let LuaValue::Table(table) = &value else {
                    unreachable!()
                };
                table.borrow_mut().set(
                    LuaValue::String(Rc::new(b"n".to_vec())),
                    LuaValue::Integer(args.len() as i64),
                )?;
                Ok(vec![value])
            }
            NativeFunction::TableUnpack => {
                // Real Lua's `unpack` is defined in terms of `lua_geti`/
                // `luaL_len`, which go through `__index`/`__len` regardless
                // of the argument's raw type - so a non-table value with
                // those metamethods (e.g. a number after `debug.setmetatable`)
                // is unpackable too, not just plain tables.
                let table_value = required(0)?;
                let start = args
                    .get(1)
                    .filter(|value| !matches!(**value, LuaValue::Nil))
                    .map(|value| self.integer(value))
                    .transpose()?
                    .unwrap_or(1);
                // `unwrap_or` (unlike `unwrap_or_else`) evaluates its
                // argument eagerly, so this must not be
                // `.unwrap_or(self.length_of(...)?)` - that would call
                // `length_of` (and so require the argument to be
                // length-capable) even when `end` was explicitly given.
                let end = match args
                    .get(2)
                    .filter(|value| !matches!(**value, LuaValue::Nil))
                {
                    Some(value) => self.integer(value)?,
                    None => self.length_of(table_value.clone())?,
                };
                // Real Lua rejects a range that would fill its result stack
                // before looping. Widen first: subtracting user-provided i64
                // bounds can overflow.
                if start <= end && (end as i128) - (start as i128) + 1 >= MAX_LUA_RESULTS {
                    return Err(LuaError::new("too many results to unpack"));
                }
                let mut values = Vec::new();
                for index in start..=end {
                    values.push(self.index_get(table_value.clone(), LuaValue::Integer(index))?);
                }
                Ok(values)
            }
            NativeFunction::TableSort => {
                let table_value = required(0)?;
                self.expect_table(&table_value)?;
                // An explicit `nil` comparator (as opposed to omitting the
                // argument entirely) must fall back to the default `<`
                // order, exactly like omitting it - `args.get(1)` alone
                // would otherwise return `Some(LuaValue::Nil)` and later
                // code would try to *call* that nil value as the comparator.
                let comparator = args.get(1).cloned().filter(|v| *v != LuaValue::Nil);
                let size = self.length_of(table_value.clone())?;
                // Real Lua checks this before ever allocating (`ltablib.c`'s
                // `sort`: `luaL_argcheck(L, n < INT_MAX, 1, "array too big")`),
                // so a table whose `__len` reports an enormous size errors
                // cleanly instead of this native attempting a huge `Vec`
                // allocation.
                if size > 1 && size >= i32::MAX as i64 {
                    return Err(LuaError::new("bad argument #1 to 'sort' (array too big)"));
                }
                let mut values = Vec::with_capacity(size.max(0) as usize);
                for index in 1..=size {
                    values.push(self.index_get(table_value.clone(), LuaValue::Integer(index))?);
                }
                if values.len() > 1 {
                    let mut sorter = TimSort::new(values);
                    let mut comparison = None;
                    loop {
                        match sorter
                            .step(comparison.take())
                            .map_err(|_| LuaError::new("invalid order function for sorting"))?
                        {
                            TimSortStep::Done => {
                                values = sorter.into_values();
                                break;
                            }
                            TimSortStep::NeedsComparison { left, right } => {
                                comparison = Some(self.less_blocking(&comparator, left, right)?);
                            }
                        }
                    }
                }
                for (offset, value) in values.into_iter().enumerate() {
                    self.index_set(
                        table_value.clone(),
                        LuaValue::Integer(offset as i64 + 1),
                        value,
                    )?;
                }
                Ok(Vec::new())
            }
            NativeFunction::TableMove => {
                // `table.move(a1, f, e, t [, a2])`: copies `a1[f..e]` to
                // `a2[t...]` (default `a2 = a1`), matching real Lua's
                // `ltablib.c` `tmove`. The forward/backward direction choice
                // and the range-overflow checks mirror that C implementation
                // exactly (widened to `i128` here instead of relying on C's
                // unsigned-wraparound tricks), since the corpus exercises
                // `math.maxinteger`/`mininteger` boundary ranges directly.
                let source = required(0)?;
                let f = self.integer(&required(1)?)?;
                let e = self.integer(&required(2)?)?;
                let t = self.integer(&required(3)?)?;
                let has_destination = args.get(4).is_some_and(|value| *value != LuaValue::Nil);
                let destination = if has_destination {
                    required(4)?
                } else {
                    source.clone()
                };

                // Real Lua's `checktab`: a plain table is always fine; any
                // other value needs the metamethod the access actually uses
                // (`__index` for reads, `__newindex` for writes) - so a
                // proxy scalar can be a source/destination too, but a bare
                // number/etc. with no metatable at all is rejected up front
                // with "table expected" rather than a generic index error.
                let readable = matches!(source, LuaValue::Table(_))
                    || self.metamethod(&source, b"__index")?.is_some();
                if !readable {
                    return Err(LuaError::new(format!(
                        "bad argument #1 to 'move' (table expected, got {})",
                        source.type_name()
                    )));
                }
                let writable = matches!(destination, LuaValue::Table(_))
                    || self.metamethod(&destination, b"__newindex")?.is_some();
                if !writable {
                    return Err(LuaError::new(format!(
                        "bad argument #{} to 'move' (table expected, got {})",
                        if has_destination { 5 } else { 1 },
                        destination.type_name()
                    )));
                }

                if e >= f {
                    // Real Lua: `luaL_argcheck(L, f > 0 || e < LUA_MAXINTEGER + f,
                    // 3, "too many elements to move");`
                    if !(f > 0 || (e as i128) < (i64::MAX as i128) + (f as i128)) {
                        return Err(LuaError::new(
                            "bad argument #3 to 'move' (too many elements to move)",
                        ));
                    }
                    let count = (e as i128) - (f as i128) + 1;
                    // Real Lua: `luaL_argcheck(L, t <= LUA_MAXINTEGER - n + 1,
                    // 4, "destination wrap around");`
                    if !((t as i128) <= (i64::MAX as i128) - count + 1) {
                        return Err(LuaError::new(
                            "bad argument #4 to 'move' (destination wrap around)",
                        ));
                    }
                    let same_table = !has_destination || source == destination;
                    // Copy backward (high-to-low) only when the source and
                    // destination ranges could overlap in the same table;
                    // otherwise always copy forward. This must match real
                    // Lua's direction exactly, not just for the final
                    // result but for the exact sequence of reads/writes a
                    // side-effecting `__index`/`__newindex` can observe.
                    let forward = t > e || t <= f || !same_table;
                    if forward {
                        let mut offset: i128 = 0;
                        while offset < count {
                            let value = self.index_get(
                                source.clone(),
                                LuaValue::Integer((f as i128 + offset) as i64),
                            )?;
                            self.index_set(
                                destination.clone(),
                                LuaValue::Integer((t as i128 + offset) as i64),
                                value,
                            )?;
                            offset += 1;
                        }
                    } else {
                        let mut offset = count - 1;
                        loop {
                            let value = self.index_get(
                                source.clone(),
                                LuaValue::Integer((f as i128 + offset) as i64),
                            )?;
                            self.index_set(
                                destination.clone(),
                                LuaValue::Integer((t as i128 + offset) as i64),
                                value,
                            )?;
                            if offset == 0 {
                                break;
                            }
                            offset -= 1;
                        }
                    }
                }
                Ok(vec![destination])
            }
            NativeFunction::TableCreate => {
                // `table.create(sizeseq [, sizerest])` preallocates the array
                // part to `sizeseq` and the hash part to `sizerest` (ltablib.c's
                // `tcreate`/`lua_createtable`); the 2nd argument is a hash-part
                // size hint, not a fill value.
                const MAX_SIZE_HINT: i64 = i32::MAX as i64;
                // Mirrors ltable.c's MAXHBITS: the hash part is always sized
                // as a power of two whose exponent must fit a 32-bit `int`.
                const MAX_HASH_SIZE_BITS: u32 = 30;

                let sizeseq = self.integer(&required(0)?)?;
                if !(0..=MAX_SIZE_HINT).contains(&sizeseq) {
                    return Err(LuaError::new(format!(
                        "bad argument #1 to '{}' (out of range)",
                        function.name()
                    )));
                }
                let sizerest = match args.get(1) {
                    Some(value) => self.integer(value)?,
                    None => 0,
                };
                if !(0..=MAX_SIZE_HINT).contains(&sizerest) {
                    return Err(LuaError::new(format!(
                        "bad argument #2 to '{}' (out of range)",
                        function.name()
                    )));
                }
                if sizerest > 0 && 32 - (sizerest as u32 - 1).leading_zeros() > MAX_HASH_SIZE_BITS {
                    return Err(LuaError::new("table overflow"));
                }

                let count = sizeseq as usize;
                let nrec = sizerest as usize;
                self.charge_allocation(count * std::mem::size_of::<LuaValue>())?;
                self.charge_allocation(nrec * std::mem::size_of::<LuaValue>() * 2)?;
                let table = LuaTable {
                    array: vec![LuaValue::Nil; count],
                    hash: IndexMap::with_capacity(nrec),
                    ..LuaTable::default()
                };
                Ok(vec![LuaValue::Table(
                    self.track_table(Rc::new(RefCell::new(table))),
                )])
            }
            _ => unreachable!("call_native_table received a non-table NativeFunction"),
        }
    }

    /// Blocking comparison used by the already-blocking `call_native_table`
    /// path. The trampoline path feeds comparisons to the same TimSort state
    /// through `sort_step` below.
    fn less_blocking(
        &mut self,
        comparator: &Option<LuaValue>,
        a: LuaValue,
        b: LuaValue,
    ) -> LuaResult<bool> {
        if let Some(comparator) = comparator {
            Ok(self
                .call(comparator.clone(), vec![a, b])?
                .into_iter()
                .next()
                .unwrap_or(LuaValue::Nil)
                .truthy())
        } else {
            Ok(self.binary(BinaryOp::Lt, a, b)?.truthy())
        }
    }

    /// Drives the dedicated TimSort state one comparison at a time so a
    /// comparator (or `__lt` metamethod) call never blocks on native Rust
    /// recursion.
    pub(super) fn sort_step(
        &mut self,
        state: &mut SortState,
        mut resume: Option<bool>,
    ) -> LuaResult<SortOutcome> {
        loop {
            match state
                .sorter
                .step(resume.take())
                .map_err(|_| LuaError::new("invalid order function for sorting"))?
            {
                TimSortStep::Done => {
                    // Write back only after every comparison succeeded so a
                    // comparator error leaves the original table untouched.
                    let sorter = std::mem::replace(&mut state.sorter, TimSort::new(Vec::new()));
                    for (offset, value) in sorter.into_values().into_iter().enumerate() {
                        self.index_set(
                            state.table.clone(),
                            LuaValue::Integer(offset as i64 + 1),
                            value,
                        )?;
                    }
                    return Ok(SortOutcome::Done(Vec::new()));
                }
                TimSortStep::NeedsComparison { left, right } => {
                    if let Some(comparator) = &state.comparator {
                        return Ok(SortOutcome::NeedsCall {
                            callee: comparator.clone(),
                            args: vec![left, right],
                        });
                    }
                    match self.binary_resolve(BinaryOp::Lt, left, right)? {
                        BinaryResolution::Value(value) => resume = Some(value.truthy()),
                        BinaryResolution::Call { method, args, .. } => {
                            return Ok(SortOutcome::NeedsCall {
                                callee: method,
                                args,
                            });
                        }
                    }
                }
            }
        }
    }
}
