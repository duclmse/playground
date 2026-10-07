import scope
function main(): i64
    return scope.via_value() + scope.shadow() + scope.param(42) + scope.loop()
end
