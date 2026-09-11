# Source text and lexical grammar

## Source modes

The command-line tools select source mode from the input extension. `.sol`
selects typed Sol mode and `.lua` selects Lua-compatibility mode. `.fl` is a
legacy alias for `.sol` accepted by the CLI; new files should use `.sol`.

Source loading is byte-oriented. Source locations use byte offsets together
with one-based human-readable line and column values. A source file and a
string literal are not required to contain valid UTF-8 in Lua mode.

## Tokens common to both modes

Whitespace separates tokens and is otherwise insignificant. `--` begins a
line comment. Identifiers, numeric literals, quoted strings, punctuation, and
operators are tokenized before parsing. The longest valid operator token wins.

Lua mode additionally accepts Lua long strings and comments, hexadecimal and
hexadecimal-floating numerals, Lua escape forms, labels, varargs, and the Lua
5.5 `global` declaration grammar. Exact support is listed in
[Lua compatibility](lua-compatibility.md).

## Keywords

Typed Sol reserves its declaration and control-flow words, including
`function`, `fn`, `local`, `extern`, `export`, `import`, `struct`, `type`,
`return`, `if`, `then`, `elseif`, `else`, `while`, `for`, `do`, `end`, `true`,
`false`, `nil`, `and`, `or`, `not`, `is`, and `as`.

In `.sol`, `fn` is an exact declaration/type alias for `function`. It is valid
in forms such as `fn name(...)`, `local fn name(...)`, `export fn name(...)`,
`extern fn name(...)`, and `fn(T) -> U`. In `.lua`, `fn` remains an ordinary
identifier and does not introduce a function.

Lua 5.5 `global` declarations are accepted only in `.lua`. Typed Sol must
reject that declaration form with a source-located diagnostic.

## Diagnostics

Lexical and parse errors must identify the source file and offending span when
available. The CLI's JSON diagnostic format is part of the tooling interface,
not the grammar; consumers should use its stable diagnostic code rather than
matching prose messages.
