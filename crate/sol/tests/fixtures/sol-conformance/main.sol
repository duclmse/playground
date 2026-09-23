-- Upstream main.lua tests the `lua` standalone executable's own
-- command-line argument, stdin, and shell interaction behavior. That is a
-- property of a host CLI, not of the language typed Sol programs are
-- written in - `sol run`/`sol build`/`sol debug`'s own CLI surface is
-- covered by main.rs directly, not by a `.sol` program. Not applicable -
-- intentional stub.
function main(): i64
    return 0
end
