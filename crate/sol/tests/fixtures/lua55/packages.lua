-- Original Lua 5.5 behavioral fixture; assertions are the oracle.
local count = 0
package.preload.fixture_module = function(name, data)
    count = count + 1;
    return {
        name = name
    }
end
local a = require('fixture_module');
local b = require('fixture_module')
assert(a == b and a.name == 'fixture_module' and count == 1 and package.loaded.fixture_module == a)
package.loaded.fixture_module = nil;
require('fixture_module');
assert(count == 2)
package.preload.fixture_empty = function()
end;
assert(require('fixture_empty') == true)
assert(type(package.path) == 'string' and type(package.cpath) == 'string')
assert(type(package.config) == 'string' and type(package.searchers) == 'table')
assert(type(package.loadlib) == 'function' and type(package.searchpath) == 'function')
local p, e = package.searchpath('certainly_absent_fixture_module', './?.lua');
assert(p == nil and type(e) == 'string')
print('ok: packages')
