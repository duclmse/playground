// Host-side support functions linked into the JIT module - the only way
// sol code allocates anything. An `Array<T>` is a pointer to an
// `ArrayHeader` (fits in one SSA value); a struct is just `sol_alloc_layout`'s
// raw bytes, since `StructLayout` fully describes its fields at compile
// time. All allocation goes through gc.rs. Array and map data buffers use
// `sol_gc_alloc_atomic` while their element/value types are scalar and contain
// no GC pointers, so the collector can skip scanning them; a pointer-bearing
// `Array<T>` (see `sol_new_array_ptr`) instead uses a conservative managed
// buffer so its elements are traced.

use crate::gc;

#[repr(C)]
pub struct ArrayHeader {
    pub len: i64,
    pub data: *mut u8,
    #[cfg(target_pointer_width = "32")]
    data_padding: u32,
}

#[repr(C)]
pub struct MapI64Header {
    len: i64,
    capacity: i64,
    keys: *mut i64,
    #[cfg(target_pointer_width = "32")]
    keys_padding: u32,
    values: *mut i64,
    #[cfg(target_pointer_width = "32")]
    values_padding: u32,
    occupied: *mut u8,
    #[cfg(target_pointer_width = "32")]
    occupied_padding: u32,
}

// Pointer fields occupy whole interpreter/GC slots on every target. In
// particular wasm32 must not pack two pointers into one traced 64-bit word.
const _: () = assert!(std::mem::size_of::<ArrayHeader>() == 16);
const _: () = assert!(std::mem::size_of::<MapI64Header>() == 40);
const _: () = assert!(std::mem::offset_of!(ArrayHeader, data) == 8);
const _: () = assert!(std::mem::offset_of!(MapI64Header, keys) == 16);
const _: () = assert!(std::mem::offset_of!(MapI64Header, values) == 24);
const _: () = assert!(std::mem::offset_of!(MapI64Header, occupied) == 32);

impl MapI64Header {
    /// U12 item 4, deliverable 2: every occupied `(key, value)` pair, read
    /// directly off this header's own occupied-bitmap-filtered open-addressed
    /// table (the same layout `map_slot`/`map_insert` above read/write) -
    /// `pub(crate)` only, since this is still a private runtime layout with
    /// no public C-layout contract; `debugger.rs`'s `ValueRenderer::expand`
    /// is the one caller, from inside this crate.
    ///
    /// # Safety
    /// `header` must point to a live `MapI64Header` returned by
    /// `sol_new_map_i64` (or a pointer rebuilt from a `Type::Map` local's raw
    /// register word, which is exactly such a pointer - see `interp.rs`'s own
    /// `Op::Index`/`sol_map_get_i64` call sites for the same assumption).
    pub(crate) unsafe fn entries(header: *const MapI64Header) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        unsafe {
            let capacity = (*header).capacity as usize;
            for slot in 0..capacity {
                if *(*header).occupied.add(slot) != 0 {
                    let key = *(*header).keys.add(slot);
                    let value = *(*header).values.add(slot);
                    out.push((key, value));
                }
            }
        }
        out
    }
}

// The map runtime stores scalar values as opaque 64-bit slots. Typed lowering
// bitcasts f64 and widens bool at the ABI boundary; i64 values pass through.
// Pointer-bearing values remain gated on precise GC layouts.

#[no_mangle]
pub extern "C" fn sol_new_map_i64() -> *mut MapI64Header {
    unsafe { alloc_map(8) }
}

unsafe fn alloc_map(capacity: usize) -> *mut MapI64Header {
    // len/capacity are scalars; keys, values, and occupancy are managed
    // buffers. Keep that distinction precise instead of scanning all words.
    let header = gc::sol_gc_alloc_layout(std::mem::size_of::<MapI64Header>() as i64, 0b1_1100)
        as *mut MapI64Header;
    (*header).len = 0;
    (*header).capacity = capacity as i64;
    (*header).keys = gc::sol_gc_alloc_atomic((capacity * 8) as i64) as *mut i64;
    (*header).values = gc::sol_gc_alloc_atomic((capacity * 8) as i64) as *mut i64;
    (*header).occupied = gc::sol_gc_alloc_atomic(capacity as i64);
    #[cfg(target_pointer_width = "32")]
    {
        (*header).keys_padding = 0;
        (*header).values_padding = 0;
        (*header).occupied_padding = 0;
    }
    header
}

fn map_slot(map: *mut MapI64Header, key: i64) -> (usize, bool) {
    unsafe {
        let capacity = (*map).capacity as usize;
        let mut slot = (key as u64).wrapping_mul(0x9e3779b97f4a7c15) as usize & (capacity - 1);
        loop {
            if *(*map).occupied.add(slot) == 0 {
                return (slot, false);
            }
            if *(*map).keys.add(slot) == key {
                return (slot, true);
            }
            slot = (slot + 1) & (capacity - 1);
        }
    }
}

unsafe fn map_insert(map: *mut MapI64Header, key: i64, value: i64) {
    let (slot, present) = map_slot(map, key);
    *(*map).keys.add(slot) = key;
    *(*map).values.add(slot) = value;
    *(*map).occupied.add(slot) = 1;
    if !present {
        (*map).len += 1;
    }
}

#[no_mangle]
/// # Safety
/// `map` must point to a live header returned by `sol_new_map_i64`.
pub unsafe extern "C" fn sol_map_set_i64(map: *mut MapI64Header, key: i64, value: i64) -> i64 {
    unsafe {
        let (_, present) = map_slot(map, key);
        if !present && ((*map).len + 1) * 10 >= (*map).capacity * 7 {
            let old_capacity = (*map).capacity as usize;
            let old_keys = (*map).keys;
            let old_values = (*map).values;
            let old_occupied = (*map).occupied;
            let replacement = alloc_map(old_capacity * 2);
            for slot in 0..old_capacity {
                if *old_occupied.add(slot) != 0 {
                    map_insert(replacement, *old_keys.add(slot), *old_values.add(slot));
                }
            }
            *map = std::ptr::read(replacement);
        }
        map_insert(map, key, value);
    }
    value
}

#[no_mangle]
/// # Safety
/// `map` must point to a live header returned by `sol_new_map_i64`.
pub unsafe extern "C" fn sol_map_get_i64(map: *mut MapI64Header, key: i64) -> i64 {
    let (slot, present) = map_slot(map, key);
    if present {
        unsafe { *(*map).values.add(slot) }
    } else {
        0
    }
}

#[no_mangle]
/// # Safety
/// `map` must point to a live header returned by `sol_new_map_i64`.
pub unsafe extern "C" fn sol_map_next_i64(map: *mut MapI64Header, cursor: i64) -> i64 {
    unsafe {
        let mut slot = cursor.max(0) as usize;
        while slot < (*map).capacity as usize {
            if *(*map).occupied.add(slot) != 0 {
                return slot as i64 + 1;
            }
            slot += 1;
        }
    }
    0
}

#[no_mangle]
/// # Safety
/// `map` must be live and `cursor` must be a nonzero cursor returned by
/// `sol_map_next_i64` for that map.
pub unsafe extern "C" fn sol_map_key_at_i64(map: *mut MapI64Header, cursor: i64) -> i64 {
    unsafe { *(*map).keys.add((cursor - 1) as usize) }
}

#[no_mangle]
/// # Safety
/// `map` must be live and `cursor` must be a nonzero cursor returned by
/// `sol_map_next_i64` for that map.
pub unsafe extern "C" fn sol_map_value_at_i64(map: *mut MapI64Header, cursor: i64) -> i64 {
    unsafe { *(*map).values.add((cursor - 1) as usize) }
}

#[no_mangle]
pub extern "C" fn sol_new_array_i64(len: i64) -> *mut ArrayHeader {
    new_array(len, true)
}

#[no_mangle]
/// # Safety
/// `input` must be a live `Array<i64>` and `callback` must use the declared
/// `fn(i64) -> i64` ABI.
pub unsafe extern "C" fn sol_array_map_i64(
    input: *mut ArrayHeader,
    callback: unsafe extern "C" fn(i64) -> i64,
) -> *mut ArrayHeader {
    let output = sol_new_array_i64(unsafe { (*input).len });
    for index in 0..unsafe { (*input).len as usize } {
        let value = unsafe { *((*input).data.add(index * 8) as *const i64) };
        let mapped = unsafe { callback(value) };
        unsafe { *((*output).data.add(index * 8) as *mut i64) = mapped };
    }
    output
}

#[no_mangle]
pub extern "C" fn sol_new_array_f64(len: i64) -> *mut ArrayHeader {
    new_array(len, true)
}

#[no_mangle]
/// # Safety
/// `input` must be a live `Array<f64>` and `callback` must use the declared
/// `fn(f64) -> f64` ABI.
pub unsafe extern "C" fn sol_array_map_f64(
    input: *mut ArrayHeader,
    callback: unsafe extern "C" fn(f64) -> f64,
) -> *mut ArrayHeader {
    let output = sol_new_array_f64(unsafe { (*input).len });
    for index in 0..unsafe { (*input).len as usize } {
        let value = unsafe { *((*input).data.add(index * 8) as *const f64) };
        let mapped = unsafe { callback(value) };
        unsafe { *((*output).data.add(index * 8) as *mut f64) = mapped };
    }
    output
}

/// `Array<T>` where `T` is pointer-bearing (struct/string/array/map/any) -
/// unlike `sol_new_array_i64`/`sol_new_array_f64`, the data buffer must not
/// be atomic: it holds real GC pointers the collector needs to trace into.
#[no_mangle]
pub extern "C" fn sol_new_array_ptr(len: i64) -> *mut ArrayHeader {
    new_array(len, false)
}

fn new_array(len: i64, atomic_data: bool) -> *mut ArrayHeader {
    if len < 0 {
        std::process::abort();
    }
    let data_bytes = len.checked_mul(8).unwrap_or_else(|| std::process::abort());
    let data = if atomic_data {
        gc::sol_gc_alloc_atomic(data_bytes.max(1))
    } else {
        // Zeroed like the atomic path: an unwritten pointer-array slot
        // must read as nil (all-zero), not garbage bump-arena bytes.
        let ptr = gc::sol_gc_alloc(data_bytes.max(1));
        unsafe { std::ptr::write_bytes(ptr, 0, data_bytes.max(1) as usize) };
        ptr
    };
    let header = gc::sol_gc_alloc_layout(16, 0b10) as *mut ArrayHeader;
    unsafe {
        (*header).len = len;
        (*header).data = data;
        #[cfg(target_pointer_width = "32")]
        {
            (*header).data_padding = 0;
        }
    }
    header
}

/// Backs struct literals: a zeroed buffer `codegen.rs` fills field-by-field
/// right after. Zeroing is just a safety net - fields are always fully
/// initialized.
#[no_mangle]
pub extern "C" fn sol_alloc(size_bytes: i64) -> *mut u8 {
    gc::sol_gc_alloc(size_bytes)
}

/// Backs typed records, closure environments, and `any` boxes whose exact
/// pointer-bearing slots are known by lowering.
#[no_mangle]
pub extern "C" fn sol_alloc_layout(size_bytes: i64, pointer_mask: u64) -> *mut u8 {
    gc::sol_gc_alloc_layout(size_bytes, pointer_mask)
}

// AOT-only entry helpers (aot.rs's generated `main` calls these by symbol
// name via the linked staticlib - the JIT path calls gc::init_stack_base
// and println! directly from Rust in main.rs, no wrapper needed there).

#[no_mangle]
pub extern "C" fn sol_gc_init_stack_base() {
    gc::init_stack_base();
}

#[no_mangle]
pub extern "C" fn sol_print_i64(v: i64) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn sol_print_f64(v: f64) {
    println!("{v}");
}

#[no_mangle]
pub extern "C" fn sol_print_bool(v: i64) {
    println!("{}", v != 0);
}
