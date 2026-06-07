#ifndef _MEM_HC
#define _MEM_HC
// mem.hc — raw memory operations (the `<string.h>` `mem*` family) plus the heap
// helpers `CAlloc` and `ReAlloc`.
//
// Everything here is pure HolyC built on the irreducible heap primitives
// (`MAlloc`/`Free`/`HeapExtend`), so it computes identically on the interpreter and
// every native backend. Include with `#include <mem.hc>`; the guard above makes that
// idempotent.
//
// `MAlloc` and `Free` are the universal allocator pair. They live in the implicit
// prelude (`<builtin.hc>`) and need no `#include`. The two advanced heap primitives
// below are intrinsics declared here, with the compiler as their implementation.
// `HeapExtend(ptr, old, new)` grows the last block in place, or returns NULL.
// `MSize(ptr)` returns the block's requested size; the allocator prepends an 8-byte
// header when a program uses `MSize`.

public U8 *HeapExtend(U8 *ptr, I64 old, I64 newsz);
public I64 MSize(U8 *ptr);

public U8 *MemCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = src[i]; i++; }
  return dst;
}

// Overlap-safe: copy backwards when dst is above src within the same region.
public U8 *MemMove(U8 *dst, U8 *src, I64 n)
{
  if (dst <= src) {
    I64 i = 0;
    while (i < n) { dst[i] = src[i]; i++; }
  } else {
    I64 i = n;
    while (i > 0) { i--; dst[i] = src[i]; }
  }
  return dst;
}

public U8 *MemSet(U8 *dst, I64 c, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = c; i++; }
  return dst;
}

// Allocate `n` zero-filled bytes (HolyC `CAlloc`). `MAlloc` returns uninitialised
// memory, so this zeroes it explicitly. That is correct on every target, since the
// hosted libc heap is not zeroed either.
public U8 *CAlloc(I64 n)
{
  U8 *p = MAlloc(n);
  if (p) MemSet(p, 0, n);
  return p;
}

// Sign-normalised to -1/0/1 (bytes compared unsigned), like the old builtin.
public I64 MemCmp(U8 *a, U8 *b, I64 n)
{
  I64 i = 0;
  while (i < n) {
    if (a[i] != b[i]) { if (a[i] < b[i]) return -1; return 1; }
    i++;
  }
  return 0;
}

// First byte equal to `c` in buf[0..n], or NULL (memchr).
public U8 *MemFind(U8 *buf, I64 c, I64 n)
{
  U8 ch = c;
  I64 i = 0;
  while (i < n) { if (buf[i] == ch) return &buf[i]; i++; }
  return NULL;
}

// First occurrence of needle[0..nlen] in hay[0..hlen], or NULL (memmem). An empty
// needle matches at the start.
public U8 *MemSearch(U8 *hay, I64 hlen, U8 *needle, I64 nlen)
{
  if (nlen <= 0) return hay;
  if (nlen > hlen) return NULL;
  I64 i = 0;
  while (i <= hlen - nlen) {
    I64 j = 0;
    while (j < nlen && hay[i + j] == needle[j]) j++;
    if (j == nlen) return &hay[i];
    i++;
  }
  return NULL;
}

// Resize the block at `p` (originally `oldsz` bytes) to `newsz`, preserving the first
// min(oldsz, newsz) bytes. Returns the block, which may have moved. A bump allocator
// extends in place via `HeapExtend`, with no copy, when `p` is its last block.
// Otherwise (and always on the libc and interpreter heaps) it allocates a new block,
// copies, and frees the old one. `Free` reclaims on libc and is a no-op on the bump
// allocators. `p == NULL` behaves like `MAlloc(newsz)`.
public U8 *ReAlloc(U8 *p, I64 oldsz, I64 newsz)
{
  if (!p) return MAlloc(newsz);
  U8 *grown = HeapExtend(p, oldsz, newsz);
  if (grown) return grown;
  U8 *q = MAlloc(newsz);
  I64 n = oldsz;
  if (newsz < n) n = newsz;
  MemCpy(q, p, n);
  Free(p);
  return q;
}

#endif
