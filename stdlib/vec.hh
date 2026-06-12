#ifndef _VEC_HH
#define _VEC_HH
// vec.hh — `Vec<T>`, an owning, growable typed array.
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
// It works for scalar, pointer, and class element types. It is built on `<stdlib.hh>`'s
// `ReAlloc` (a push loop grows the buffer in place) and `Sort`/`BSearch` (`VecSort`/
// `VecBSearch`), plus `<string.hh>`'s `MemCpy` for `VecClone`. The implementation is pure
// HolyC and behaves identically on the interpreter and every backend. Include with
// `#include <vec.hh>`.
//
// The caller owns the `Vec` struct, and `VecInit(&v)` is required before use. A `Vec`
// owns its buffer: copy it with `VecClone` (not `=`), and free it with `VecFree`.


#include <string.hh>
#include <stdlib.hh>

public class Vec<type T> {
  T  *data;   // heap buffer of `cap` elements, or NULL before the first allocation
  I64 len;    // elements in use
  I64 cap;    // allocated capacity, in elements
}

// The generic `Vec` operations are templates the parser must register *before* any use
// site (generics are define-before-use), so they cannot be deferred to the end like an
// ordinary `.hc` implementation. They live in `<vec.hc>`, included at the foot of this
// header — the C++ template-header idiom — so they are parsed eagerly with these
// declarations. The prototypes are listed here for the reader; the bodies are in the
// implementation file:
//
//   U0  VecInit   <type T>(Vec<T> *v);
//   U0  VecFree   <type T>(Vec<T> *v);
//   U0  VecClear  <type T>(Vec<T> *v);
//   I64 VecLen    <type T>(Vec<T> *v);
//   U0  VecReserve<type T>(Vec<T> *v, I64 need);
//   U0  VecPush   <type T>(Vec<T> *v, T x);
//   T   VecAt     <type T>(Vec<T> *v, I64 i);
//   T  *VecRef    <type T>(Vec<T> *v, I64 i);
//   U0  VecSet    <type T>(Vec<T> *v, I64 i, T x);
//   T   VecPop    <type T>(Vec<T> *v);
//   U0  VecClone  <type T>(Vec<T> *dst, Vec<T> *src);
//   U0  VecSort   <type T>(Vec<T> *v, I64 (*cmp)(T *, T *));
//   I64 VecBSearch<type T>(Vec<T> *v, T *key, I64 (*cmp)(T *, T *));

// Collect every environment entry ("KEY=VALUE", a `U8 *`) into `out`, a `Vec<U8 *>`
// initialised here, in the OS's order. Read an entry with `VecAt(&out, i)`. This builds
// a `Vec` from the implicit `envp` (C's `environ`), so it lives here next to `Vec`
// rather than in `<stdlib.hh>` (where the scalar `Getenv` is). The entries point into
// the process environment and are read-only. `VecFree(&out)` frees the Vec's own buffer,
// not the entries.
public U0 Environ(Vec<U8 *> *out);

#include <vec.hc>

#endif
