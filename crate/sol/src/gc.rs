// A conservative generational mark-sweep GC over a chunked bump arena.
// Design notes:
//
// - Conservative root scanning (Boehm-GC style): scan every aligned word of the
//   native stack between the current SP and a recorded base, treating an exact
//   match to a live block's start address as a root.
//   `flush_callee_saved_registers` spills registers first so a register-only
//   pointer (never yet spilled to the stack) isn't missed.
// - Chunked bump allocation, not per-block `alloc`/`dealloc`: cheaper (no
//   hashing, no allocator bookkeeping per object - measured as the dominant
//   cost of an earlier `HashMap`-based version; a chunk with nothing reachable
//   after a collection is reset and reused whole. Coarser than a real
//   generational collector (one surviving object keeps the whole chunk's dead
//   space) but avoids needing to move/compact objects, which conservative
//   root-finding can't safely do anyway.
// - A per-chunk bitset (not a hash table) tracks live block starts - O(1) bit
//   tests plus a binary search over chunk address ranges.
// - Exact-address matching only: every Array/Struct value is always a block's
//   own base address, never an interior pointer.
// - Typed allocations carry a compact pointer-slot bitmap, so their scalar
//   words are never mistaken for edges. Legacy/dynamic allocations retain a
//   conservative all-slots layout until their object representation migrates.
// - Generations are tracked at chunk granularity, not per-object - this
//   collector never moves/compacts objects (conservative root-finding can't
//   safely do that), so "promotion" just relabels a whole surviving chunk `Old`
//   in place rather than copying it out of a young space. A minor collection
//   (`collect_minor`) only rescans `Young` chunks plus the remembered set
//   (every `Old` chunk with its `dirty` bit set, populated by
//   `sol_gc_write_barrier` at `AssignField`/array-element mutation sites). Only
//   a major collection (`collect_heap`) may ever *clear* a `dirty` bit, by
//   re-deriving the true Old -> Young edge set from its own full trace - see
//   `collect_heap`'s comments for why a minor collection can't safely do this
//   itself.
// - Selective zeroing: `sol_gc_alloc` skips it (callers always overwrite
//   immediately); `sol_gc_alloc_atomic` (array data) always zeroes, since
//   unwritten array elements have a real language-level zero-read guarantee to
//   uphold.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

const ALIGN: usize = 8;
const HEADER: usize = 8;

const _: () = assert!(
    std::mem::size_of::<usize>() == 8,
    "gc.rs assumes a 64-bit target (matches flush_callee_saved_registers's aarch64/x86_64 scope)"
);

/// Size of a freshly-grown chunk. A large single allocation gets its own,
/// bigger dedicated chunk (see `alloc_in_heap`) rather than being rejected.
const CHUNK_SIZE: usize = 1 << 16; // 64 KiB

fn round_up(n: usize, align: usize) -> usize {
    (n + align - 1) & !(align - 1)
}

fn bitset_words(slots: usize) -> usize {
    slots.div_ceil(64)
}

fn get_bit(bits: &[u64], idx: usize) -> bool {
    (bits[idx / 64] >> (idx % 64)) & 1 != 0
}

fn set_bit(bits: &mut [u64], idx: usize) {
    bits[idx / 64] |= 1u64 << (idx % 64);
}

fn clear_bits(bits: &mut [u64]) {
    bits.iter_mut().for_each(|w| *w = 0);
}

fn count_set_bits(bits: &[u64]) -> usize {
    bits.iter().map(|w| w.count_ones() as usize).sum()
}

fn read_usize(bytes: &[u8], off: usize) -> usize {
    usize::from_ne_bytes(bytes[off..off + 8].try_into().unwrap())
}

/// A chunk's generation - tracked per-chunk, not per-object, since this
/// collector never moves/compacts objects (see the module doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    Young,
    Old,
}

/// One bump-allocated region. `data`'s address is stable for its whole
/// lifetime, so reusing a dead chunk never invalidates a `base`-relative
/// address.
struct Chunk {
    data: Box<[u8]>,
    base: usize,
    bump: usize,
    /// Bit `i` set: a block's payload starts at byte offset `i * ALIGN`.
    starts: Vec<u64>,
    /// Transient per-collection "already visited" bits.
    marked: Vec<u64>,
    /// Bit `i` set: block at slot `i` is guaranteed pointer-free (see
    /// `sol_gc_alloc_atomic`) - still marked reachable, never traced
    /// into. Persists across collections, unlike `marked`.
    atomic: Vec<u64>,
    /// Per-object pointer-slot bitmap. `u64::MAX` is the conservative/all
    /// layout used by legacy callers; any other value precisely identifies
    /// pointer-bearing 8-byte slots in the first 64 words.
    layouts: Vec<u64>,
    /// True if anything in this chunk was found reachable this cycle.
    any_marked: bool,
    /// New chunks start `Young`; a chunk found reachable at the end of a
    /// collection while still `Young` is promoted to `Old` in place (see the
    /// module doc comment) - never demoted back except via `reset`, which
    /// only ever runs on an entirely dead chunk.
    generation: Generation,
    /// `Old`-only remembered-set bit: "this chunk might hold a pointer into
    /// `Young` space". Set (only ever set, never cleared) by
    /// `sol_gc_write_barrier`; cleared only by `collect_heap`'s precise
    /// re-derivation pass. Meaningless (left stale) on a `Young` chunk.
    dirty: bool,
}

impl Chunk {
    fn new(capacity: usize) -> Self {
        let data = vec![0u8; capacity].into_boxed_slice();
        let base = data.as_ptr() as usize;
        let words = bitset_words(capacity / ALIGN);
        Chunk {
            data,
            base,
            bump: 0,
            starts: vec![0u64; words],
            marked: vec![0u64; words],
            atomic: vec![0u64; words],
            layouts: vec![0u64; capacity / ALIGN],
            any_marked: false,
            generation: Generation::Young,
            dirty: false,
        }
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }

    fn has_room(&self, total: usize) -> bool {
        self.bump + total <= self.capacity()
    }

    /// Carves out `HEADER + payload_size` bytes at the bump pointer.
    /// Hot path - uses raw pointer writes (safe: always in range, since
    /// callers only call this after `has_room` confirms space).
    fn carve(
        &mut self,
        payload_size: usize,
        atomic: bool,
        needs_zero: bool,
        pointer_mask: u64,
    ) -> *mut u8 {
        let pos = self.bump;
        let payload_off = pos + HEADER;
        debug_assert!(
            payload_off + payload_size <= self.data.len(),
            "carve called without a prior has_room check"
        );
        // SAFETY: has_room already confirmed room.
        unsafe {
            std::ptr::write_unaligned(self.data.as_mut_ptr().add(pos) as *mut usize, payload_size)
        };
        self.bump = payload_off + payload_size;
        if needs_zero {
            self.data[payload_off..payload_off + payload_size].fill(0);
        }
        let slot = payload_off / ALIGN;
        // SAFETY: bitsets are sized to capacity / ALIGN bits.
        unsafe { *self.starts.get_unchecked_mut(slot / 64) |= 1u64 << (slot % 64) };
        if atomic {
            unsafe { *self.atomic.get_unchecked_mut(slot / 64) |= 1u64 << (slot % 64) };
        }
        self.layouts[slot] = pointer_mask;
        (self.base + payload_off) as *mut u8
    }

    /// Resets bookkeeping for reuse (no bulk zero-fill - `carve` zeroes
    /// only what needs it). Only ever called on an entirely dead chunk, so
    /// it's safe to hand back out as fresh `Young` space.
    fn reset(&mut self) {
        self.bump = 0;
        clear_bits(&mut self.starts);
        clear_bits(&mut self.atomic);
        self.layouts.fill(0);
        self.generation = Generation::Young;
        self.dirty = false;
    }
}

struct Heap {
    chunks: Vec<Chunk>,
    /// `(base, end, chunk_index)`, sorted by `base` for binary search.
    index: Vec<(usize, usize, usize)>,
    /// Chunk currently being bumped into.
    current: usize,
    total_capacity: usize,
}

impl Heap {
    fn new() -> Self {
        let chunk = Chunk::new(CHUNK_SIZE);
        let base = chunk.base;
        let end = base + chunk.capacity();
        Heap {
            chunks: vec![chunk],
            index: vec![(base, end, 0)],
            current: 0,
            total_capacity: CHUNK_SIZE,
        }
    }

    fn push_chunk(&mut self, capacity: usize) -> usize {
        let chunk = Chunk::new(capacity);
        let base = chunk.base;
        let end = base + chunk.capacity();
        self.chunks.push(chunk);
        let idx = self.chunks.len() - 1;
        let pos = self.index.partition_point(|&(b, _, _)| b < base);
        self.index.insert(pos, (base, end, idx));
        self.total_capacity += capacity;
        idx
    }

    fn find_chunk(&self, addr: usize) -> Option<usize> {
        let pos = self.index.partition_point(|&(b, _, _)| b <= addr);
        if pos == 0 {
            return None;
        }
        let (b, e, idx) = self.index[pos - 1];
        if addr >= b && addr < e {
            Some(idx)
        } else {
            None
        }
    }
}

thread_local! {
    static HEAP: RefCell<Heap> = RefCell::new(Heap::new());
}

/// Native stack address captured before the first JIT call; root scanning walks
/// from the current SP up to this base. 0 = uninitialized (e.g. a unit test)
/// - `collect` treats that as nothing-to-scan.
static STACK_BASE: AtomicUsize = AtomicUsize::new(0);

/// Call once, right before entering JIT'd code for the first time.
pub fn init_stack_base() {
    let sentinel: u8 = 0;
    STACK_BASE.store(&sentinel as *const u8 as usize, Ordering::Relaxed);
}

// interp.rs's register file lives on the heap, invisible to the native
// stack scan - each active interpreted call registers its address range
// here (RAII via RootGuard) so collect_heap can scan it too. Without this,
// a collection could free something referenced only by an interpreted
// local.
thread_local! {
    static EXTRA_ROOTS: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard registering `[ptr, ptr + len*8)` as an extra root range.
pub struct RootGuard;

impl RootGuard {
    pub fn new(ptr: *const u64, len: usize) -> Self {
        EXTRA_ROOTS.with(|r| r.borrow_mut().push((ptr as usize, len)));
        RootGuard
    }
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        EXTRA_ROOTS.with(|r| {
            r.borrow_mut().pop();
        });
    }
}

/// Allocates a block that may itself hold GC pointers (struct instances, array
/// headers). Not zeroed - callers always overwrite every byte.
pub extern "C" fn sol_gc_alloc(size_bytes: i64) -> *mut u8 {
    gc_alloc_impl(size_bytes, false, false, u64::MAX)
}

/// Allocates a typed block with a precise bitmap of pointer-bearing 8-byte
/// slots. Layouts larger than 64 words use the conservative allocator.
#[no_mangle]
pub extern "C" fn sol_gc_alloc_layout(size_bytes: i64, pointer_mask: u64) -> *mut u8 {
    gc_alloc_impl(size_bytes, false, false, pointer_mask)
}

/// Like `sol_gc_alloc` but guaranteed pointer-free and always zeroed - only
/// for array data buffers (i64/f64 elements can't be pointers, and unwritten
/// elements must read as zero). Skips tracing the contents, which matters for a
/// large array.
pub extern "C" fn sol_gc_alloc_atomic(size_bytes: i64) -> *mut u8 {
    gc_alloc_impl(size_bytes, true, true, 0)
}

/// Write barrier: called whenever a store might create a new `Old -> Young`
/// pointer edge (`AssignField`, non-map `AssignIndex` - see `codegen.rs`'s
/// `emit_write_barrier` and `interp.rs`'s `Op::SetField`/`Op::SetIndex`
/// handlers). `container` is any address inside the block just mutated
/// (exact byte offset doesn't matter - only which chunk it resolves to);
/// `value` is the raw bits just stored, which may not even be a pointer -
/// harmless, since `find_chunk` then simply won't match anything.
///
/// Only ever *sets* an `Old` chunk's `dirty` bit - a safe over-approximation.
/// Only `collect_heap` (a full trace) may ever clear it: a minor collection
/// never re-verifies an `Old` chunk's existing edges, so it can't tell
/// whether a previously-recorded edge is still real; clearing it early could
/// make a live `Young` object invisible to the next minor collection - a
/// real use-after-free.
#[no_mangle]
pub extern "C" fn sol_gc_write_barrier(container: i64, value: i64) {
    if container == 0 || value == 0 {
        return;
    }
    HEAP.with(|heap| {
        let mut heap = heap.borrow_mut();
        let Some(container_idx) = heap.find_chunk(container as usize) else {
            return;
        };
        if heap.chunks[container_idx].generation != Generation::Old {
            return; // Young -> anything is always found by the next minor scan
        }
        if let Some(value_idx) = heap.find_chunk(value as usize) {
            if heap.chunks[value_idx].generation == Generation::Young {
                heap.chunks[container_idx].dirty = true;
            }
        }
    });
}

fn gc_alloc_impl(size_bytes: i64, atomic: bool, needs_zero: bool, pointer_mask: u64) -> *mut u8 {
    let payload_size = round_up((size_bytes.max(0) as usize).max(1), ALIGN);
    let total = HEADER + payload_size;
    HEAP.with(|heap| {
        alloc_in_heap(
            &mut heap.borrow_mut(),
            total,
            payload_size,
            atomic,
            needs_zero,
            pointer_mask,
        )
    })
}

/// New allocations only ever land in a `Young` chunk - an `Old` chunk's
/// leftover space (if any) stays unused until the whole chunk dies, per the
/// module doc comment's chunk-granularity generational design.
fn find_young_chunk_with_room(heap: &Heap, total: usize) -> Option<usize> {
    (0..heap.chunks.len())
        .find(|&i| heap.chunks[i].generation == Generation::Young && heap.chunks[i].has_room(total))
}

fn alloc_in_heap(
    heap: &mut Heap,
    total: usize,
    payload_size: usize,
    atomic: bool,
    needs_zero: bool,
    pointer_mask: u64,
) -> *mut u8 {
    if heap.chunks[heap.current].generation == Generation::Young
        && heap.chunks[heap.current].has_room(total)
    {
        return heap.chunks[heap.current].carve(payload_size, atomic, needs_zero, pointer_mask);
    }

    if let Some(idx) = find_young_chunk_with_room(heap, total) {
        heap.current = idx;
        return heap.chunks[idx].carve(payload_size, atomic, needs_zero, pointer_mask);
    }

    // Always collect before growing - a capacity-threshold gate would never
    // re-arm during pure allocate/reclaim/reuse cycling (this collector's
    // best case), silently disabling collection. Try a cheap minor
    // collection (Young chunks + the Old remembered set) first; only fall
    // back to a full major collection if that doesn't free enough.
    collect_minor(heap);
    if let Some(idx) = find_young_chunk_with_room(heap, total) {
        heap.current = idx;
        return heap.chunks[idx].carve(payload_size, atomic, needs_zero, pointer_mask);
    }

    collect_heap(heap);
    if let Some(idx) = find_young_chunk_with_room(heap, total) {
        heap.current = idx;
        return heap.chunks[idx].carve(payload_size, atomic, needs_zero, pointer_mask);
    }

    let capacity = total.max(CHUNK_SIZE);
    let idx = heap.push_chunk(capacity);
    heap.current = idx;
    heap.chunks[idx].carve(payload_size, atomic, needs_zero, pointer_mask)
}

/// Spills callee-saved registers to a stack buffer so a pointer held only
/// in a register (never spilled) is visible to the scan below - a real bug
/// once corrupted the heap without this (`tests/fixtures/gc_stress.fl`).
/// `#[inline(never)]` ensures this runs as a real call, so the values
/// captured are the caller's own.
#[cfg(target_arch = "aarch64")]
#[inline(never)]
fn flush_callee_saved_registers() -> [usize; 10] {
    let mut buf = [0usize; 10];
    unsafe {
        std::arch::asm!(
            "stp x19, x20, [{0}, #0]",
            "stp x21, x22, [{0}, #16]",
            "stp x23, x24, [{0}, #32]",
            "stp x25, x26, [{0}, #48]",
            "stp x27, x28, [{0}, #64]",
            in(reg) buf.as_mut_ptr(),
        );
    }
    buf
}

#[cfg(target_arch = "x86_64")]
#[inline(never)]
fn flush_callee_saved_registers() -> [usize; 6] {
    let mut buf = [0usize; 6];
    unsafe {
        std::arch::asm!(
            "mov [{0}], rbx",
            "mov [{0} + 8], rbp",
            "mov [{0} + 16], r12",
            "mov [{0} + 24], r13",
            "mov [{0} + 32], r14",
            "mov [{0} + 40], r15",
            in(reg) buf.as_mut_ptr(),
        );
    }
    buf
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(never)]
fn flush_callee_saved_registers() -> [usize; 0] {
    // Other architectures: a register-only pointer could be missed. Not a
    // concern on this project's actual targets (aarch64/x86_64).
    []
}

/// Marks `addr` reachable and pushes it if it's exactly a live,
/// not-yet-visited block start. `only_young`: used by `collect_minor`, which
/// never computes `Old` liveness this cycle - a hit inside an `Old` chunk is
/// ignored entirely (it's already assumed alive, and its `marked`/`any_marked`
/// bits are stale leftovers from the last major collection, not meaningful
/// here).
fn try_mark(heap: &mut Heap, addr: usize, worklist: &mut Vec<usize>, only_young: bool) {
    let Some(chunk_idx) = heap.find_chunk(addr) else {
        return;
    };
    let chunk = &mut heap.chunks[chunk_idx];
    if only_young && chunk.generation != Generation::Young {
        return;
    }
    let rel = addr - chunk.base;
    if !rel.is_multiple_of(ALIGN) {
        return;
    }
    let slot = rel / ALIGN;
    if !get_bit(&chunk.starts, slot) || get_bit(&chunk.marked, slot) {
        return;
    }
    set_bit(&mut chunk.marked, slot);
    chunk.any_marked = true;
    worklist.push(addr);
}

/// Every live (`starts`-bit set), non-atomic block's `(payload_off,
/// size_words)` in `chunk` - used by remembered-set scanning
/// (`collect_minor`) and dirty-flag re-derivation (`collect_heap`), both of
/// which need to read "everything this chunk's blocks currently point at"
/// rather than relying on this cycle's own reachability computation for
/// `chunk` itself. `only_marked`: restrict to blocks this cycle's trace
/// already found reachable (meaningful only right after a full trace, i.e.
/// from `collect_heap`; `collect_minor` passes `false` since an `Old`
/// chunk's `marked` bits aren't maintained during a minor collection at all).
fn nonatomic_blocks(chunk: &Chunk, only_marked: bool) -> Vec<(usize, usize, u64)> {
    let mut out = Vec::new();
    for slot in 0..chunk.starts.len() * 64 {
        if !get_bit(&chunk.starts, slot) || get_bit(&chunk.atomic, slot) {
            continue;
        }
        if only_marked && !get_bit(&chunk.marked, slot) {
            continue;
        }
        let payload_off = slot * ALIGN;
        let size = read_usize(&chunk.data, payload_off - HEADER);
        out.push((payload_off, size / ALIGN, chunk.layouts[slot]));
    }
    out
}

fn pointer_contents(chunk: &Chunk, payload_off: usize, words: usize, mask: u64) -> Vec<usize> {
    (0..words)
        .filter(|index| mask == u64::MAX || (*index < 64 && mask & (1u64 << index) != 0))
        .map(|index| read_usize(&chunk.data, payload_off + index * ALIGN))
        .collect()
}

/// Cheap collection restricted to `Young` chunks plus the `Old` remembered
/// set (every `Old` chunk with `dirty` set) - see the module doc comment.
/// Any `Young` chunk still reachable at the end is promoted to `Old` in
/// place (this collector never moves objects, so "promotion" is just a
/// generation relabel, not a copy).
fn collect_minor(heap: &mut Heap) {
    let flushed = flush_callee_saved_registers();
    let approx_sp = flushed.as_ptr() as usize;
    let base = STACK_BASE.load(Ordering::Relaxed);
    if base == 0 || approx_sp >= base {
        return; // uninitialized - nothing safe to scan
    }

    for chunk in &mut heap.chunks {
        if chunk.generation == Generation::Young {
            clear_bits(&mut chunk.marked);
            chunk.any_marked = false;
        }
    }

    let mut worklist: Vec<usize> = Vec::new();
    let mut addr = approx_sp & !(ALIGN - 1);
    while addr < base {
        // SAFETY: within [flushed buffer, recorded base] on this thread's
        // own stack - mapped, readable memory.
        let word = unsafe { std::ptr::read_unaligned(addr as *const usize) };
        try_mark(heap, word, &mut worklist, true);
        addr += ALIGN;
    }

    EXTRA_ROOTS.with(|roots| {
        for &(ptr, len) in roots.borrow().iter() {
            for i in 0..len {
                // SAFETY: see collect_heap's identical loop.
                let word = unsafe { std::ptr::read_unaligned((ptr + i * ALIGN) as *const usize) };
                try_mark(heap, word, &mut worklist, true);
            }
        }
    });

    // Remembered set: an Old -> Young edge a minor collection could
    // otherwise never discover, since it doesn't retrace Old objects' own
    // reachability. The whole Old generation is always treated as alive
    // during a minor collection, so this scans every live block in a dirty
    // Old chunk unconditionally, not just ones "marked" this cycle.
    let dirty_old: Vec<usize> = (0..heap.chunks.len())
        .filter(|&i| heap.chunks[i].generation == Generation::Old && heap.chunks[i].dirty)
        .collect();
    for chunk_idx in dirty_old {
        for (payload_off, words, mask) in nonatomic_blocks(&heap.chunks[chunk_idx], false) {
            let contents = pointer_contents(&heap.chunks[chunk_idx], payload_off, words, mask);
            for word in contents {
                try_mark(heap, word, &mut worklist, true);
            }
        }
    }

    // Trace: only_young means every address pushed above (and everything
    // reachable from it) is inside a Young chunk, so this only ever walks
    // Young objects' contents - exactly the minor-collection cost bound.
    let mut contents: Vec<usize> = Vec::new();
    while let Some(payload_addr) = worklist.pop() {
        let chunk_idx = heap
            .find_chunk(payload_addr)
            .expect("only validated addresses are ever pushed");
        let chunk = &heap.chunks[chunk_idx];
        let payload_off = payload_addr - chunk.base;
        let slot = payload_off / ALIGN;
        if get_bit(&chunk.atomic, slot) {
            continue;
        }
        let size = read_usize(&chunk.data, payload_off - HEADER);
        let words = size / ALIGN;
        let mask = chunk.layouts[slot];
        contents.clear();
        contents.extend(pointer_contents(chunk, payload_off, words, mask));
        for &word in &contents {
            try_mark(heap, word, &mut worklist, true);
        }
    }

    for chunk in &mut heap.chunks {
        if chunk.generation != Generation::Young {
            continue;
        }
        if !chunk.any_marked && chunk.bump > 0 {
            chunk.reset();
        } else if chunk.any_marked {
            // Promoted in place - conservatively assumed dirty until a
            // major collection re-derives the true remembered set.
            chunk.generation = Generation::Old;
            chunk.dirty = true;
        }
    }
    heap.current = 0;
}

/// Runs a full mark-sweep cycle. Exposed publicly so tests can force one.
pub fn collect() {
    HEAP.with(|heap| collect_heap(&mut heap.borrow_mut()));
}

fn collect_heap(heap: &mut Heap) {
    let flushed = flush_callee_saved_registers();
    // The flushed buffer's own address is the scan's lower bound - it's
    // guaranteed at or below every stack slot we need to see.
    let approx_sp = flushed.as_ptr() as usize;
    let base = STACK_BASE.load(Ordering::Relaxed);
    if base == 0 || approx_sp >= base {
        return; // uninitialized - nothing safe to scan
    }

    for chunk in &mut heap.chunks {
        clear_bits(&mut chunk.marked);
        chunk.any_marked = false;
    }

    let mut worklist: Vec<usize> = Vec::new();
    let mut addr = approx_sp & !(ALIGN - 1);
    while addr < base {
        // SAFETY: within [flushed buffer, recorded base] on this thread's
        // own stack - mapped, readable memory.
        let word = unsafe { std::ptr::read_unaligned(addr as *const usize) };
        try_mark(heap, word, &mut worklist, false);
        addr += ALIGN;
    }

    // Interpreted call frames' register files - see EXTRA_ROOTS.
    EXTRA_ROOTS.with(|roots| {
        for &(ptr, len) in roots.borrow().iter() {
            for i in 0..len {
                // SAFETY: [ptr, ptr + len*8) is a still-live register file,
                // registered by a RootGuard that outlives this collection.
                let word = unsafe { std::ptr::read_unaligned((ptr + i * ALIGN) as *const usize) };
                try_mark(heap, word, &mut worklist, false);
            }
        }
    });

    let mut contents: Vec<usize> = Vec::new();
    let mut words_scanned = 0usize;
    while let Some(payload_addr) = worklist.pop() {
        let chunk_idx = heap
            .find_chunk(payload_addr)
            .expect("only validated addresses are ever pushed");
        let chunk = &heap.chunks[chunk_idx];
        let payload_off = payload_addr - chunk.base;
        let slot = payload_off / ALIGN;
        if get_bit(&chunk.atomic, slot) {
            continue; // pointer-free, nothing to trace inside
        }
        let size = read_usize(&chunk.data, payload_off - HEADER);
        let words = size / ALIGN;
        let mask = chunk.layouts[slot];
        // Copy out first - can't call try_mark (needs &mut heap) while
        // still borrowing chunk.
        contents.clear();
        contents.extend(pointer_contents(chunk, payload_off, words, mask));
        words_scanned += contents.len();
        for &word in &contents {
            try_mark(heap, word, &mut worklist, false);
        }
    }

    // Re-derive precise Old -> Young remembered-set membership. This is the
    // only point a dirty bit may ever be safely cleared: this full trace
    // already knows exactly which chunk (Young survivors included) is
    // reachable, so re-scanning each live Old chunk's own contents here
    // tells us definitively whether it still holds a live Old -> Young edge
    // - see sol_gc_write_barrier's comment for why a minor collection can't
    // do this itself.
    for chunk_idx in 0..heap.chunks.len() {
        if heap.chunks[chunk_idx].generation != Generation::Old
            || !heap.chunks[chunk_idx].any_marked
        {
            continue;
        }
        let mut points_at_young = false;
        'blocks: for (payload_off, words, mask) in nonatomic_blocks(&heap.chunks[chunk_idx], true) {
            let contents = pointer_contents(&heap.chunks[chunk_idx], payload_off, words, mask);
            for word in contents {
                if let Some(target_idx) = heap.find_chunk(word) {
                    if heap.chunks[target_idx].generation == Generation::Young {
                        let rel = word - heap.chunks[target_idx].base;
                        if rel.is_multiple_of(ALIGN)
                            && get_bit(&heap.chunks[target_idx].starts, rel / ALIGN)
                        {
                            points_at_young = true;
                            break 'blocks;
                        }
                    }
                }
            }
        }
        heap.chunks[chunk_idx].dirty = points_at_young;
    }

    let mut chunks_reclaimed = 0usize;
    for chunk in &mut heap.chunks {
        if !chunk.any_marked && chunk.bump > 0 {
            chunk.reset();
            chunks_reclaimed += 1;
        } else if chunk.any_marked && chunk.generation == Generation::Young {
            // Survived a full collection while still Young - promote in
            // place (same rule as collect_minor's own sweep).
            chunk.generation = Generation::Old;
            chunk.dirty = true;
        }
    }
    heap.current = 0; // re-search next time rather than trust a stale hint

    if std::env::var_os("SOL_GC_DEBUG")
        .or_else(|| std::env::var_os("SOL_GC_DEBUG"))
        .is_some()
    {
        eprintln!(
            "[gc] collect: chunks={} reclaimed={} total_capacity={} words_scanned={} approx_sp={:#x} base={:#x}",
            heap.chunks.len(),
            chunks_reclaimed,
            heap.total_capacity,
            words_scanned,
            approx_sp,
            base
        );
    }
}

/// Payload bytes currently carved out (over-approximates true live bytes,
/// since a partially-dead chunk isn't reclaimed until fully dead).
pub fn live_bytes() -> usize {
    HEAP.with(|heap| {
        let heap = heap.borrow();
        heap.chunks
            .iter()
            .map(|c| c.bump - HEADER * count_set_bits(&c.starts))
            .sum()
    })
}

/// Number of allocation records currently tracked (same caveat as `live_bytes`).
pub fn live_blocks() -> usize {
    HEAP.with(|heap| {
        heap.borrow()
            .chunks
            .iter()
            .map(|c| count_set_bits(&c.starts))
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run with no JIT'd caller on the stack, so STACK_BASE stays
    // uninitialized and collect() is a no-op - they only check
    // allocation/registration bookkeeping. Real reclaim-with-roots
    // behavior is covered by tests/programs.rs's GC fixtures.

    #[test]
    fn allocating_registers_a_block_of_the_requested_size() {
        // Checks bookkeeping only, not zeroing (sol_gc_alloc doesn't
        // guarantee it).
        let before = live_bytes();
        let ptr = sol_gc_alloc(64);
        assert!(!ptr.is_null());
        assert_eq!(live_bytes(), before + 64);
    }

    #[test]
    fn allocating_atomic_memory_is_always_zeroed() {
        // Zero-across-reuse is covered separately by tests/programs.rs's
        // array_data_reads_as_zero_even_after_many_collections_reuse_the_chunk.
        let ptr = sol_gc_alloc_atomic(64);
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, 64) };
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn precise_layout_scans_only_declared_pointer_slots() {
        let mut chunk = Chunk::new(128);
        let ptr = chunk.carve(24, false, false, 0b010);
        unsafe {
            *(ptr as *mut usize) = 11;
            *((ptr as *mut usize).add(1)) = 22;
            *((ptr as *mut usize).add(2)) = 33;
        }
        let payload_off = ptr as usize - chunk.base;
        assert_eq!(pointer_contents(&chunk, payload_off, 3, 0b010), vec![22]);
        assert_eq!(
            pointer_contents(&chunk, payload_off, 3, u64::MAX),
            vec![11, 22, 33]
        );
    }

    #[test]
    fn collect_without_an_initialized_stack_base_is_a_safe_no_op() {
        let ptr = sol_gc_alloc(32);
        let before = live_bytes();
        collect(); // STACK_BASE is 0 in this test process - must not free anything
        assert_eq!(live_bytes(), before);
        assert!(!ptr.is_null());
    }

    #[test]
    fn each_allocation_registers_exactly_one_more_live_block() {
        let before = live_blocks();
        sol_gc_alloc(8);
        sol_gc_alloc(16);
        sol_gc_alloc(24);
        assert_eq!(live_blocks(), before + 3);
    }

    #[test]
    fn flush_callee_saved_registers_returns_a_stack_resident_buffer() {
        // Only checks the buffer is really stack memory, not register values.
        let flushed = flush_callee_saved_registers();
        let local = 0u8;
        let flushed_addr = flushed.as_ptr() as usize;
        let local_addr = &local as *const u8 as usize;
        assert!(flushed_addr.abs_diff(local_addr) < 1 << 20, "flushed buffer address {flushed_addr:#x} is nowhere near this frame's own stack address {local_addr:#x}");
    }

    #[test]
    fn allocating_past_a_chunk_boundary_grows_the_arena_and_keeps_working() {
        // Forces multiple chunks - checks growth doesn't corrupt bookkeeping.
        let before = live_blocks();
        for _ in 0..(CHUNK_SIZE / 64 + 10) {
            let ptr = sol_gc_alloc(48);
            assert!(!ptr.is_null());
        }
        assert_eq!(live_blocks(), before + (CHUNK_SIZE / 64 + 10));
    }
}
