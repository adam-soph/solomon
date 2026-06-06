#ifndef _CSTR_HC
#define _CSTR_HC
// cstr.hc — C-style primitives over NUL-terminated byte strings (the `<string.h>`
// `str*` family) and number <-> string conversion. (The `Abs`/`Sign` integer
// helpers moved to `<math.hc>`, next to the float `Fabs`/`FMin`/… and the other
// integer helpers.)
//
// Pure HolyC over raw byte pointers (`U8 *`). Byte values are `U8` (unsigned), so the
// `<`/`>` comparisons are unsigned — matching libc's `strcmp` family. Include with
// `#include <cstr.hc>`.
//
// (`F64ToStr`, a `StrPrint("%g")` wrapper, lives in `<fmt.hc>` next to the rest of the
// printf machinery — keeping `cstr.hc` free of a dependency on `fmt.hc`, which now
// includes the printf core that depends back on these string primitives.)

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

// Stock comparator for a `U8 *` (string-pointer) element: a `Sort`/`VecSort`/
// `HmapSortKeys` over a `Vec<U8 *>` hands the comparator *pointers to elements*, i.e.
// `U8 **`, so dereference once before comparing the strings.
I64 CmpStr(U8 *a, U8 *b) { return StrCmp(*(U8 **)a, *(U8 **)b); }

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

#endif
