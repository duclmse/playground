function add(n: i64): i64
    _G.counter_calls = (_G.counter_calls or 0) + 1
    local m = require('counter')
    assert(m.add == add)
    print(_G.counter_calls)
    return n + _G.counter_calls
end
