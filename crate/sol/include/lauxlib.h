#ifndef SOL_LAUXLIB_H
#define SOL_LAUXLIB_H
#include "lua.h"

typedef struct luaL_Reg {
  const char *name;
  lua_CFunction func;
} luaL_Reg;

#define LUAL_BUFFERSIZE 8192
typedef struct luaL_Buffer {
  char *b;
  size_t size;
  size_t n;
  lua_State *L;
  union {
    max_align_t align;
    char b[LUAL_BUFFERSIZE];
  } init;
} luaL_Buffer;

#define LUA_NOREF (-2)
#define LUA_REFNIL (-1)

lua_State *luaL_newstate(void);
void luaL_checkversion_(lua_State *L, lua_Number version, size_t sizes);
int luaL_loadbufferx(lua_State *L, const char *buff, size_t size,
                     const char *name, const char *mode);
int luaL_loadstring(lua_State *L, const char *s);
int luaL_loadfilex(lua_State *L, const char *filename, const char *mode);
int luaL_error(lua_State *L, const char *message, ...);
int luaL_argerror(lua_State *L, int arg, const char *message);
int luaL_typeerror(lua_State *L, int arg, const char *type_name);
int luaL_checkoption(lua_State *L, int arg, const char *def,
                     const char *const options[]);
void luaL_where(lua_State *L, int level);
const char *luaL_tolstring(lua_State *L, int idx, size_t *len);
int luaL_getmetafield(lua_State *L, int obj, const char *field);
int luaL_callmeta(lua_State *L, int obj, const char *field);
int luaL_getsubtable(lua_State *L, int idx, const char *field);
void luaL_requiref(lua_State *L, const char *module, lua_CFunction openf,
                   int global);
unsigned luaL_makeseed(lua_State *L);
void luaL_traceback(lua_State *L, lua_State *source, const char *message,
                    int level);
int luaL_fileresult(lua_State *L, int status, const char *filename);
int luaL_execresult(lua_State *L, int status);
void luaL_setfuncs(lua_State *L, const luaL_Reg *l, int nup);
int luaL_newmetatable(lua_State *L, const char *name);
void *luaL_testudata(lua_State *L, int arg, const char *name);
void *luaL_checkudata(lua_State *L, int arg, const char *name);
int luaL_ref(lua_State *L, int table);
void luaL_unref(lua_State *L, int table, int ref);
lua_Integer luaL_checkinteger(lua_State *L, int arg);
lua_Integer luaL_optinteger(lua_State *L, int arg, lua_Integer def);
lua_Number luaL_checknumber(lua_State *L, int arg);
lua_Number luaL_optnumber(lua_State *L, int arg, lua_Number def);
const char *luaL_checklstring(lua_State *L, int arg, size_t *len);
const char *luaL_optlstring(lua_State *L, int arg, const char *def,
                            size_t *len);
void luaL_checkstack(lua_State *L, int size, const char *message);
void luaL_checktype(lua_State *L, int arg, int type);
void luaL_checkany(lua_State *L, int arg);
lua_Integer luaL_len(lua_State *L, int idx);
void luaL_setmetatable(lua_State *L, const char *name);
void *luaL_alloc(void *ud, void *ptr, size_t osize, size_t nsize);
void luaL_buffinit(lua_State *L, luaL_Buffer *buffer);
char *luaL_prepbuffsize(luaL_Buffer *buffer, size_t size);
char *luaL_buffinitsize(lua_State *L, luaL_Buffer *buffer, size_t size);
void luaL_addlstring(luaL_Buffer *buffer, const char *s, size_t len);
void luaL_addstring(luaL_Buffer *buffer, const char *s);
void luaL_addvalue(luaL_Buffer *buffer);
void luaL_pushresult(luaL_Buffer *buffer);
void luaL_pushresultsize(luaL_Buffer *buffer, size_t size);
void luaL_addgsub(luaL_Buffer *buffer, const char *s, const char *pattern,
                  const char *replacement);
const char *luaL_gsub(lua_State *L, const char *s, const char *pattern,
                      const char *replacement);

#define luaL_checkstring(L, n) luaL_checklstring((L), (n), NULL)
#define luaL_loadbuffer(L, s, sz, n) luaL_loadbufferx((L), (s), (sz), (n), NULL)
#define luaL_loadfile(L, f) luaL_loadfilex((L), (f), NULL)
#define luaL_dostring(L, s)                                                    \
  (luaL_loadstring((L), (s)) || lua_pcall((L), 0, LUA_MULTRET, 0))
#define luaL_newlibtable(L, l)                                                 \
  lua_createtable((L), 0, (int)(sizeof(l) / sizeof((l)[0]) - 1))
#define luaL_newlib(L, l)                                                      \
  (luaL_newlibtable((L), (l)), luaL_setfuncs((L), (l), 0))
#define LUAL_NUMSIZES (sizeof(lua_Integer) * 16 + sizeof(lua_Number))
#define luaL_checkversion(L)                                                   \
  luaL_checkversion_((L), LUA_VERSION_NUM, LUAL_NUMSIZES)
#define luaL_addchar(B, c)                                                     \
  ((void)((B)->n < (B)->size || luaL_prepbuffsize((B), 1)),                    \
   ((B)->b[(B)->n++] = (c)))
#define luaL_addsize(B, s) ((B)->n += (s))
#define luaL_bufflen(B) ((B)->n)
#define luaL_buffaddr(B) ((B)->b)

#endif
