-- Sol language demo - the middle of a two-level module chain: imported by
-- main.sol as `shapes.pipeline`, and itself imports `base` (resolved
-- relative to this file, i.e. `shapes/base.sol`). `main.sol` reaches
-- `base.double` only through the qualified name `shapes.pipeline.double_
-- twice`, never importing `shapes.base` directly.

import base

export type Doubled = i64

export function double_twice(value: i64): Doubled
    return base.double(base.double(value))
end
