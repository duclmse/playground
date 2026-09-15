# ADR 0006: performance measurement and public claim gates

- Status: accepted
- Date: 2026-09-15

## Context

Typed microbenchmark wins previously allowed “Sol beats LuaJIT” to be read more
broadly than the dynamic runtime evidence supported. The final goal requires an
honest unchanged-Lua comparison.

## Decision

Performance reports separate untyped compatibility, progressively annotated,
and typed/data-oriented suites. Untyped comparisons use identical source,
inputs, and observable results. Reports pin builds/hardware and retain raw
samples for cold latency, warm throughput, compilation, memory, GC, and tail
latency on x86-64 and AArch64.

Progressive gates are:

| Gate | Requirement |
| --- | --- |
| Interpreter readiness | Beat pinned reference Lua geometric mean and stop losing to the Piccolo baseline in any core category |
| Baseline-JIT readiness | Reach at least 0.75x LuaJIT geometric-mean throughput with bounded compile cost |
| Dynamic parity | Reach LuaJIT geometric-mean throughput; no application case more than 20% slower |
| Final claim | Reach at least 1.10x LuaJIT geometric-mean throughput on the published untyped application suite; no category geometric mean below parity |

All timed executions pass compatibility first. There are no semantic waivers in
a performance gate. Typed results are additional evidence and never substitute
for the unchanged-Lua suite.

## Consequences

- Sol uses no unqualified “faster than LuaJIT” claim before the final gate.
- Benchmark summaries name the exact runtime/tier and cannot label typed ports
  as unchanged Lua.
- Regressions in startup, memory, or GC remain visible even when throughput
  improves.
- Failed gates are published as current status rather than hidden or narrowed
  to another engine after the fact.
