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
// C's `atoi`/`atof` family, so they live in `<stdlib.hc>`, not here. The generic
// allocators (`CAlloc`/`ReAlloc`) also live in `<stdlib.hc>`; the string duplicators
// `StrDup`/`StrNDup` (which `MAlloc` a copy) are `<string.h>` members and live here.

// --- length & comparison (sign-normalised to -1/0/1) ---

public I64 StrLen(U8 *s) { I64 n = 0; while (s[n]) n++; return n; }

// Length of `s`, but scanning at most `n` bytes (strnlen): stops at a NUL or after n.
public I64 StrNLen(U8 *s, I64 n) { I64 i = 0; while (i < n && s[i]) i++; return i; }

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

// ASCII lowercase of one byte (private; avoids a `<ctype.hc>` dependency).
I64 LowerByte(U8 c) { if (c >= 'A' && c <= 'Z') return c + 32; return c; }

// Case-insensitive compares (ASCII), sign-normalised to -1/0/1, like POSIX
// strcasecmp / strncasecmp.
public I64 StrCaseCmp(U8 *a, U8 *b)
{
  while (*a && LowerByte(*a) == LowerByte(*b)) { a++; b++; }
  U8 ca = LowerByte(*a), cb = LowerByte(*b);
  if (ca < cb) return -1;
  if (ca > cb) return 1;
  return 0;
}

public I64 StrNCaseCmp(U8 *a, U8 *b, I64 n)
{
  while (n > 0 && *a && LowerByte(*a) == LowerByte(*b)) { a++; b++; n--; }
  if (n == 0) return 0;
  U8 ca = LowerByte(*a), cb = LowerByte(*b);
  if (ca < cb) return -1;
  if (ca > cb) return 1;
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

// Append up to n chars of src, then NUL-terminate (always), like strncat.
public U8 *StrNCat(U8 *dst, U8 *src, I64 n)
{
  I64 d = StrLen(dst), i = 0;
  while (i < n && src[i]) { dst[d + i] = src[i]; i++; }
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

// First char of s that is in `set`, or NULL (strpbrk).
public U8 *StrPBrk(U8 *s, U8 *set)
{
  while (*s) { if (StrInSet(*s, set)) return s; s++; }
  return NULL;
}

// Tokenise `s` by any char in `delim`, reentrantly (strtok_r). The first call passes the
// string; later calls pass NULL to continue. `*save` carries the position between calls.
// Writes a NUL over each delimiter, so `s` is modified in place. Returns the next token,
// or NULL when there are none left.
public U8 *StrTokR(U8 *s, U8 *delim, U8 **save)
{
  if (s == NULL) s = *save;
  while (*s && StrInSet(*s, delim)) s++;  // skip leading delimiters
  if (!*s) { *save = s; return NULL; }    // no more tokens
  U8 *tok = s;
  while (*s && !StrInSet(*s, delim)) s++; // run to the next delimiter
  if (*s) { *s = 0; s++; }                // terminate this token, step past the delimiter
  *save = s;
  return tok;
}

// Non-reentrant strtok: like StrTokR but with private state, so later calls pass NULL to
// continue the same string. Use StrTokR for thread-safe or nested tokenising.
U8 *StrTokState = NULL;
public U8 *StrTok(U8 *s, U8 *delim) { return StrTokR(s, delim, &StrTokState); }

// strsep: split *`stringp` at the first char in `delim`. Returns the token before that
// delimiter and advances *`stringp` just past it (or to NULL when none remains). Unlike
// StrTok it does NOT merge adjacent delimiters, so it yields empty tokens — the right tool
// for fixed fields (CSV, `key=value`). Writes a NUL over the delimiter (modifies the
// string). Returns NULL only once *`stringp` is already NULL.
public U8 *StrSep(U8 **stringp, U8 *delim)
{
  U8 *s = *stringp;
  if (s == NULL) return NULL;
  U8 *p = s;
  while (*p && !StrInSet(*p, delim)) p++;
  if (*p) { *p = 0; *stringp = p + 1; } // terminate token, step past the delimiter
  else *stringp = NULL;                 // no delimiter: this is the final token
  return s;
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

// --- allocate & duplicate (the caller owns the result; `Free` it) -------------

// strdup: a fresh heap copy of the NUL-terminated string `s` (including its NUL).
public U8 *StrDup(U8 *s)
{
  I64 n = StrLen(s);
  U8 *p = MAlloc(n + 1);
  MemCpy(p, s, n + 1);
  return p;
}

// strndup: a fresh heap copy of at most n chars of `s`, always NUL-terminated.
public U8 *StrNDup(U8 *s, I64 n)
{
  I64 len = 0;
  while (len < n && s[len]) len++;
  U8 *p = MAlloc(len + 1);
  MemCpy(p, s, len);
  p[len] = 0;
  return p;
}

// --- raw memory (the `mem*` family) ------------------------------------------

public U8 *MemCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = src[i]; i++; }
  return dst;
}

// Copy bytes until `c` has been copied or n bytes are done. Returns the byte just past
// the copied `c` in dst, or NULL if `c` wasn't among the first n bytes (memccpy).
public U8 *MemCCpy(U8 *dst, U8 *src, I64 c, I64 n)
{
  U8 ch = c;
  I64 i = 0;
  while (i < n) {
    dst[i] = src[i];
    if (src[i] == ch) return &dst[i + 1];
    i++;
  }
  return NULL;
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
