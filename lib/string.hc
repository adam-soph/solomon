#ifndef _STRING_HC
#define _STRING_HC
// string.hc — the solomon standard string library.
//
// Two layers, both pure HolyC (no builtins beyond `MAlloc`/`Free`), so they compute
// identically on the interpreter and every native backend:
//
//   1. C-style primitives over raw byte pointers (`U8 *`): `StrLen`/`StrCmp`/…,
//      number conversion, the `Mem*` ops, and the ASCII ctype predicates. Byte
//      values are `U8` (unsigned), so `<`/`>` match libc's `strcmp` family.
//   2. `Str` — an owning, growable string object built on layer 1.
//
// Include with `#include <string.hc>` (idempotent — the guard above makes a second
// include a no-op rather than a redefinition error).

// ============================================================================
// 1. C-style primitives over raw byte pointers (`U8 *`).
// ============================================================================

// --- length & comparison (sign-normalised to -1/0/1) ---

I64 StrLen(U8 *s) { I64 n = 0; while (s[n]) n++; return n; }

I64 StrCmp(U8 *a, U8 *b)
{
  while (*a && *a == *b) {
      a++; b++;
  }
  if (*a < *b) return -1;
  if (*a > *b) return 1;
  return 0;
}

I64 StrNCmp(U8 *a, U8 *b, I64 n)
{
  while (n > 0 && *a && *a == *b) { a++; b++; n--; }
  if (n == 0) return 0;
  if (*a < *b) return -1;
  if (*a > *b) return 1;
  return 0;
}

// --- copy & concatenate (return dst) ---

U8 *StrCpy(U8 *dst, U8 *src)
{
  I64 i = 0;
  while (src[i]) { dst[i] = src[i]; i++; }
  dst[i] = 0;
  return dst;
}

// Copy up to n chars; NUL-pad to exactly n (no terminator past n), like strncpy.
U8 *StrNCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n && src[i]) { dst[i] = src[i]; i++; }
  while (i < n) { dst[i] = 0; i++; }
  return dst;
}

U8 *StrCat(U8 *dst, U8 *src)
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
U8 *StrFind(U8 *hay, U8 *needle)
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
U8 *StrChr(U8 *s, I64 c)
{
  U8 ch = c;
  while (TRUE) {
    if (*s == ch) return s;
    if (!*s) return NULL;
    s++;
  }
}

U8 *StrLastChr(U8 *s, I64 c)
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
I64 StrInSet(U8 c, U8 *set)
{
  while (*set) { if (*set == c) return 1; set++; }
  return 0;
}

// Length of the initial run of str whose chars are in / not in `set`.
I64 StrSpn(U8 *s, U8 *set)
{
  I64 n = 0;
  while (s[n] && StrInSet(s[n], set)) n++;
  return n;
}

I64 StrCSpn(U8 *s, U8 *set)
{
  I64 n = 0;
  while (s[n] && !StrInSet(s[n], set)) n++;
  return n;
}

// --- in-place transforms (return str) ---

U8 *StrToUpper(U8 *s)
{
  I64 i = 0;
  while (s[i]) { U8 c = s[i]; if (c >= 'a' && c <= 'z') s[i] = c - 32; i++; }
  return s;
}

U8 *StrToLower(U8 *s)
{
  I64 i = 0;
  while (s[i]) { U8 c = s[i]; if (c >= 'A' && c <= 'Z') s[i] = c + 32; i++; }
  return s;
}

U8 *StrRev(U8 *s)
{
  I64 i = 0, j = StrLen(s) - 1;
  while (i < j) { U8 t = s[i]; s[i] = s[j]; s[j] = t; i++; j--; }
  return s;
}

// --- number <-> string ---

// Parse a base-10 integer like atoll: skip leading whitespace, optional sign, then
// digits; wraps on overflow.
I64 StrToI64(U8 *s)
{
  while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\f' || *s == '\r') s++;
  I64 neg = 0;
  if (*s == '-') { neg = 1; s++; }
  else if (*s == '+') s++;
  I64 n = 0;
  while (*s >= '0' && *s <= '9') { n = n * 10 + (*s - '0'); s++; }
  if (neg) return -n;
  return n;
}

// Format n as decimal into buf (matching "%d"); return buf. Digits are extracted in
// the non-positive domain so I64 min doesn't overflow on negation.
U8 *I64ToStr(I64 n, U8 *buf)
{
  U8 tmp[24];
  I64 i = 0, neg = n < 0;
  if (!neg) n = -n;
  tmp[i++] = '0' - (n % 10);
  n /= 10;
  while (n != 0) {
      tmp[i++] = '0' - (n % 10);
      n /= 10;
  }
  I64 j = 0;
  if (neg) buf[j++] = '-';
  while (i > 0) { i--; buf[j++] = tmp[i]; }
  buf[j] = 0;
  return buf;
}

// --- integer math ---

I64 Abs(I64 n) { if (n < 0) return -n; return n; }
I64 Sign(I64 n) { return (n > 0) - (n < 0); }

// --- memory ------------------------------------------------------------------

U8 *MemCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = src[i]; i++; }
  return dst;
}

// Overlap-safe: copy backwards when dst is above src within the same region.
U8 *MemMove(U8 *dst, U8 *src, I64 n)
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

U8 *MemSet(U8 *dst, I64 c, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = c; i++; }
  return dst;
}

// Sign-normalised to -1/0/1 (bytes compared unsigned), like the old builtin.
I64 MemCmp(U8 *a, U8 *b, I64 n)
{
  I64 i = 0;
  while (i < n) {
    if (a[i] != b[i]) { if (a[i] < b[i]) return -1; return 1; }
    i++;
  }
  return 0;
}

// First byte equal to `c` in buf[0..n], or NULL (memchr).
U8 *MemFind(U8 *buf, I64 c, I64 n)
{
  U8 ch = c;
  I64 i = 0;
  while (i < n) { if (buf[i] == ch) return &buf[i]; i++; }
  return NULL;
}

// First occurrence of needle[0..nlen] in hay[0..hlen], or NULL (memmem). An empty
// needle matches at the start.
U8 *MemSearch(U8 *hay, I64 hlen, U8 *needle, I64 nlen)
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

// Resize the block at `p` (originally `oldsz` bytes) to `newsz`, preserving the
// first min(oldsz, newsz) bytes; returns the (possibly moved) block. A bump
// allocator extends in place when `p` is its last block (no copy, via `HeapExtend`);
// otherwise — and always on the libc/interp heaps — it allocates a new block, copies,
// and frees the old one (`Free` reclaims on libc; a no-op on the bump allocators).
// `p == NULL` behaves like `MAlloc(newsz)`.
U8 *ReAlloc(U8 *p, I64 oldsz, I64 newsz)
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

// --- character classification (ASCII, "C" locale) ----------------------------

I64 ToUpper(I64 c) { if (c >= 'a' && c <= 'z') return c - 32; return c; }
I64 ToLower(I64 c) { if (c >= 'A' && c <= 'Z') return c + 32; return c; }

I64 IsDigit(I64 c)  { return c >= '0' && c <= '9'; }
I64 IsUpper(I64 c)  { return c >= 'A' && c <= 'Z'; }
I64 IsLower(I64 c)  { return c >= 'a' && c <= 'z'; }
I64 IsAlpha(I64 c)  { return IsUpper(c) || IsLower(c); }
I64 IsAlNum(I64 c)  { return IsAlpha(c) || IsDigit(c); }
I64 IsXDigit(I64 c) { return IsDigit(c) || (c >= 'A' && c <= 'F') || (c >= 'a' && c <= 'f'); }
I64 IsSpace(I64 c)  { return (c >= 0x09 && c <= 0x0d) || c == ' '; }  // \t\n\v\f\r space
I64 IsBlank(I64 c)  { return c == '\t' || c == ' '; }
I64 IsCntrl(I64 c)  { return (c >= 0 && c <= 0x1f) || c == 0x7f; }
I64 IsPrint(I64 c)  { return c >= ' ' && c <= 0x7e; }
I64 IsGraph(I64 c)  { return c >= 0x21 && c <= 0x7e; }
I64 IsPunct(I64 c)  { return IsGraph(c) && !IsAlNum(c); }

// ============================================================================
// 2. `Str` — an owning, growable string object (heap buffer, built on layer 1).
// ============================================================================
//
// The caller owns the `Str` struct (stack or heap); methods take `Str *` and mutate
// it in place. The byte buffer lives on the heap (`MAlloc`/`Free`) and is always
// kept NUL-terminated (`ptr[len] == 0`), so `StrCStr` hands it straight to any
// `U8 *` primitive or to `"%s"`. A zero-filled `Str` (`Str s;` — locals are
// zero-initialised) is already a valid empty string, so `StrInit` is only needed to
// reset a used one. `Str` owns its buffer: copy it with `StrClone`, not `=` (a plain
// assignment would alias the buffer and double-free). Free it with `StrFree`.

class Str {
  U8 *ptr;   // NUL-terminated heap buffer, or NULL before the first allocation
  I64 len;   // byte length (excludes the NUL)
  I64 cap;   // capacity in bytes (excludes the NUL slot)
}

// Reset to the empty state without freeing (use on a fresh, non-zeroed struct).
U0 StrInit(Str *s) { s->ptr = NULL; s->len = 0; s->cap = 0; }

// Release the buffer and return to the empty state.
U0 StrFree(Str *s)
{
  if (s->ptr) Free(s->ptr);
  s->ptr = NULL;
  s->len = 0;
  s->cap = 0;
}

// Empty the string but keep the buffer (so refilling won't reallocate).
U0 StrClear(Str *s) { s->len = 0; if (s->ptr) s->ptr[0] = 0; }

// Ensure room for at least `need` bytes (plus the NUL), growing geometrically.
// Uses `ReAlloc` so a tight push loop extends the buffer in place (no copy, no
// leak) when it is the heap's last allocation. The old/new block sizes are `cap + 1`
// (the buffer always reserves one extra byte for the NUL).
U0 StrReserve(Str *s, I64 need)
{
  if (s->cap >= need) return;
  I64 cap = s->cap;
  if (cap < 8) cap = 8;
  while (cap < need) cap *= 2;
  s->ptr = ReAlloc(s->ptr, s->cap + 1, cap + 1);
  s->ptr[s->len] = 0;
  s->cap = cap;
}

// Append one byte.
U0 StrPushChar(Str *s, I64 c)
{
  StrReserve(s, s->len + 1);
  s->ptr[s->len] = c;
  s->len++;
  s->ptr[s->len] = 0;
}

// Append a C string.
U0 StrPushCStr(Str *s, U8 *c)
{
  I64 n = StrLen(c);
  StrReserve(s, s->len + n);
  MemCpy(&s->ptr[s->len], c, n);
  s->len += n;
  s->ptr[s->len] = 0;
}

// Append another `Str`. Reserve up front so no reallocation happens mid-copy —
// that keeps `StrPushStr(&s, &s)` (append self) safe, since otherwise the source
// pointer could be freed by a grow before it is read.
U0 StrPushStr(Str *s, Str *other)
{
  StrReserve(s, s->len + other->len);
  StrPushCStr(s, StrCStr(other));
}

// Replace the contents with a C string.
U0 StrSet(Str *s, U8 *c) { StrClear(s); StrPushCStr(s, c); }

// Borrow the NUL-terminated bytes (never NULL — an empty string reads as "").
U8 *StrCStr(Str *s) { if (s->ptr) return s->ptr; return ""; }

// Whether two strings are byte-equal.
I64 StrEq(Str *a, Str *b) { return StrCmp(StrCStr(a), StrCStr(b)) == 0; }

// Deep-copy `src` into a fresh `dst` (the correct way to duplicate a `Str`).
U0 StrClone(Str *dst, Str *src) { StrInit(dst); StrPushStr(dst, src); }

// In-place ASCII case / reversal, reusing the `U8 *` primitives.
U0 StrMakeUpper(Str *s) { if (s->ptr) StrToUpper(s->ptr); }
U0 StrMakeLower(Str *s) { if (s->ptr) StrToLower(s->ptr); }
U0 StrReverse(Str *s)   { if (s->ptr) StrRev(s->ptr); }

// Strip leading and trailing ASCII whitespace in place (`lo`/`hi`, since `start`/
// `end` are reserved switch keywords).
U0 StrTrim(Str *s)
{
  if (!s->ptr) return;
  I64 lo = 0;
  while (lo < s->len && IsSpace(s->ptr[lo])) lo++;
  I64 hi = s->len;
  while (hi > lo && IsSpace(s->ptr[hi - 1])) hi--;
  I64 n = hi - lo;
  if (lo > 0) MemMove(s->ptr, &s->ptr[lo], n);
  s->len = n;
  s->ptr[n] = 0;
}

#endif
