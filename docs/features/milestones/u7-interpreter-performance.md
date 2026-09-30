# U7 — Interpreter performance foundation

**Status:** planned

**Purpose:** make the canonical semantic interpreter efficient before dynamic
native tiers.

- [ ] Select and measure a packed/tagged `Value` representation.
- [ ] Make common frame/register operations allocation- and clone-free.
- [ ] Make common call, return, vararg, and iterator paths allocation-free.
- [ ] Optimize table array/hash layout, string interning/hashing, and shapes.
- [ ] Adopt direct-threaded dispatch only if it is maintainable and measured.
- [ ] Add generational allocation fast paths and measured barriers.
- [ ] Optimize metamethod-negative paths and tail-frame reuse.

**Exit gate:** interpreter-readiness performance gate passes with compatibility,
GC stress, debugger, and WASM tests enabled.

This owns the superlinear large-live-heap stress behavior currently blocking
`constructs.lua`. See the [historical U7 ledger](../unified-sol-runtime-plan.md#u7--interpreter-performance-foundation).
