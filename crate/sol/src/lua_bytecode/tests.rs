use super::Instr;

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
