use super::{BoundedCache, Instr, IC_SLOTS};

#[test]
fn generic_calls_project_to_the_semantic_call_abi() {
    let instruction = Instr::Call(
        5,
        sol_core::ValueCount::Open,
        sol_core::ValueCount::Fixed(2),
    );
    let call = instruction.call_site().unwrap();
    assert_eq!(call.base, 5);
    assert_eq!(call.arguments, sol_core::ValueCount::Open);
    assert_eq!(call.results, sol_core::ValueCount::Fixed(2));
    assert_eq!(call.kind, sol_core::CallKind::Normal);
    let tail = Instr::TailCall(9, sol_core::ValueCount::Fixed(3))
        .call_site()
        .unwrap();
    assert_eq!(tail.base, 9);
    assert_eq!(tail.results, sol_core::ValueCount::Open);
    assert_eq!(tail.kind, sol_core::CallKind::Tail);
}

/// Pins `Instr`'s in-memory size so a future change doesn't silently
/// reintroduce a fat inline payload (like the `Rc<str>` `GetField`/
/// `SetField` used to carry) into every instruction in the stream,
/// which would cost dispatch-loop cache density across the whole
/// program, not just the instructions that use the fat variant.
#[test]
fn instr_size_regression() {
    let size = std::mem::size_of::<Instr>();
    assert!(
        size <= 24,
        "Instr grew to {size} bytes; keep dispatch-loop entries compact"
    );
}

/// `BoundedCache` (U8's shared inline-cache infrastructure) must stay at
/// exactly `IC_SLOTS` entries once full, evicting the oldest first - this is
/// the sole mechanism bounding a call site's mono -> poly -> megamorphic
/// growth, so a regression here would let a megamorphic call site grow
/// cache memory without bound.
#[test]
fn bounded_cache_evicts_oldest_once_full() {
    let cache: BoundedCache<u32> = BoundedCache::new();
    assert!(cache.is_empty());
    for entry in 0..IC_SLOTS as u32 {
        cache.insert(entry);
    }
    assert_eq!(cache.len(), IC_SLOTS);
    for entry in 0..IC_SLOTS as u32 {
        assert_eq!(cache.find(|&e| e == entry), Some(entry));
    }

    // One more insert beyond capacity evicts entry 0 (the oldest) and stays
    // bounded at IC_SLOTS, never growing further.
    cache.insert(IC_SLOTS as u32);
    assert_eq!(cache.len(), IC_SLOTS);
    assert_eq!(cache.find(|&e| e == 0), None, "oldest entry must be evicted");
    for entry in 1..=IC_SLOTS as u32 {
        assert_eq!(cache.find(|&e| e == entry), Some(entry));
    }

    // Megamorphic stress: many more distinct entries than IC_SLOTS never
    // grows the cache past IC_SLOTS.
    for entry in 0..1000u32 {
        cache.insert(entry);
        assert!(cache.len() <= IC_SLOTS);
    }
}
