-- Sol language demo - imported module.
--
-- Shows: `export` on a type alias, a `struct`, and a function; a module is
-- just a `.sol` file resolved by `import path.to.module` (relative to the
-- importing file, `.sol` tried before `.lua`); and a bare top-level
-- expression statement, which becomes this module's initializer, run once
-- (in import-dependency order) before `main` executes anywhere in the
-- program. See docs/spec/functions-and-modules.md.

export type Scalar = f64

export struct Vector2 {
    x: Scalar,
    y: Scalar,
}

export function magnitude_squared(v: Vector2): Scalar
    return v.x * v.x + v.y * v.y
end

-- Module initializer: exercised for its side effect on program startup,
-- not for this value (which is discarded).
magnitude_squared(Vector2 { x = 1.0, y = 1.0 })
