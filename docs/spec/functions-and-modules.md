# Functions and modules

## Function declarations

A typed function is declared as:

```sol
function add(a: i64, b: i64): i64
    return a + b
end
```

`fn` may replace `function` in `.sol`. Every typed function must declare its
return type. The program entry point is a parameterless `main`; `sol run`
prints its returned value.

`extern function name(parameters): Return` declares a symbol resolved from the
process. The symbol name is the Sol declaration name. Only `i64`, `f64`, and
`bool` may cross the current native FFI boundary.

## Function values and captures

Top-level functions and stateless nested functions may be stored in a matching
function-typed local and called indirectly. A nested function may directly
capture supported scalar/string locals when it is used only in direct calls.
The compiler passes the current captured values as hidden unboxed parameters,
so reassignment in the enclosing function is observed at the next call.

Assigning to a captured local from inside a typed closure and allowing a
capturing typed closure to escape are unsupported. Programs requiring shared
mutable upvalues must use the Lua compatibility mode until typed heap closure
environments are specified.

## Modules

`import path.to.module` resolves relative to the importing file. Resolution
tries `.sol` before `.lua`. `export` makes a typed function, struct, or alias
part of a `.sol` module interface. Imported declarations are referenced with
qualified names.

The compiler builds a canonical dependency graph, rejects missing modules,
private access, duplicate exports, indirect undeclared imports, and import
cycles, then runs typed module initializers once in dependency order before the
root `main`.

`sol run` may import a dynamic `.lua` module from typed `.sol` when the called
function declares a result type and explicitly annotates every parameter. That
signature is a checked contract, not an inference from the dynamic body. The
current mixed boundary accepts only `i64`, `f64`, and `bool`; a mismatching
argument or result is a runtime boundary error.

The canonical import graph loads a source path once. A dynamic module namespace
loaded by `import` is also installed in `package.loaded`, so a later
`require("name")` returns that same table and the same exported callable
identities rather than executing a second module instance. Unannotated dynamic
functions remain private to typed import. `sol build` and `sol debug` do not yet
support dynamic module bodies.
