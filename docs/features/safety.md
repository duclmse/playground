# Safety and correctness

> Status: implemented, with any remaining limitations called out below.

**Purpose**: close safety and correctness gaps discovered in the initial typed
compiler,
before building more on top of a foundation with holes in it.

**Prerequisite**: the typed compiler pipeline.

- [x] **Array bounds checking.** Added in `codegen.rs`'s `array_elem_addr`: an
      unsigned `index >= len` compare (catches negative indices too, since they
      wrap to a huge unsigned value) followed by
      `trapnz(_, TrapCode::HEAP_OUT_OF_BOUNDS)`. Verified: out-of-bounds and
      negative-index accesses now crash (a real trap, exit code ≠ 0 and ≠ the
      normal `exit(1)` compile-error path) instead of reading/ writing past the
      allocation - see `tests/programs.rs`'s
      `out_of_bounds_array_access_traps_instead_of_reading_garbage` and
      `negative_array_index_traps`.
- [x] **Negative/zero-length arrays.** Zero-length arrays remain valid and own
      a one-byte atomic backing allocation; negative lengths and lengths whose
      eight-byte element storage calculation overflows now abort consistently
      through bytecode, native promotion, and AOT instead of being silently
      clamped to zero.
- [x] **Division/modulo by zero.** Confirmed empirically (not just assumed):
      Cranelift's `sdiv`/`srem` already trap on a zero divisor on this target -
      no extra check needed in `codegen.rs`. Documented in `docs/sol.md`'s
      language reference and pinned by `tests/programs.rs`'s
      `integer_division_by_zero_traps`.
- [x] **Integer overflow.** Documented in `docs/sol.md`'s language
      reference: `iadd`/`isub`/`imul` wrap silently, matching typical
      systems-language behavior - a stated design choice now, not an accidental
      gap.
- [x] Regression tests added to `crates/sol/tests/programs.rs` for bounds
      checking, negative indices, and division by zero.

**Files**: `crates/sol/src/codegen.rs`, `runtime.rs`, `tests/programs.rs`,
`docs/sol.md`.
