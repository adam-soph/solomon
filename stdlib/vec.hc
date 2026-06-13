#ifndef _VEC_HC
#define _VEC_HC
// vec.hc — implementation (interface in vec.hh).

#include <vec.hh>

U0 VecInit<type T>(Vec<T> *v) { v->data = NULL; v->len = 0; v->cap = 0; }

U0 VecFree<type T>(Vec<T> *v)
{
  if (v->data) Free(v->data);
  v->data = NULL;
  v->len = 0;
  v->cap = 0;
}

U0 VecClear<type T>(Vec<T> *v) { v->len = 0; }
I64 VecLen<type T>(Vec<T> *v) { return v->len; }

// `ReAlloc` extends in place when the buffer is the heap's last allocation.
U0 VecReserve<type T>(Vec<T> *v, I64 need)
{
  if (v->cap >= need) return;
  I64 cap = v->cap;
  if (cap < 4) cap = 4;
  while (cap < need) cap *= 2;
  v->data = ReAlloc(v->data, v->cap * sizeof(T), cap * sizeof(T));
  v->cap = cap;
}

U0 VecPush<type T>(Vec<T> *v, T x)
{
  VecReserve<T>(v, v->len + 1);
  v->data[v->len] = x;
  v->len++;
}

T VecAt<type T>(Vec<T> *v, I64 i) { return v->data[i]; }
T *VecRef<type T>(Vec<T> *v, I64 i) { return &v->data[i]; }
U0 VecSet<type T>(Vec<T> *v, I64 i, T x) { v->data[i] = x; }

T VecPop<type T>(Vec<T> *v) { v->len--; return v->data[v->len]; }

U0 VecClone<type T>(Vec<T> *dst, Vec<T> *src)
{
  VecInit<T>(dst);
  VecReserve<T>(dst, src->len);
  MemCpy(dst->data, src->data, src->len * sizeof(T));
  dst->len = src->len;
}

U0 VecSort<type T>(Vec<T> *v, I64 (*cmp)(T *, T *))
{
  Sort<T>(v->data, v->len, cmp);
}

I64 VecBSearch<type T>(Vec<T> *v, T *key, I64 (*cmp)(T *, T *))
{
  T *p = BSearch<T>(key, v->data, v->len, cmp);
  if (p == NULL) return -1;
  return p - v->data;
}

public U0 Environ(Vec<U8 *> *out)
{
  VecInit(out);
  if (envp == NULL) return;   // no environment (e.g. Windows, for now)
  I64 i = 0;
  while (envp[i] != NULL) {
    VecPush(out, envp[i]);
    i++;
  }
}

#endif
