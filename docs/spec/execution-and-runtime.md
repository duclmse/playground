# Execution and runtime behavior

## Execution tiers

Typed `.sol` has bytecode, JIT, on-stack-replacement, and AOT execution paths.
For supported features, all paths must produce the same observable result or
the same class of runtime failure. Optimization must not change language
semantics.

The interpreter starts execution immediately and may promote hot typed
functions or loops to native code. A failed speculative `any` parameter guard
falls back to compatible generic execution. Debugging and profiling hooks are
absent from the ordinary uninstrumented monomorphization.

Dynamic `.lua` code executes in its separate compatibility interpreter. It is
not AOT-native by default.

## Memory management

Heap arrays, escaping records/structs, strings, and supported boxed values are
managed by the Sol runtime. The current collector is conservative mark/sweep
over a chunked bump arena. Collection timing and traversal order are
implementation-defined and must not be observable in typed programs except
through resource usage or a fatal allocation failure.

Pointer-bearing map specializations and escaping typed closure environments are
unsupported until their tracing and write-barrier behavior is implemented.

## Errors and traps

Static errors prevent compilation and should include a stable diagnostic code
and source span. Typed runtime contract violations such as array bounds errors,
invalid checked casts, division by zero, and invalid allocation sizes trap;
they are not catchable as Lua errors.

Lua compatibility errors are host-independent dynamic error values. Supported
`pcall`/`xpcall` operations can catch them. They must not surface as Rust panics
or unchecked typed-memory operations.

## Tooling contracts

`sol run` executes `.sol` or `.lua`; `sol build` emits a standalone executable
for supported typed input; `sol debug` provides the documented call-boundary
debugger. `--diagnostic-format json` emits structured diagnostics. Profiling,
IR, assembly, target, and tier logs are diagnostic interfaces and may evolve;
scripts should not treat their prose formatting as a language guarantee.
