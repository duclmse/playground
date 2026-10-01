//! Per-`Instr` Cranelift IR lowering for the dynamic JIT.
//!
//! Empty in Work item 1 (scaffolding only - see `mod.rs`'s `DynJit::promote`,
//! which always fails rather than calling into this module). Work item 3
//! adds the first real lowering pass here, restricted to call-free,
//! capture-free `Proto` bodies; items 4-5 extend it to field/global/upvalue
//! access and the call/return protocol. See the plan's own "Work items"
//! section for the exact per-item instruction-set scope.
