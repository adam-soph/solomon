#ifndef _STRING_HC
#define _STRING_HC
// string.hc — implementation (interface in string.hh).

#include <string.hh>
#include <heap.hh>

public I64 StrLen(U8 *s) { I64 n = 0; while (s[n]) n++; return n; }
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
public U8 *StrCpy(U8 *dst, U8 *src)
{
  I64 i = 0;
  while (src[i]) { dst[i] = src[i]; i++; }
  dst[i] = 0;
  return dst;
}
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
public U8 *StrNCat(U8 *dst, U8 *src, I64 n)
{
  I64 d = StrLen(dst), i = 0;
  while (i < n && src[i]) { dst[d + i] = src[i]; i++; }
  dst[d + i] = 0;
  return dst;
}
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
public I64 StrInSet(U8 c, U8 *set)
{
  while (*set) { if (*set == c) return 1; set++; }
  return 0;
}
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
public U8 *StrPBrk(U8 *s, U8 *set)
{
  while (*s) { if (StrInSet(*s, set)) return s; s++; }
  return NULL;
}
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
public U8 *StrDup(U8 *s)
{
  I64 n = StrLen(s);
  U8 *p = MAlloc(n + 1);
  MemCpy(p, s, n + 1);
  return p;
}
public U8 *StrNDup(U8 *s, I64 n)
{
  I64 len = 0;
  while (len < n && s[len]) len++;
  U8 *p = MAlloc(len + 1);
  MemCpy(p, s, len);
  p[len] = 0;
  return p;
}
public U8 *MemCpy(U8 *dst, U8 *src, I64 n)
{
  I64 i = 0;
  while (i < n) { dst[i] = src[i]; i++; }
  return dst;
}
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
public I64 MemCmp(U8 *a, U8 *b, I64 n)
{
  I64 i = 0;
  while (i < n) {
    if (a[i] != b[i]) { if (a[i] < b[i]) return -1; return 1; }
    i++;
  }
  return 0;
}
public U8 *MemFind(U8 *buf, I64 c, I64 n)
{
  U8 ch = c;
  I64 i = 0;
  while (i < n) { if (buf[i] == ch) return &buf[i]; i++; }
  return NULL;
}
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
