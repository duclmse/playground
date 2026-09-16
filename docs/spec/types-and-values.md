# Types and values

## Static types

Typed Sol supports `i64`, `f64`, `bool`, `string`, `any`, `Array<T>`, nominal
`struct` types, structural record types, supported `Map<K, V>`
specializations, and function types. Type aliases name another type and do not
create a distinct runtime representation.

A function type is written `fn(T1, T2) -> R` or `fn(T1, T2): R`; the forms are
equivalent. Qualified type names refer to exported definitions in an imported
module.

The implemented map family is `Map<i64, V>` where `V` is `i64`, `f64`, or
`bool`. Other key/value specializations are unsupported until their equality,
hashing, and GC layout rules are specified.

User-declared generic functions, arbitrary union types, `Option<T>`, tagged
enums, and exhaustive `match` are currently unsupported.

The optimizer may use internal unions of up to four runtime type atoms while
analyzing annotation-free code. These unions are flow facts, not source-level
types: they widen to `dynamic` when precision would exceed the bound and never
cause an otherwise-valid Lua program to be rejected.

## Inference and conversion

The initializer in `local name = expression` determines the local's static
type. An explicit annotation in `local name: T = expression` constrains it.
`i64` widens implicitly to `f64` where `f64` is required. No other numeric or
unrelated-type conversion is implicit.

An omitted function-parameter annotation is distinguishable from an explicit
`: any` contract. A nonescaping local function may infer the former from all
of its local call sites. Explicit `any` remains dynamic under every policy.

## Arrays, records, and structs

`new_array_i64(n)` and `new_array_f64(n)` construct arrays of the named element
type. Their length is fixed. Indexing is zero-based and bounds checked.

A nominal struct is declared with `struct Name { field: Type, ... }` and
constructed with `Name { field = value, ... }`. Every declared field must
appear exactly once. Structural records use a record type annotation such as
`{ x: f64, y: f64 }` and a compatible record literal. Field access is statically
resolved.

An empty table-shaped literal in `.sol` requires an expected record or map type;
it does not silently create a dynamic Lua table.

## `any` and narrowing

`any` is an explicit tagged boundary. It can represent the implemented scalar
and reference families, including nil, booleans, numbers, strings, arrays,
records, maps, and stateless function values. Boxing a reference must preserve
its identity.

`value is Type` tests the runtime type. In `value is Type and expression`, a
directly tested local is narrowed to `Type` in the right operand and in the
true branch. `value as Type` performs a checked cast and traps with a type error
when the runtime value does not match. Else-edge, negated, and disjunctive
narrowing are unsupported.

A Lua dynamic table or capturing Lua closure is not interchangeable with a
typed aggregate or typed function merely because both can cross an `any`
boundary; conversion must be explicit and checked.
