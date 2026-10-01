local track = {}
track.count = 0

local function closer()
    return setmetatable({}, { __close = function() track.count = track.count + 1 end })
end

local function use_closers(a, b)
    do
        local x <close> = a
        local y <close> = b
    end
    return track.count
end

print(use_closers(closer(), closer()))
print(use_closers(closer(), closer()))
