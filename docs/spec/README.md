# Sol language specification

This directory defines the observable behavior of Sol source programs. The
specification covers two source modes selected by file extension:

- `.sol` is the statically typed language. Its compiler preserves unboxed,
  specialized representations on typed paths.
- `.lua` is the Lua-compatibility language. It uses a separate dynamic value
  model and interpreter. Compatibility status is documented in
  [Lua compatibility](lua-compatibility.md).

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

## Compatibility boundary

Typed and dynamic representations are distinct. A `.lua` table is not a typed
record, array, or `Map<K, V>`. A dynamic value may enter `.sol` only through an
explicit `any` boundary and must be narrowed or checked before an operation
requiring a static type. Implementations must not add hidden Lua boxing or
dynamic dispatch to a fully typed `.sol` path.
