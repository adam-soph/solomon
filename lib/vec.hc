#ifndef _VEC_HC
#define _VEC_HC
// vec.hc — `Vec`, an owning, growable array of fixed-size elements (a generic
// dynamic array / "vector").
//
// Elements are stored by value in a byte buffer, so one `Vec` type holds elements of
// *any* type — scalars, pointers, or class values — with the size chosen at `VecInit`
// (`sizeof(I64)`, `sizeof(F64)`, `sizeof(U8 *)`, `sizeof(SomeClass)`, …). Access is by
// pointer: `VecPush`/`VecPop`/`VecAt` return a `U8 *` into the buffer that you cast to
// the element type and read or write through:
//
//     Vec v; VecInit(&v, sizeof(I64));
//     *(I64 *)VecPush(&v) = 42;          // emplace: write into the new slot
//     I64 x = *(I64 *)VecAt(&v, 0);      // read element 0
//
//     Vec w; VecInit(&w, sizeof(Pt));    // a class element
//     Pt *p = VecPush(&w); p->x = 1;     // write fields straight into the slot
//
// Writing through the buffer pointer (rather than copying a local in) is what keeps
// it portable across the interpreter and every backend.
//
// Built on the heap primitives in <mem.hc> (`ReAlloc`), so a push loop grows the
// buffer in place — no copy, no leak — when it is the heap's last allocation. Pure
// HolyC, identical on the interpreter and every backend. Include with
// `#include <vec.hc>`.
//
// The caller owns the `Vec` struct; methods take `Vec *`. `VecInit(&v, esize)` is
// **required** before use — it records the element size. `Vec` owns its buffer: copy
// with `VecClone` (not `=`), free with `VecFree`.

#include <mem.hc>
#include <sort.hc>   // VecSort/VecBSearch wrap the generic Sort/BSearch

class Vec {
  U8 *data;    // heap buffer of `cap * esize` bytes, or NULL before first allocation
  I64 len;     // number of elements in use
  I64 cap;     // allocated capacity, in elements
  I64 esize;   // element size in bytes
}

// Initialise an empty `Vec` whose elements are `esize` bytes each.
U0 VecInit(Vec *v, I64 esize)
{
  v->data = NULL;
  v->len = 0;
  v->cap = 0;
  v->esize = esize;
}

// Release the buffer and return to the empty state (keeps the element size).
U0 VecFree(Vec *v)
{
  if (v->data) Free(v->data);
  v->data = NULL;
  v->len = 0;
  v->cap = 0;
}

// Drop all elements but keep the buffer (so refilling won't reallocate).
U0 VecClear(Vec *v) { v->len = 0; }

// Ensure room for at least `need` elements, growing geometrically. `ReAlloc` extends
// in place when the buffer is the heap's last allocation.
U0 VecReserve(Vec *v, I64 need)
{
  if (v->cap >= need) return;
  I64 cap = v->cap;
  if (cap < 4) cap = 4;
  while (cap < need) cap *= 2;
  v->data = ReAlloc(v->data, v->cap * v->esize, cap * v->esize);
  v->cap = cap;
}

// Pointer to element `i` (no bounds check — caller keeps 0 <= i < len). Cast it to
// the element type to read or write: `*(I64 *)VecAt(&v, i)`.
U8 *VecAt(Vec *v, I64 i) { return &v->data[i * v->esize]; }

// Append one element and return a pointer to its new (uninitialised) slot; the caller
// writes the value through it: `*(I64 *)VecPush(&v) = 42;`.
U8 *VecPush(Vec *v)
{
  VecReserve(v, v->len + 1);
  U8 *slot = &v->data[v->len * v->esize];
  v->len++;
  return slot;
}

// Remove the last element and return a pointer to it (valid until the next push;
// caller ensures the Vec is non-empty).
U8 *VecPop(Vec *v)
{
  v->len--;
  return &v->data[v->len * v->esize];
}

// Deep-copy `src` into a fresh `dst` (the correct way to duplicate a `Vec`).
U0 VecClone(Vec *dst, Vec *src)
{
  VecInit(dst, src->esize);
  VecReserve(dst, src->len);
  MemCpy(dst->data, src->data, src->len * src->esize);
  dst->len = src->len;
}

// Sort the elements in place by `cmp` (a `<sort.hc>` comparator over element pointers).
U0 VecSort(Vec *v, I64 (*cmp)(U8 *, U8 *))
{
  Sort(v->data, v->len, v->esize, cmp);
}

// Binary-search a sorted `Vec` for `key` (a pointer to a key element). Returns the
// element index, or -1 if absent.
I64 VecBSearch(Vec *v, U8 *key, I64 (*cmp)(U8 *, U8 *))
{
  U8 *p = BSearch(key, v->data, v->len, v->esize, cmp);
  if (p == NULL) return -1;
  return (p - v->data) / v->esize;
}

#endif
