# ADR 0003: contextual superset syntax and optional types

- Status: accepted
- Date: 2026-09-15

## Context

The current extension-selected grammars make typed `.sol` a different language
from `.lua`. Unconditional new keywords can also reject otherwise valid Lua
programs.

## Decision

`.lua` accepts standard Lua 5.5 syntax. `.sol` accepts that same syntax plus Sol
extensions. Sol-only declaration words and annotations are contextual wherever
reserving a Lua identifier would break the superset rule.

Renaming annotation-free `.lua` to `.sol` preserves AST meaning and execution.
Extensions, type-check policy, host capabilities, and execution tier are
independent configuration dimensions rather than one `SourceMode` switch.

Missing annotations mean dynamic Lua. Static inference may prove types without
turning uncertainty into an error. Annotations are checked contracts; a dynamic
caller that violates one receives a catchable Lua error. `off`, `infer`, and
`strict` policies control diagnostics, not runtime semantics.

Compiler facts are labeled proven, guarded, observed, or unknown. Only proven
facts remove checks unconditionally. Observed facts require guards and
deoptimization.

## Consequences

- Existing mandatory return annotations and extension-only semantic forms must
  migrate without breaking ordinary Lua.
- Parser tests run equivalent Lua in both source forms.
- The LSP uses the same contextual parser and type policy as the CLI.
- Optimizations cannot treat annotations as unchecked promises.
