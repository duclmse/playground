#include "lauxlib.h"
#include "lua.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void sol_c_api_shim_anchor(void) {}

const char *lua_pushvfstring(lua_State *L, const char *fmt, va_list args) {
  va_list copy;
  va_copy(copy, args);
  int needed = vsnprintf(NULL, 0, fmt, copy);
  va_end(copy);
  if (needed < 0) {
    lua_pushliteral(L, "formatting error");
    return lua_tostring(L, -1);
  }
  char stack[512];
  if ((size_t)needed < sizeof(stack)) {
    vsnprintf(stack, sizeof(stack), fmt, args);
    return lua_pushlstring(L, stack, (size_t)needed);
  }
  void *alloc_ud = NULL;
  lua_Alloc alloc = lua_getallocf(L, &alloc_ud);
  char *buffer = (char *)alloc(alloc_ud, NULL, 0, (size_t)needed + 1);
  if (buffer == NULL) {
    lua_pushliteral(L, "not enough memory");
    lua_error(L);
  }
  vsnprintf(buffer, (size_t)needed + 1, fmt, args);
  const char *result = lua_pushlstring(L, buffer, (size_t)needed);
  alloc(alloc_ud, buffer, (size_t)needed + 1, 0);
  return result;
}

const char *lua_pushfstring(lua_State *L, const char *fmt, ...) {
  va_list args;
  va_start(args, fmt);
  const char *result = lua_pushvfstring(L, fmt, args);
  va_end(args);
  return result;
}

const char *lua_pushexternalstring(lua_State *L, const char *s, size_t len,
                                   lua_Alloc freef, void *ud) {
  const char *result = lua_pushlstring(L, s, len);
  if (freef != NULL)
    (void)freef(ud, (void *)s, len, 0);
  return result;
}

int luaL_error(lua_State *L, const char *fmt, ...) {
  va_list args;
  va_start(args, fmt);
  (void)lua_pushvfstring(L, fmt, args);
  va_end(args);
  return lua_error(L);
}

void *luaL_alloc(void *ud, void *ptr, size_t osize, size_t nsize) {
  (void)ud;
  (void)osize;
  if (nsize == 0) {
    free(ptr);
    return NULL;
  }
  return realloc(ptr, nsize);
}

void luaL_buffinit(lua_State *L, luaL_Buffer *B) {
  B->b = B->init.b;
  B->size = sizeof(B->init.b);
  B->n = 0;
  B->L = L;
}

char *luaL_prepbuffsize(luaL_Buffer *B, size_t sz) {
  if (sz <= B->size - B->n)
    return B->b + B->n;
  size_t needed = B->n + sz;
  size_t capacity = B->size;
  while (capacity < needed) {
    size_t next = capacity + capacity / 2 + 1;
    if (next < capacity)
      return NULL;
    capacity = next;
  }
  void *alloc_ud = NULL;
  lua_Alloc alloc = lua_getallocf(B->L, &alloc_ud);
  char *next;
  if (B->b == B->init.b) {
    next = (char *)alloc(alloc_ud, NULL, 0, capacity);
    if (next != NULL)
      memcpy(next, B->b, B->n);
  } else {
    next = (char *)alloc(alloc_ud, B->b, B->size, capacity);
  }
  if (next == NULL)
    luaL_error(B->L, "not enough memory");
  B->b = next;
  B->size = capacity;
  return B->b + B->n;
}

char *luaL_buffinitsize(lua_State *L, luaL_Buffer *B, size_t sz) {
  luaL_buffinit(L, B);
  return luaL_prepbuffsize(B, sz);
}

void luaL_addlstring(luaL_Buffer *B, const char *s, size_t len) {
  char *target = luaL_prepbuffsize(B, len);
  memcpy(target, s, len);
  B->n += len;
}

void luaL_addstring(luaL_Buffer *B, const char *s) {
  luaL_addlstring(B, s, strlen(s));
}

void luaL_addvalue(luaL_Buffer *B) {
  size_t len = 0;
  const char *value = luaL_checklstring(B->L, -1, &len);
  luaL_addlstring(B, value, len);
  lua_pop(B->L, 1);
}

void luaL_pushresult(luaL_Buffer *B) {
  lua_pushlstring(B->L, B->b, B->n);
  if (B->b != B->init.b) {
    void *alloc_ud = NULL;
    lua_Alloc alloc = lua_getallocf(B->L, &alloc_ud);
    alloc(alloc_ud, B->b, B->size, 0);
  }
  B->b = B->init.b;
  B->size = sizeof(B->init.b);
  B->n = 0;
}

void luaL_pushresultsize(luaL_Buffer *B, size_t sz) {
  B->n += sz;
  luaL_pushresult(B);
}

void luaL_addgsub(luaL_Buffer *B, const char *s, const char *p, const char *r) {
  size_t plen = strlen(p);
  if (plen == 0) {
    luaL_addstring(B, s);
    return;
  }
  const char *match;
  while ((match = strstr(s, p)) != NULL) {
    luaL_addlstring(B, s, (size_t)(match - s));
    luaL_addstring(B, r);
    s = match + plen;
  }
  luaL_addstring(B, s);
}

const char *luaL_gsub(lua_State *L, const char *s, const char *p,
                      const char *r) {
  luaL_Buffer buffer;
  luaL_buffinit(L, &buffer);
  luaL_addgsub(&buffer, s, p, r);
  luaL_pushresult(&buffer);
  return lua_tostring(L, -1);
}
