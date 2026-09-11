# Sol feature documentation

This directory records how Sol's compiler, runtime, and tooling features are
implemented. These documents contain design rationale, implementation status,
verification notes, and remaining work. They are not the normative language
contract; observable language behavior is specified in [the language
specification](../spec/README.md).

## Feature index

| Area | Status | Document |
| --- | --- | --- |
| Safety and correctness | Implemented | [Safety and correctness](safety.md) |
| Typed compiler pipeline | Implemented | [Compiler pipeline](compiler-pipeline.md) |
| Optimization | Implemented, with scoped follow-ups | [Optimization](optimization.md) |
| Records and escape analysis | Implemented, with scoped follow-ups | [Records and escape analysis](records-and-escape-analysis.md) |
| Memory management | Conservative collector implemented; precise/generational work open | [Memory management](memory-management.md) |
| Gradual typing | Implemented for the documented value families | [Gradual typing](gradual-typing.md) |
| Tiered execution | Implemented with one native promotion tier | [Tiered execution](tiered-execution.md) |
| Native toolchain | Implemented, platform scope documented | [Native toolchain](native-toolchain.md) |
| Debugging and profiling | Implemented at call boundaries | [Debugging and profiling](debugging-and-profiling.md) |
| Language/runtime expansion | Partially implemented | [Feature delivery plan](delivery-plan.md) |
| Lua compatibility | Interpreter-first subset implemented | [Lua compatibility](lua-compatibility.md) |

## Documentation rules

- Update the applicable feature document when implementation scope, design, or
  verification changes.
- Update `docs/spec/` when observable `.sol` or `.lua` behavior changes.
- Update `docs/conformance.md` and the Lua corpus manifest when compatibility
  status changes.
- Update `benchmarks/RESULTS.md` when a change affects a measured workload.
- A checked item means the stated scope is implemented and tested. A partial
  item (`[~]`) must say what remains. An unchecked item is planned, not part of
  the current language contract.

Git history preserves the former phase-based roadmap. Feature names, rather
than delivery phase numbers, are now the stable identifiers for documentation
and cross-references.
