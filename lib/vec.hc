#ifndef _VEC_HC
#define _VEC_HC
// vec.hc — `Vec<T>`, an owning, growable typed array.
//
// `Vec<T>` is a generic dynamic array, monomorphized per element type at compile time.
// It is typed throughout, so there are no casts and no element-size bookkeeping. Type
// arguments are inferred from the call:
//
//     Vec<I64> v;
//     VecInit(&v);
//     VecPush(&v, 42);             // T = I64 inferred from &v
//     I64 x = VecAt(&v, 0);
//     VecFree(&v);
//
// It works for scalar, pointer, and class element types. It is built on `<stdlib.hc>`'s
// `ReAlloc` (a push loop grows the buffer in place) and `Sort`/`BSearch` (`VecSort`/
// `VecBSearch`), plus `<string.hc>`'s `MemCpy` for `VecClone`. The implementation is pure
// HolyC and behaves identically on the interpreter and every backend. Include with
// `#include <vec.hc>`.
//
// The caller owns the `Vec` struct, and `VecInit(&v)` is required before use. A `Vec`
// owns its buffer: copy it with `VecClone` (not `=`), and free it with `VecFree`.

#include <string.hc>   // MemCpy (VecClone)
#include <stdlib.hc>   // ReAlloc (VecReserve), Sort / BSearch (VecSort / VecBSearch)

public class Vec<type T> {
  T  *data;   // heap buffer of `cap` elements, or NULL before the first allocation
  I64 len;    // elements in use
  I64 cap;    // allocated capacity, in elements
}

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

// Ensure room for at least `need` elements, growing geometrically. `ReAlloc` extends
// in place when the buffer is the heap's last allocation.
U0 VecReserve<type T>(Vec<T> *v, I64 need)
{
  if (v->cap >= need) return;
  I64 cap = v->cap;
  if (cap < 4) cap = 4;
  while (cap < need) cap *= 2;
  v->data = ReAlloc(v->data, v->cap * sizeof(T), cap * sizeof(T));
  v->cap = cap;
}

// Append a value.
U0 VecPush<type T>(Vec<T> *v, T x)
{
  VecReserve<T>(v, v->len + 1);
  v->data[v->len] = x;
  v->len++;
}

// Element `i` by value. `VecRef` returns a pointer for in-place update. `VecSet` writes.
T VecAt<type T>(Vec<T> *v, I64 i) { return v->data[i]; }
T *VecRef<type T>(Vec<T> *v, I64 i) { return &v->data[i]; }
U0 VecSet<type T>(Vec<T> *v, I64 i, T x) { v->data[i] = x; }

// Remove and return the last element. The caller must ensure the Vec is non-empty.
T VecPop<type T>(Vec<T> *v) { v->len--; return v->data[v->len]; }

// Deep-copy `src` into a fresh `dst`. This is the correct way to duplicate a `Vec`.
U0 VecClone<type T>(Vec<T> *dst, Vec<T> *src)
{
  VecInit<T>(dst);
  VecReserve<T>(dst, src->len);
  MemCpy(dst->data, src->data, src->len * sizeof(T));
  dst->len = src->len;
}

// Sort the elements in place by `cmp`, a `<stdlib.hc>` comparator over element pointers
// (`I64 (*)(T *, T *)`).
U0 VecSort<type T>(Vec<T> *v, I64 (*cmp)(T *, T *))
{
  Sort<T>(v->data, v->len, cmp);
}

// Binary-search a sorted `Vec` for `key`, a pointer to a key value. Returns the
// element index, or -1.
I64 VecBSearch<type T>(Vec<T> *v, T *key, I64 (*cmp)(T *, T *))
{
  T *p = BSearch<T>(key, v->data, v->len, cmp);
  if (p == NULL) return -1;
  return p - v->data;
}

// Collect every environment entry ("KEY=VALUE", a `U8 *`) into `out`, a `Vec<U8 *>`
// initialised here, in the OS's order. Read an entry with `VecAt(&out, i)`. This builds
// a `Vec` from the implicit `EnvP` (C's `environ`), so it lives here next to `Vec`
// rather than in `<stdlib.hc>` (where the scalar `Getenv` is). The entries point into
// the process environment and are read-only. `VecFree(&out)` frees the Vec's own buffer,
// not the entries.
public U0 Environ(Vec<U8 *> *out)
{
  VecInit(out);
  if (EnvP == NULL) return;   // no environment (e.g. Windows, for now)
  I64 i = 0;
  while (EnvP[i] != NULL) {
    VecPush(out, EnvP[i]);
    i++;
  }
}

#endif
