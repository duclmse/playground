# Unified runtime milestones

These are the maintained milestone specifications for Sol's unified runtime
roadmap. The shared goal, architecture, compatibility profiles, dependency
graph, migration strategy, and final definition of done remain in the
[unified runtime plan](../unified-sol-runtime-plan.md).

The plan's former in-file milestone sections remain as a historical
implementation ledger while cross-references are migrated. Update milestone
scope, checklists, status, and exit gates here; add detailed implementation
findings to the relevant feature document and corpus manifest.

| Milestone | Status | Specification |
| --- | --- | --- |
| U0 | Complete | [Charter, baselines, and decisions](u0-charter-baselines.md) |
| U1 | Complete | [Superset frontend and semantic AST](u1-frontend-semantic-ast.md) |
| U2 | Complete | [Canonical object model and GC](u2-canonical-runtime-gc.md) |
| U3 | Complete | [Bytecode, frames, and call ABI](u3-bytecode-frames-call-abi.md) |
| U4 | Complete with a scoped follow-up | [Gradual types and inference](u4-gradual-types-inference.md) |
| U5 | Complete | [Typed layouts and specialization](u5-typed-layouts.md) |
| U6 | In progress | [Lua 5.5 compatibility](u6-lua55-compatibility.md) |
| U7 | Planned | [Interpreter performance foundation](u7-interpreter-performance.md) |
| U8 | Planned | [Inline caches and profiling](u8-inline-caches-profiling.md) |
| U9 | Planned | [Baseline dynamic JIT](u9-baseline-jit.md) |
| U10 | Planned | [Optimizing JIT, OSR, and deoptimization](u10-optimizing-jit-osr.md) |
| U11 | Planned | [AOT and annotation-driven performance](u11-aot-typed-performance.md) |
| U12 | Planned | [Canonical WASM playground](u12-wasm-playground.md) |
| U13 | Planned | [Semantic LSP and VS Code client](u13-lsp-vscode.md) |
| U14 | Planned | [Integrated release qualification](u14-release-qualification.md) |

An unchecked item is planned, `[~]` is partially implemented with the stated
remaining work, and `[x]` is implemented and verified at that milestone's
scope. A checkmark is never a replacement for an unchanged pinned-oracle
comparison where the compatibility profile requires one.
