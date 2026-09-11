# Statements and expressions

## Declarations and blocks

Typed Sol accepts top-level function, extern function, struct, type-alias,
import, and exported declarations. Local declarations have the form
`local name [: Type] = expression`. `do ... end` and `{ ... }` statement blocks
create lexical scopes.

Assignment is permitted only when the target and value types are compatible,
subject to `i64`-to-`f64` widening. Struct/record fields, array elements, and
supported map entries are assignable targets.

## Control flow

`if`/`elseif`/`else`, `while`, numeric `for`, generic `for`, and `return` use
Lua-style block terminators. Conditions in typed Sol must be `bool` unless an
explicit dynamic truth operation is being performed on `any`.

Numeric `for` loops include both endpoints according to the loop step. Generic
iteration currently supports allocation-free `ipairs(Array<T>)` and
`pairs(Map<i64, V>)` lowering. Map traversal order is implementation-defined;
programs must not depend on it.

## Operators

Arithmetic and comparison operands must have compatible types. `%` is defined
for integers only. Integer addition, subtraction, and multiplication wrap on
overflow. Integer division or remainder by zero traps. `and` and `or`
short-circuit; the right operand is evaluated only when required. `not` is
logical negation and `#` obtains the supported sequence/string length.

Array reads and writes must trap when the index is negative or greater than or
equal to the array length. A negative or unrepresentable array allocation
length must trap rather than wrapping an allocation size.

## Calls and field access

A direct or typed indirect call must match its function signature. Field access
on a statically known struct or record resolves to its declared field. Dynamic
indexing, calling, or field access through `any` requires a supported dynamic
operation or prior narrowing; it must never be treated as unchecked typed
memory access.

Lua mode has expression-list adjustment, multiple results, table constructors,
method-call syntax, labels/goto, and Lua truthiness. Those rules apply only to
the supported compatibility surface described in
[Lua compatibility](lua-compatibility.md).
