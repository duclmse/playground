//! Runtime entry points `DynJit`-compiled code calls out to for anything
//! that isn't worth (or isn't safe to) inline as raw Cranelift IR - slow
//! paths, allocation, field/global access, and (from Work item 5 onward)
//! the park-before-call protocol itself.
//!
//! Registered once, in `DynJit::new`, via `JITBuilder::symbol` - mirrors
//! `crate::jit::Jit::new`'s own symbol-registration block
//! (`crate::jit`'s module, `with_flags`/`symbol` calls), but against a
//! disjoint symbol namespace: these stubs are keyed to `LuaRuntime`/
//! `sol_core::Heap`, not the typed tier's own object model, so the two
//! `JITModule` instances never need to share or disambiguate names.
//!
//! No stub does real work yet (Work item 1 scope: scaffolding only, no
//! codegen references any of this). `register` is still called eagerly from
//! `DynJit::new` so the symbol-registration shape doesn't need to change
//! again once real stubs land in items 3-5.

use cranelift_jit::JITBuilder;

/// Registers every stub symbol `lower.rs` is allowed to reference by name.
/// A no-op today; each later work item adds its own `builder.symbol(...)`
/// call here alongside the stub function it names.
pub fn register(_builder: &mut JITBuilder) {}
