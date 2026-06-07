#ifndef _STRING_HC
#define _STRING_HC
// string.hc — C `<string.h>`: operations on NUL-terminated byte strings (the `str*`
// family) and on raw memory blocks (the `mem*` family).
//
// Everything here is pure HolyC over raw byte pointers (`U8 *`). Byte values are `U8`
// (unsigned), so the `<` and `>` comparisons are unsigned, matching libc's `strcmp`
// family. Include with `#include <string.hc>`.
//
// The number <-> string conversions (`StrToI64`/`I64ToStr`/`StrToF64`/`F64ToStr`) are
// C's `atoi`/`atof` family, so they live in `<stdlib.hc>`, not here. The allocation
// helpers built on the `mem*` family (`CAlloc`/`ReAlloc`) also live in `<stdlib.hc>`.

// --- length & comparison (sign-normalised to -1/0/1) ---

public I64 StrLen(U8 *s) { I64 n = 0; while (s[n]) n++; return n; }

public I64 StrCmp(U8 *a, U8 *b)
{
  while (*a && *a == *b) {
      a++; b++;
  }
  if (*a < *b) return -1;
  if (*a > *b) return 1;
  return 0;
}

// Stock comparator for a `U8 *` (string-pointer) element. `Sort`/`VecSort`/
// `HmapSortKeys` over a `Vec<U8 *>` hand the comparator *pointers to elements*, i.e.
// `U8 **`, so it dereferences once before comparing the strings.
public I64 CmpStr(U8 **a, U8 **b) { return StrCmp(*a, *b); }

public I64 StrNCmp(U8 *a, U8 *b, I64 n)
{
  while (n > 0 && *a && *a == *b) { a++; b++; n--; }
  if (n == 0) return 0;
  if (*a < *b) return -1;
  if (*a > *b) return 1;
  return 0;
}

// --- copy & concatenate (return dst) ---

public U8 *StrCpy(U8 *dst, U8 *src)
{
  I64 i = 0;
  while (src[i]) { dst[i] = src[i]; i++; }
  dst[i] = 0;
  return dst;
}

// Copy up to n chars; NUL-pad to exactly n (no terminator past n), like strncpy.
public U8 *StrNCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n && src[i]) { dst[i] = src[i]; i++; }
  while (i < n) { dst[i] = 0; i++; }
  return dst;
}

public U8 *StrCat(U8 *dst, U8 *src)
{
  I64 d = StrLen(dst), i = 0;
  while (src[i]) {
      dst[d + i] = src[i];
      i++;
  }
  dst[d + i] = 0;
  return dst;
}

// --- search ---

// First occurrence of needle in haystack, or NULL. An empty needle matches at the
// start (strstr).
public U8 *StrFind(U8 *hay, U8 *needle)
{
  if (!*needle) return hay;
  while (*hay) {
    I64 i = 0;
    while (needle[i] && hay[i] == needle[i]) i++;
    if (!needle[i]) return hay;
    hay++;
  }
  return NULL;
}

// First / last `c` in str. The terminating NUL counts, so c == 0 finds it.
public U8 *StrChr(U8 *s, I64 c)
{
  U8 ch = c;
  while (TRUE) {
    if (*s == ch) return s;
    if (!*s) return NULL;
    s++;
  }
}

public U8 *StrLastChr(U8 *s, I64 c)
{
  U8 ch = c;
  U8 *last = NULL;
  while (TRUE) {
    if (*s == ch) last = s;
    if (!*s) return last;
    s++;
  }
}

// Is byte c one of the NUL-terminated `set`'s characters?
public I64 StrInSet(U8 c, U8 *set)
{
  while (*set) { if (*set == c) return 1; set++; }
  return 0;
}

// Length of the initial run of str whose chars are in / not in `set`.
public I64 StrSpn(U8 *s, U8 *set)
{
  I64 n = 0;
  while (s[n] && StrInSet(s[n], set)) n++;
  return n;
}

public I64 StrCSpn(U8 *s, U8 *set)
{
  I64 n = 0;
  while (s[n] && !StrInSet(s[n], set)) n++;
  return n;
}

// --- in-place transforms (return str) ---

public U8 *StrToUpper(U8 *s)
{
  I64 i = 0;
  while (s[i]) { U8 c = s[i]; if (c >= 'a' && c <= 'z') s[i] = c - 32; i++; }
  return s;
}

public U8 *StrToLower(U8 *s)
{
  I64 i = 0;
  while (s[i]) { U8 c = s[i]; if (c >= 'A' && c <= 'Z') s[i] = c + 32; i++; }
  return s;
}

public U8 *StrRev(U8 *s)
{
  I64 i = 0, j = StrLen(s) - 1;
  while (i < j) { U8 t = s[i]; s[i] = s[j]; s[j] = t; i++; j--; }
  return s;
}

// --- raw memory (the `mem*` family) ------------------------------------------

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

#endif
