-- Upstream all.lua is the distribution's test-suite driver: it locates and
-- runs every other file in this directory through the standalone `lua`
-- executable and validates the reference environment (encoding, seed,
-- assertion helpers) around them. Typed Sol has no standalone multi-file
-- test-suite-driver concept at the language level, and this whole
-- `sol-conformance` suite already plays that role externally (see
-- tests/sol-conformance/manifest.toml). Not applicable - intentional stub.
function main(): i64
    return 0
end
