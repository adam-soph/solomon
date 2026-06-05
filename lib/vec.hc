#ifndef _VEC_HC
#define _VEC_HC
// vec.hc — `Vec<T>`, an owning, growable typed array (a generic dynamic array),
// monomorphized per element type at compile time. Typed throughout — no casts, no
// element-size bookkeeping. Type arguments are inferred from the call:
//
//     Vec<I64> v;
//     VecInit(&v);
//     VecPush(&v, 42);             // T = I64 inferred from &v
//     I64 x = VecAt(&v, 0);
//     VecFree(&v);
//
// Works for scalar, pointer, and class element types. Built on <mem.hc>'s `ReAlloc`
// (so a push loop grows in place) and <sort.hc> (for `VecSort`/`VecBSearch`). Pure
// HolyC, identical on the interpreter and every backend. Include with `#include <vec.hc>`.
//
// The caller owns the `Vec` struct; `VecInit(&v)` is required before use. `Vec` owns its
// buffer: copy with `VecClone` (not `=`), free with `VecFree`.

#include <mem.hc>
#include <sort.hc>

class Vec<T> {
  T  *data;   // heap buffer of `cap` elements, or NULL before the first allocation
  I64 len;    // elements in use
  I64 cap;    // allocated capacity, in elements
}

U0 VecInit<T>(Vec<T> *v) { v->data = NULL; v->len = 0; v->cap = 0; }

U0 VecFree<T>(Vec<T> *v)
{
  if (v->data) Free(v->data);
  v->data = NULL;
  v->len = 0;
  v->cap = 0;
}

U0 VecClear<T>(Vec<T> *v) { v->len = 0; }
I64 VecLen<T>(Vec<T> *v) { return v->len; }

// Ensure room for at least `need` elements, growing geometrically. `ReAlloc` extends
// in place when the buffer is the heap's last allocation.
U0 VecReserve<T>(Vec<T> *v, I64 need)
{
  if (v->cap >= need) return;
  I64 cap = v->cap;
  if (cap < 4) cap = 4;
  while (cap < need) cap *= 2;
  v->data = ReAlloc(v->data, v->cap * sizeof(T), cap * sizeof(T));
  v->cap = cap;
}

// Append a value.
U0 VecPush<T>(Vec<T> *v, T x)
{
  VecReserve<T>(v, v->len + 1);
  v->data[v->len] = x;
  v->len++;
}

// Element `i` by value; `VecRef` returns a pointer for in-place update; `VecSet` writes.
T VecAt<T>(Vec<T> *v, I64 i) { return v->data[i]; }
T *VecRef<T>(Vec<T> *v, I64 i) { return &v->data[i]; }
U0 VecSet<T>(Vec<T> *v, I64 i, T x) { v->data[i] = x; }

// Remove and return the last element (caller ensures the Vec is non-empty).
T VecPop<T>(Vec<T> *v) { v->len--; return v->data[v->len]; }

// Deep-copy `src` into a fresh `dst` (the correct way to duplicate a `Vec`).
U0 VecClone<T>(Vec<T> *dst, Vec<T> *src)
{
  VecInit<T>(dst);
  VecReserve<T>(dst, src->len);
  MemCpy(dst->data, src->data, src->len * sizeof(T));
  dst->len = src->len;
}

// Sort the elements in place by `cmp` (a `<sort.hc>` comparator over element pointers
// — cast them to `T *`).
U0 VecSort<T>(Vec<T> *v, I64 (*cmp)(U8 *, U8 *))
{
  Sort(v->data, v->len, sizeof(T), cmp);
}

// Binary-search a sorted `Vec` for `key` (a pointer to a key value). Returns the
// element index, or -1.
I64 VecBSearch<T>(Vec<T> *v, T *key, I64 (*cmp)(U8 *, U8 *))
{
  U8 *p = BSearch(key, v->data, v->len, sizeof(T), cmp);
  if (p == NULL) return -1;
  return (p - (U8 *)v->data) / sizeof(T);
}

#endif
