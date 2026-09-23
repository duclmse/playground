#ifndef SOL_LUA_H
#define SOL_LUA_H

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LUA_VERSION_MAJOR "5"
#define LUA_VERSION_MINOR "5"
#define LUA_VERSION_RELEASE "1"
#define LUA_VERSION_NUM 505

#define LUA_OK 0
#define LUA_YIELD 1
#define LUA_ERRRUN 2
#define LUA_ERRSYNTAX 3
#define LUA_ERRMEM 4
#define LUA_ERRERR 5
#define LUA_MULTRET (-1)
#define LUA_REGISTRYINDEX (-1001000)
#define lua_upvalueindex(i) (LUA_REGISTRYINDEX - (i))
#define LUA_API extern
#define LUALIB_API extern
#define LUAMOD_API extern

#define LUA_TNONE (-1)
#define LUA_TNIL 0
#define LUA_TBOOLEAN 1
#define LUA_TLIGHTUSERDATA 2
#define LUA_TNUMBER 3
#define LUA_TSTRING 4
#define LUA_TTABLE 5
#define LUA_TFUNCTION 6
#define LUA_TUSERDATA 7
#define LUA_TTHREAD 8

#define LUA_OPADD 0
#define LUA_OPSUB 1
#define LUA_OPMUL 2
#define LUA_OPMOD 3
#define LUA_OPPOW 4
#define LUA_OPDIV 5
#define LUA_OPIDIV 6
#define LUA_OPBAND 7
#define LUA_OPBOR 8
#define LUA_OPBXOR 9
#define LUA_OPSHL 10
#define LUA_OPSHR 11
#define LUA_OPUNM 12
#define LUA_OPBNOT 13
#define LUA_OPEQ 0
#define LUA_OPLT 1
#define LUA_OPLE 2

typedef struct lua_State lua_State;
typedef struct lua_Debug lua_Debug;
typedef int64_t lua_Integer;
typedef uint64_t lua_Unsigned;
typedef double lua_Number;
typedef intptr_t lua_KContext;
typedef int (*lua_CFunction)(lua_State *L);
typedef int (*lua_KFunction)(lua_State *L, int status, lua_KContext ctx);
typedef void *(*lua_Alloc)(void *ud, void *ptr, size_t osize, size_t nsize);
typedef const char *(*lua_Reader)(lua_State *L, void *data, size_t *size);
typedef int (*lua_Writer)(lua_State *L, const void *data, size_t size,
                          void *ud);
typedef void (*lua_WarnFunction)(void *ud, const char *msg, int tocont);
typedef void (*lua_Hook)(lua_State *L, lua_Debug *ar);

#define LUA_HOOKCALL 0
#define LUA_HOOKRET 1
#define LUA_HOOKLINE 2
#define LUA_HOOKCOUNT 3
#define LUA_HOOKTAILCALL 4
#define LUA_MASKCALL (1 << LUA_HOOKCALL)
#define LUA_MASKRET (1 << LUA_HOOKRET)
#define LUA_MASKLINE (1 << LUA_HOOKLINE)
#define LUA_MASKCOUNT (1 << LUA_HOOKCOUNT)
#define LUA_IDSIZE 60

struct lua_Debug {
  int event;
  const char *name;
  const char *namewhat;
  const char *what;
  const char *source;
  size_t srclen;
  int currentline;
  int linedefined;
  int lastlinedefined;
  unsigned char nups;
  unsigned char nparams;
  char isvararg;
  unsigned char extraargs;
  char istailcall;
  int ftransfer;
  int ntransfer;
  char short_src[LUA_IDSIZE];
  void *i_ci;
};

lua_State *lua_newstate(lua_Alloc f, void *ud, unsigned seed);
void lua_close(lua_State *L);
lua_State *lua_newthread(lua_State *L);
int lua_closethread(lua_State *L, lua_State *from);
lua_Number lua_version(lua_State *L);
lua_CFunction lua_atpanic(lua_State *L, lua_CFunction panicf);
void lua_setwarnf(lua_State *L, lua_WarnFunction f, void *ud);
void lua_warning(lua_State *L, const char *msg, int tocont);
int lua_gettop(lua_State *L);
int lua_absindex(lua_State *L, int idx);
int lua_checkstack(lua_State *L, int n);
void lua_settop(lua_State *L, int idx);
void lua_toclose(lua_State *L, int idx);
void lua_closeslot(lua_State *L, int idx);
void lua_pushvalue(lua_State *L, int idx);
void lua_rotate(lua_State *L, int idx, int n);
void lua_copy(lua_State *L, int fromidx, int toidx);
void lua_xmove(lua_State *from, lua_State *to, int n);
int lua_type(lua_State *L, int idx);
const char *lua_typename(lua_State *L, int tp);
void lua_pushnil(lua_State *L);
void lua_pushboolean(lua_State *L, int b);
void lua_pushinteger(lua_State *L, lua_Integer n);
void lua_pushnumber(lua_State *L, lua_Number n);
void lua_pushlightuserdata(lua_State *L, void *p);
const char *lua_pushlstring(lua_State *L, const char *s, size_t len);
const char *lua_pushstring(lua_State *L, const char *s);
const char *lua_pushexternalstring(lua_State *L, const char *s, size_t len,
                                   lua_Alloc freef, void *ud);
const char *lua_pushvfstring(lua_State *L, const char *fmt, va_list args);
const char *lua_pushfstring(lua_State *L, const char *fmt, ...);
void lua_pushcclosure(lua_State *L, lua_CFunction fn, int n);
int lua_pushthread(lua_State *L);
const char *lua_getupvalue(lua_State *L, int funcindex, int n);
const char *lua_setupvalue(lua_State *L, int funcindex, int n);
void *lua_upvalueid(lua_State *L, int fidx, int n);
void lua_upvaluejoin(lua_State *L, int fidx1, int n1, int fidx2, int n2);
int lua_getstack(lua_State *L, int level, lua_Debug *ar);
int lua_getinfo(lua_State *L, const char *what, lua_Debug *ar);
const char *lua_getlocal(lua_State *L, const lua_Debug *ar, int n);
const char *lua_setlocal(lua_State *L, const lua_Debug *ar, int n);
void lua_sethook(lua_State *L, lua_Hook func, int mask, int count);
lua_Hook lua_gethook(lua_State *L);
int lua_gethookmask(lua_State *L);
int lua_gethookcount(lua_State *L);
int lua_toboolean(lua_State *L, int idx);
lua_Integer lua_tointegerx(lua_State *L, int idx, int *isnum);
lua_Number lua_tonumberx(lua_State *L, int idx, int *isnum);
const char *lua_tolstring(lua_State *L, int idx, size_t *len);
void *lua_touserdata(lua_State *L, int idx);
lua_State *lua_tothread(lua_State *L, int idx);
void *lua_newuserdatauv(lua_State *L, size_t size, int nuvalue);
int lua_iscfunction(lua_State *L, int idx);
int lua_isinteger(lua_State *L, int idx);
int lua_isnumber(lua_State *L, int idx);
int lua_isstring(lua_State *L, int idx);
int lua_isuserdata(lua_State *L, int idx);
lua_CFunction lua_tocfunction(lua_State *L, int idx);
const void *lua_topointer(lua_State *L, int idx);
size_t lua_rawlen(lua_State *L, int idx);
int lua_rawequal(lua_State *L, int idx1, int idx2);
int lua_compare(lua_State *L, int idx1, int idx2, int op);
void lua_arith(lua_State *L, int op);
lua_Alloc lua_getallocf(lua_State *L, void **ud);
void lua_setallocf(lua_State *L, lua_Alloc f, void *ud);
void lua_createtable(lua_State *L, int narr, int nrec);
int lua_getglobal(lua_State *L, const char *name);
void lua_setglobal(lua_State *L, const char *name);
int lua_gettable(lua_State *L, int idx);
void lua_settable(lua_State *L, int idx);
int lua_getfield(lua_State *L, int idx, const char *key);
void lua_setfield(lua_State *L, int idx, const char *key);
int lua_geti(lua_State *L, int idx, lua_Integer n);
void lua_seti(lua_State *L, int idx, lua_Integer n);
int lua_rawget(lua_State *L, int idx);
void lua_rawset(lua_State *L, int idx);
int lua_rawgetp(lua_State *L, int idx, const void *p);
void lua_rawsetp(lua_State *L, int idx, const void *p);
int lua_getmetatable(lua_State *L, int idx);
int lua_setmetatable(lua_State *L, int idx);
int lua_rawgeti(lua_State *L, int idx, lua_Integer n);
void lua_rawseti(lua_State *L, int idx, lua_Integer n);
int lua_getiuservalue(lua_State *L, int idx, int n);
int lua_setiuservalue(lua_State *L, int idx, int n);
void lua_callk(lua_State *L, int nargs, int nresults, lua_KContext ctx,
               lua_KFunction k);
int lua_pcallk(lua_State *L, int nargs, int nresults, int errfunc,
               lua_KContext ctx, lua_KFunction k);
int lua_yieldk(lua_State *L, int nresults, lua_KContext ctx, lua_KFunction k);
int lua_resume(lua_State *L, lua_State *from, int nargs, int *nresults);
int lua_status(lua_State *L);
int lua_isyieldable(lua_State *L);
int lua_load(lua_State *L, lua_Reader reader, void *data, const char *name,
             const char *mode);
int lua_dump(lua_State *L, lua_Writer writer, void *data, int strip);
int lua_error(lua_State *L);
int lua_next(lua_State *L, int idx);
int lua_gc(lua_State *L, int what, ...);
void lua_len(lua_State *L, int idx);
void lua_concat(lua_State *L, int n);
unsigned lua_numbertocstring(lua_State *L, int idx, char *buffer);
size_t lua_stringtonumber(lua_State *L, const char *s);

#define lua_pop(L, n) lua_settop((L), -(n) - 1)
#define lua_yield(L, n) lua_yieldk((L), (n), 0, NULL)
#define lua_insert(L, idx) lua_rotate((L), (idx), 1)
#define lua_newtable(L) lua_createtable((L), 0, 0)
#define lua_pushcfunction(L, f) lua_pushcclosure((L), (f), 0)
#define lua_tointeger(L, i) lua_tointegerx((L), (i), NULL)
#define lua_tonumber(L, i) lua_tonumberx((L), (i), NULL)
#define lua_tostring(L, i) lua_tolstring((L), (i), NULL)
#define lua_call(L, n, r) lua_callk((L), (n), (r), 0, NULL)
#define lua_pcall(L, n, r, f) lua_pcallk((L), (n), (r), (f), 0, NULL)
#define lua_isnil(L, n) (lua_type((L), (n)) == LUA_TNIL)
#define lua_istable(L, n) (lua_type((L), (n)) == LUA_TTABLE)
#define lua_isfunction(L, n) (lua_type((L), (n)) == LUA_TFUNCTION)
#define lua_isuserdata(L, n) (lua_type((L), (n)) == LUA_TUSERDATA)
#define lua_pushliteral(L, s)                                                  \
  lua_pushlstring((L), "" s, (sizeof(s) / sizeof(char)) - 1)

#ifdef __cplusplus
}
#endif
#endif
