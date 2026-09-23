#include <assert.h>
#include "lua.h"
#include "lauxlib.h"

static int fixture_add(lua_State *L) {
  lua_pushinteger(L, luaL_checkinteger(L, 1) + luaL_checkinteger(L, 2));
  return 1;
}

static int fixture_get(lua_State *L);

static int fixture_make(lua_State *L) {
  lua_Integer *value = (lua_Integer *)lua_newuserdatauv(L, sizeof(*value), 0);
  *value = luaL_checkinteger(L, 1);
  if (luaL_newmetatable(L, "sol.fixture.integer")) {
    lua_pushvalue(L, -1);
    lua_setfield(L, -2, "__index");
    lua_pushcfunction(L, fixture_get);
    lua_setfield(L, -2, "get");
  }
  assert(lua_setmetatable(L, -2));
  return 1;
}

static int fixture_get(lua_State *L) {
  lua_Integer *value = (lua_Integer *)luaL_checkudata(L, 1, "sol.fixture.integer");
  lua_pushinteger(L, *value);
  return 1;
}

int luaopen_sol_fixture(lua_State *L) {
  static const luaL_Reg functions[] = {
    {"add", fixture_add},
    {"make", fixture_make},
    {"get", fixture_get},
    {NULL, NULL}
  };
  luaL_newlib(L, functions);
  return 1;
}
