-- Sol language demo - the bottom of a two-level module chain (main.sol
-- imports shapes.pipeline, which imports this file as `base`, relative to
-- its own directory: `shapes/pipeline.sol` -> `shapes/base.sol`). See
-- crates/sol/tests/fixtures/modules for the fixture this pattern mirrors.

export function double(value: i64): i64
    return value * 2
end
