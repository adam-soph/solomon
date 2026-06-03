// vector.hc — a growable dynamic array of I64 on the heap. Demonstrates typed
// MAlloc, MemCpy to copy on growth, Free, pointer indexing, and a class threaded
// through functions by pointer so mutations persist.

#include <mem.hc>   // MemCpy

class Vec {
  I64 *data;
  I64 len;
  I64 cap;
}

U0 VecInit(Vec *v, I64 cap) {
  v->data = MAlloc(cap * 8);
  v->len = 0;
  v->cap = cap;
}

// Append, doubling the backing buffer (and copying it over) when full.
U0 VecPush(Vec *v, I64 x) {
  if (v->len == v->cap) {
    I64 newcap = v->cap * 2;
    I64 *bigger = MAlloc(newcap * 8);
    MemCpy(bigger, v->data, v->len * 8);
    Free(v->data);
    v->data = bigger;
    v->cap = newcap;
  }
  v->data[v->len] = x;
  v->len++;
}

I64 VecSum(Vec *v) {
  I64 s = 0;
  I64 i;
  for (i = 0; i < v->len; i++)
    s += v->data[i];
  return s;
}

U0 Main() {
  Vec v;
  VecInit(&v, 2); // start small to force several growths
  I64 i;
  for (i = 1; i <= 10; i++)
    VecPush(&v, i * i);
  "len=%d cap=%d\n", v.len, v.cap;
  "first=%d last=%d sum=%d\n", v.data[0], v.data[9], VecSum(&v);
  Free(v.data);
}

Main;
