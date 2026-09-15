# Architecture decisions

These accepted records define convergence milestone U0. Current implementation
may still differ; each record states the migration consequence and links to the
authoritative [unified runtime plan](../features/unified-sol-runtime-plan.md).

| ADR                                         | Decision                                                    |
| ------------------------------------------- | ----------------------------------------------------------- |
| [0001](0001-unified-product-and-runtime.md) | One Sol product and semantic runtime                        |
| [0002](0002-lua-compatibility-profile.md)   | Lua 5.5.1 target and native embedding/C API scope           |
| [0003](0003-superset-syntax-and-types.md)   | Contextual superset syntax and optional type policy         |
| [0004](0004-runtime-boundary-and-abi.md)    | Portable runtime crate and semantic call ABI                |
| [0005](0005-tiered-jit-strategy.md)         | Interpreter, baseline JIT, optimizing JIT, and AOT strategy |
| [0006](0006-performance-and-claim-gates.md) | Reproducible LuaJIT comparison and claim gates              |

An ADR can be superseded only by another checked-in ADR that explains the new
evidence. Updating current implementation documentation does not silently change
these decisions.
