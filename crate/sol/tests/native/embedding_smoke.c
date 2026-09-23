#include <assert.h>
#include <stdint.h>
#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"

static int multiply(lua_State *L) {
  lua_Integer a = luaL_checkinteger(L, 1);
  lua_Integer b = luaL_checkinteger(L, 2);
  lua_pushinteger(L, a * b);
  return 1;
}

static int add_offset(lua_State *L) {
  lua_pushinteger(L, luaL_checkinteger(L, 1) + lua_tointeger(L, lua_upvalueindex(1)));
  return 1;
}

static int fail(lua_State *L) {
  return luaL_error(L, "fixture failure %d", 17);
}

static int formatted(lua_State *L) {
  lua_pushfstring(L, "%d:%s", 7, "ok");
  return 1;
}

static int buffered(lua_State *L) {
  luaL_Buffer buffer;
  luaL_buffinit(L, &buffer);
  luaL_addstring(&buffer, "buffer");
  luaL_addchar(&buffer, ':');
  lua_pushliteral(L, "ok");
  luaL_addvalue(&buffer);
  luaL_pushresult(&buffer);
  return 1;
}

static int must_jump(lua_State *L) {
  (void)luaL_checkinteger(L, 1);
  lua_pushliteral(L, "unreachable after type error");
  return 1;
}

static int warning_count;
static int hook_count;
static int close_count;

static void capture_warning(void *ud, const char *message, int to_continue) {
  int *count = (int *)ud;
  assert(message != NULL);
  assert(to_continue == 0 || to_continue == 1);
  ++*count;
}

static void capture_hook(lua_State *L, lua_Debug *debug) {
  (void)L;
  assert(debug != NULL);
  ++hook_count;
}

static int finish_yield(lua_State *L, int status, lua_KContext context) {
  assert(status == LUA_YIELD);
  lua_pushinteger(L, luaL_checkinteger(L, 1) + (lua_Integer)context);
  return 1;
}

static int yield_with_continuation(lua_State *L) {
  lua_pushinteger(L, 7);
  return lua_yieldk(L, 1, 35, finish_yield);
}

static int count_close(lua_State *L) {
  (void)L;
  ++close_count;
  return 0;
}

int main(int argc, char **argv) {
  lua_State *L = luaL_newstate();
  assert(L != NULL);
  luaL_openlibs(L);
  lua_setwarnf(L, capture_warning, &warning_count);
  lua_warning(L, "first ", 1);
  lua_warning(L, "second", 0);
  assert(warning_count == 2);
  lua_pushcfunction(L, multiply);
  lua_setglobal(L, "multiply");
  lua_pushinteger(L, 1);
  lua_pushcclosure(L, add_offset, 1);
  lua_setglobal(L, "add_offset");
  assert(luaL_dostring(L, "return multiply(6, 7), add_offset(41)") == LUA_OK);
  assert(lua_tointeger(L, -2) == 42);
  assert(lua_tointeger(L, -1) == 42);
  lua_pop(L, 2);
  lua_pushcfunction(L, fail);
  lua_setglobal(L, "fixture_fail");
  lua_pushcfunction(L, formatted);
  lua_setglobal(L, "formatted");
  lua_pushcfunction(L, buffered);
  lua_setglobal(L, "buffered");
  assert(luaL_dostring(L,
      "local ok, message = pcall(fixture_fail); "
      "return not ok and message == 'fixture failure 17' and "
      "formatted() == '7:ok' and buffered() == 'buffer:ok'") == LUA_OK);
  assert(lua_toboolean(L, -1));
  lua_pop(L, 1);
  lua_pushcfunction(L, must_jump);
  lua_setglobal(L, "must_jump");
  assert(luaL_dostring(L,
      "local ok, message = pcall(must_jump, 'not an integer'); "
      "return not ok and message:find('integer expected', 1, true) ~= nil") == LUA_OK);
  assert(lua_toboolean(L, -1));
  lua_pop(L, 1);

  uint8_t *bytes = (uint8_t *)lua_newuserdatauv(L, 32, 0);
  assert(bytes != NULL);
  bytes[31] = 99;
  assert(((uint8_t *)lua_touserdata(L, -1))[31] == 99);
  assert(lua_rawlen(L, -1) == 32);
  lua_pop(L, 1);

  (void)lua_newuserdatauv(L, 1, 1);
  lua_pushinteger(L, 73);
  assert(lua_setiuservalue(L, -2, 1));
  assert(lua_getiuservalue(L, -1, 1) == LUA_TNUMBER);
  assert(lua_tointeger(L, -1) == 73);
  lua_pop(L, 2);

  lua_newtable(L);
  lua_pushinteger(L, 91);
  lua_setfield(L, -2, "value");
  lua_pushliteral(L, "raw");
  lua_pushinteger(L, 92);
  lua_rawset(L, -3);
  lua_pushinteger(L, 3);
  lua_seti(L, -2, 4);
  lua_pushliteral(L, "raw");
  assert(lua_rawget(L, -2) == LUA_TNUMBER);
  assert(lua_tointeger(L, -1) == 92);
  lua_pop(L, 1);
  assert(lua_geti(L, -1, 4) == LUA_TNUMBER);
  assert(lua_tointeger(L, -1) == 3);
  lua_pop(L, 1);
  int table_ref = luaL_ref(L, LUA_REGISTRYINDEX);
  assert(table_ref >= 0);
  assert(lua_rawgeti(L, LUA_REGISTRYINDEX, table_ref) == LUA_TTABLE);
  assert(lua_getfield(L, -1, "value") == LUA_TNUMBER);
  assert(lua_tointeger(L, -1) == 91);
  lua_pop(L, 2);
  luaL_unref(L, LUA_REGISTRYINDEX, table_ref);

  static int pointer_key;
  lua_pushinteger(L, 123);
  lua_rawsetp(L, LUA_REGISTRYINDEX, &pointer_key);
  assert(lua_rawgetp(L, LUA_REGISTRYINDEX, &pointer_key) == LUA_TNUMBER);
  assert(lua_tointeger(L, -1) == 123);
  lua_pop(L, 1);

  lua_pushinteger(L, 10);
  lua_pushcclosure(L, add_offset, 1);
  assert(lua_upvalueid(L, -1, 1) != NULL);
  assert(lua_getupvalue(L, -1, 1) != NULL);
  assert(lua_tointeger(L, -1) == 10);
  lua_pop(L, 1);
  lua_pushinteger(L, 1);
  assert(lua_setupvalue(L, -2, 1) != NULL);
  lua_pop(L, 1);

  lua_newtable(L);
  lua_newtable(L);
  lua_pushcfunction(L, count_close);
  lua_setfield(L, -2, "__close");
  assert(lua_setmetatable(L, -2));
  lua_toclose(L, -1);
  lua_pop(L, 1);
  assert(close_count == 1);

  lua_pushcfunction(L, yield_with_continuation);
  lua_setglobal(L, "c_yield");
  lua_State *thread = lua_newthread(L);
  assert(thread != NULL);
  assert(lua_tothread(L, -1) == thread);
  lua_sethook(thread, capture_hook,
              LUA_MASKCALL | LUA_MASKRET | LUA_MASKLINE | LUA_MASKCOUNT, 1);
  assert(lua_gethook(thread) == capture_hook);
  assert(lua_gethookcount(thread) == 1);
  assert(luaL_loadstring(thread, "return c_yield()") == LUA_OK);
  int result_count = 0;
  assert(lua_resume(thread, L, 0, &result_count) == LUA_YIELD);
  assert(result_count == 1 && lua_tointeger(thread, -1) == 7);
  lua_settop(thread, 0);
  lua_pushinteger(thread, 5);
  assert(lua_resume(thread, L, 1, &result_count) == LUA_OK);
  assert(result_count == 1 && lua_tointeger(thread, -1) == 40);
  assert(hook_count > 0);
  lua_pop(L, 1);

  assert(argc == 2);
  assert(lua_getglobal(L, "package") == LUA_TTABLE);
  lua_pushstring(L, argv[1]);
  lua_setfield(L, -2, "cpath");
  lua_pop(L, 1);
  assert(luaL_dostring(L,
      "local fixture = require('sol_fixture'); "
      "local value = fixture.make(41); "
      "return fixture.add(value:get(), 1)") == LUA_OK);
  assert(lua_tointeger(L, -1) == 42);
  lua_close(L);
  return 0;
}
