# bytecode-to-sol

Best-effort decompilation of Sol tier-0 instructions and Lua binary chunks into
annotated, Sol-style register code.

```sh
cargo run --manifest-path crates/bytecode-to-sol/Cargo.toml -- \
  sol instructions.bin --name recovered

luac -o program.luac program.lua
cargo run --manifest-path crates/bytecode-to-sol/Cargo.toml -- \
  lua program.luac -o recovered.sol
```

The `sol` adapter reads consecutive little-endian 32-bit words using the opcode
layout in `crates/sol/src/bytecode.rs`. Sol does not yet persist its constant
pool, function table, type layouts, or debug names, so references to that
missing metadata are emitted as `K[n]`, `function_n`, and `field_n`.

The `lua` adapter validates the Lua chunk signature and asks `luac -l -l` to
decode it. Use `--luac /path/to/luac` when the chunk was produced by a different
installed Lua version. This intentionally avoids pretending that one opcode
table can safely decode every Lua release. Existing disassembler output can be
converted without a binary chunk using the `lua-listing` mode.

## Output contract

The result is recovery-oriented Sol-style code, not guaranteed round-trippable
source. Straight-line loads, moves, arithmetic, calls, and returns are rendered
as expressions. Operations that need lost metadata or control-flow structuring
retain their program counter and original opcode in comments. The converter
never silently drops an instruction.
