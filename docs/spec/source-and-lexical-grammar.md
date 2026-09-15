# Source text and lexical grammar

## Source profiles

Both `.lua` and `.sol` use the Lua 5.5 grammar. `.sol` additionally enables
contextual Sol declarations and operators. The extension supplies defaults for
syntax extensions and type policy; it does not define a second base language.
`.fl` is a legacy alias for `.sol` accepted by the CLI; new files should use
`.sol`.

Source loading is byte-oriented. Source locations use byte offsets together
with one-based human-readable line and column values. A source file and a
string literal are not required to contain valid UTF-8 in either profile.

## Tokens common to both modes

Whitespace separates tokens and is otherwise insignificant. `--` begins a
line comment. Identifiers, numeric literals, quoted strings, punctuation, and
operators are tokenized before parsing. The longest valid operator token wins.

Lua long strings and comments, hexadecimal and hexadecimal-floating numerals,
Lua escape forms, labels, varargs, and the Lua 5.5 `global` declaration grammar
are common syntax. Exact support is listed in [Lua compatibility](lua-compatibility.md).

## Keywords

Lua control-flow words retain their Lua meaning. Sol-only words including
`fn`, `extern`, `export`, `import`, `struct`, `type`, `is`, and `as` are lexed
as identifiers and recognized contextually only where their extension grammar
is unambiguous. They remain valid Lua identifiers elsewhere.

In `.sol`, `fn` is an exact declaration/type alias for `function`. It is valid
in forms such as `fn name(...)`, `local fn name(...)`, `export fn name(...)`,
`extern fn name(...)`, and `fn(T) -> U`. In `.lua`, `fn` remains an ordinary
identifier and does not introduce a function.

Lua 5.5 `global` declarations, dotted/method declarations, varargs, multiple
results, first-class calls, and call sugar are accepted in both profiles.

The frontend exposes `LanguageConfig` with independent dialect, Sol-extension,
and type-policy fields. Its lossless parse result retains each token's exact
source bytes and half-open byte span alongside the semantic AST.

## Diagnostics

Lexical and parse errors must identify the source file and offending span when
available. The CLI's JSON diagnostic format is part of the tooling interface,
not the grammar; consumers should use its stable diagnostic code rather than
matching prose messages.
