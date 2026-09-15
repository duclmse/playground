# Sol language specification

This directory defines the observable behavior of currently implemented Sol
source programs. Sol's final language goal is one Lua 5.5-compatible superset:
types are optional contracts and optimizations, and typed/untyped programs use
one semantic runtime. The convergence contract and milestones are in the
[unified runtime plan](../features/unified-sol-runtime-plan.md).

The implementation is still transitional and currently exposes two source
modes selected by file extension:

- `.sol` currently selects the statically typed frontend. Its compiler preserves unboxed,
  specialized representations on typed paths.
- `.lua` currently selects the Lua-compatibility frontend and a separate dynamic value
  model and interpreter. Compatibility status is documented in
  [Lua compatibility](lua-compatibility.md).

That split describes shipped behavior, not the desired product boundary. Until
milestones U1-U3 land, specification sections must clearly distinguish current
behavior from the accepted future direction rather than pretending convergence
is already implemented.

Implementation rationale and work-in-progress checklists live in
[`docs/features/`](../features/README.md). If a feature document and this
specification disagree about currently supported behavior, the executable
tests identify the implementation truth and the specification must be fixed.

## Conformance language

The words **must**, **must not**, **should**, and **may** are normative. A
section marked **unsupported** describes syntax or behavior that programs must
not rely on. A section marked **implementation-defined** permits the
implementation to choose a behavior, which must remain documented.

## Contents

1. [Source text and lexical grammar](source-and-lexical-grammar.md)
2. [Types and values](types-and-values.md)
3. [Statements and expressions](statements-and-expressions.md)
4. [Functions and modules](functions-and-modules.md)
5. [Execution and runtime behavior](execution-and-runtime.md)
6. [Lua compatibility](lua-compatibility.md)

## Current compatibility boundary

Typed and dynamic representations are distinct. A `.lua` table is not a typed
record, array, or `Map<K, V>`. A dynamic value may enter `.sol` only through an
explicit `any` boundary and must be narrowed or checked before an operation
requiring a static type. Implementations must not add hidden Lua boxing or
dynamic dispatch to a fully typed `.sol` path.

The target architecture keeps those optimized representations but gives them a
shared heap, object identity, module graph, and semantic call ABI. After each
convergence milestone lands, the detailed specification and tier-agreement
tests must be updated together.
