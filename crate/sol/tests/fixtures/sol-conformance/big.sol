-- Upstream big.lua is a general stress test gated behind `require "debug"`
-- (not implemented; see tests/lua55/manifest.toml). Reinterpreted here as a
-- typed stress test over a large Array<i64> and a small set of struct-typed
-- records instead of dynamic tables, exercising the same "big data" intent
-- without any debug-library dependency.
struct Node {
    value: i64,
    weight: i64,
}

function main(): i64
    local n: i64 = 20000
    local data = new_array_i64(n)
    local i: i64 = 0
    while i < n do
        data[i] = i * 3 - 1
        i = i + 1
    end

    local nodes: Array<Node> = {
        Node { value = 1, weight = 2 },
        Node { value = 3, weight = 4 },
    }

    local total: i64 = 0
    i = 0
    while i < n do
        total = total + data[i]
        i = i + 1
    end
    return total + nodes[0].value + nodes[0].weight + nodes[1].value + nodes[1].weight
end
