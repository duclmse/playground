# Records and escape analysis

> Status: structs and scalar replacement are implemented. Managed array
> allocation is covered by [Memory management](memory-management.md).

**Purpose**: implement fixed-layout structs (§9) and escape analysis / scalar replacement
(§12) so non-escaping allocations - a struct or array that never leaves its
function - skip heap allocation entirely.

**Prerequisite**: escape analysis benefits from CSE/DCE being available to
clean up after scalar replacement.

- [x] **Struct syntax**: `struct Name { field: Type, ... }` (top-level,
      alongside functions); struct literals `Name { field = expr, ... }` (named,
      any order - `typeck.rs` reorders to declaration order); field access
      `value.field` (read via `ExprKind::Field`, write via
      `AssignTarget::Field`, both added as a third case alongside the existing
      `Index`/plain-name ones).
- [x] **Struct type-checking**: struct names collected up front (so
      mutually-referencing structs work - see `typeck.rs`'s `check()` doc
      comment on why cycles aren't actually a size problem here), then each
      struct's field types resolved; field-literal completeness (every field
      required, with no defaults) and unknown-field/non-struct-field-access
      errors covered by `typeck.rs`'s test module.
- [x] **Struct codegen**: no header at all (simpler than the original plan) - a
      struct is exactly `fields.len() * 8` raw bytes from `runtime.rs`'s new
      `sol_alloc`, since every field's type/offset is already fully resolved
      by `typeck.rs` (`field_index`) and `codegen.rs` never needs to look
      anything up by name or struct identity at codegen time.
- [x] **Escape analysis** (§12): `escape.rs`'s `expr_leaks_local` - deliberately
      narrow like the bounds-check pattern (this project's established
      style): eligible locals are exactly `local p = Struct {     ... }`
      literals, never reassigned as a whole (`is_reassigned_as_whole`), and
      never used as a whole value anywhere except as the direct base of a
      `.field` access (a function-call argument, a `return`, an array element,
      or another struct's field all count as escaping).
- [x] **Scalar replacement of aggregates** (§12, §34 - "Huge" impact):
      `escape.rs`'s `scalar_replace` - an eligible struct's declaration becomes
      N plain `Local` statements (one per field) and every
      `Field{base: Local(id), ..}` becomes a direct `Local` reference; no
      pointer, no allocation, no load/store survives for it at all. **Verified
      at the IR level, not just behaviorally** (this checklist's own original
      ask): `tests/programs.rs`'s
      `non_escaping_struct_local_has_no_allocation_in_the_emitted_ir` uses a
      `SOL_DUMP_CLIF` debug env var (`jit.rs`) to confirm the emitted
      Cranelift IR for a non-escaping `Point` is bare
      `f64const`/`fmul`/`fadd`/`return` - zero calls, zero stores - while the
      escaping case in `structs.sol` still shows a real `call fn0(...)` to the
      allocator. Bonus finding from the same dump: inlining and escape
      analysis compose correctly - `structs.sol`'s `dist_squared` gets inlined
      into `main`, and the escape analysis (which runs before inlining, at the
      AST level) still correctly kept `p` heap-allocated, since escaping is
      about _how a value's own declaring function used it_, not about whether
      the callee later happened to get inlined away.
- [x] Replace the initial "leaked `Vec` via a runtime call" arrays with a real
      managed array runtime. Scalar-replacing _arrays_ (as opposed to structs)
      was considered and deliberately not attempted here: unlike a struct's fixed, named fields,
      array elements are almost always accessed with a variable index (a loop),
      which scalar replacement fundamentally can't help with (only a small,
      all-constant-index pattern could benefit, judged too narrow a win for the
      effort here).
- [x] Verification via IR inspection - see the "Scalar replacement" item above.
      A dedicated struct-heavy wall-clock benchmark in `benchmarks/` was _not_
      added as part of the broader benchmark suite once language features
      land" item, since the IR-level proof is the more direct, more convincing
      evidence for this specific optimization (a benchmark's wall-clock delta
      would just be "however long a single `sol_alloc` call costs vs. not
      calling it," which the IR diff already shows unambiguously without needing
      hyperfine).

**Files**: `ast.rs`, `types.rs`, `typeck.rs`, `codegen.rs`, `runtime.rs` (struct
support - no separate header needed); new `crates/sol/src/escape.rs` (both
escape analysis and the scalar- replacement transform - combined into one file
rather than the originally sketched `escape.rs` + `scalar_replace.rs` split,
since they're tightly coupled and the combined file is still under 250 lines);
`jit.rs`'s `SOL_DUMP_CLIF` debug hook.
