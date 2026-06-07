#ifndef _CSTR_HC
#define _CSTR_HC
// cstr.hc — C-style operations on NUL-terminated byte strings, the `<string.h>`
// `str*` family, plus number <-> string conversion.
//
// Everything here is pure HolyC over raw byte pointers (`U8 *`). Byte values are `U8`
// (unsigned), so the `<` and `>` comparisons are unsigned, matching libc's `strcmp`
// family. Include with `#include <cstr.hc>`.
//
// The `Abs` and `Sign` integer helpers moved to `<math.hc>`, next to the float
// `Fabs`/`FMin`/… and the other integer helpers.
//
// The `F64` <-> string pair (`StrToF64` / `F64ToStr`) lives here too, alongside the
// integer `StrToI64` / `I64ToStr`. They pull in `<bignum.hc>` (the correctly-rounded
// `atof`) and `<fltfmt.hc>` (the float formatter `F64ToStr` reuses); neither depends
// back on `cstr.hc`, so there is no include cycle (the printf core in `<fmt.hc>` would
// have made one, which is why the `"%g"` wrapper used to live there instead).
#include <bignum.hc>
#include <fltfmt.hc>

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

// --- number <-> string ---

// Parse a base-10 integer, like atoll. Skips leading whitespace and an optional sign,
// then reads digits. Wraps on overflow.
public I64 StrToI64(U8 *s)
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

// Format n as decimal into buf (matching "%d") and return buf. Digits are extracted
// in the non-positive domain, so I64 min doesn't overflow on negation.
public U8 *I64ToStr(I64 n, U8 *buf)
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

// --- F64 <-> string ---
//
// `StrToF64` is a correctly-rounded decimal -> double parser, the freestanding `atof`.
// It returns the IEEE-754 double nearest the decimal value, ties to even, so for the
// normal range it is bit-for-bit a good libc `strtod`. Pure HolyC, so it is the *same*
// on the interpreter and every backend (the freestanding targets have no libc `atof`).
//
// Grammar (like `atof`): optional leading ASCII whitespace, an optional sign, then
// `digits[.digits]` and an optional `e`/`E` exponent. Parsing stops at the first
// character that doesn't fit. With no digits the result is 0.0.
//
// Two paths: a fast Clinger path (<=15 exact digits, power of ten in [-22, 22]: one
// correctly-rounded multiply or divide) and an exact path over the `Bn` big integer
// (build num/den, normalise into [2^52, 2^53) by powers of two, extract the 53-bit
// mantissa by binary long division, round half-to-even). Significands past ~40 digits
// are truncated (sub-ULP); subnormals are best-effort; the whole normal range is exact.
public F64 StrToF64(U8 *str)
{
  U8 *s = str;
  while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r' || *s == '\f' || *s == '\v')
    s++;
  I64 sign = 1;
  if (*s == '-') { sign = -1; s++; }
  else if (*s == '+') s++;

  // Significand digits accumulate into the bignum `m`, and into a fast-path I64 `sig`
  // while it stays exact. `exp10` is the power of ten implied by the decimal point.
  Bn m;
  BnSetU64(&m, 0);
  I64 sig = 0, ndig = 0, exp10 = 0, sawDot = 0, sawDigit = 0;
  while (1) {
    U8 c = *s;
    if (c >= '0' && c <= '9') {
      sawDigit = 1;
      I64 dv = c - '0';
      if (ndig == 0 && dv == 0) {
        // Leading zero: no significant digit yet. A fractional one still lowers
        // the exponent (0.001 -> 1*10^-3); an integer one is a no-op.
        if (sawDot) exp10--;
      } else if (ndig < 40) {
        BnMulAddSmall(&m, 10, dv);
        if (ndig < 18) sig = sig * 10 + dv;
        ndig++;
        if (sawDot) exp10--;
      } else {
        // Past capacity: drop the digit. An integer digit still scales the
        // magnitude, so bump the exponent; a fractional one is sub-significant.
        if (!sawDot) exp10++;
      }
      s++;
    } else if (c == '.' && !sawDot) {
      sawDot = 1;
      s++;
    } else break;
  }
  if (!sawDigit) return 0.0;

  // Optional exponent. Only consumed when at least one exponent digit follows.
  I64 expo = 0;
  if (*s == 'e' || *s == 'E') {
    U8 *p = s + 1;
    I64 esign = 1;
    if (*p == '-') { esign = -1; p++; }
    else if (*p == '+') p++;
    if (*p >= '0' && *p <= '9') {
      while (*p >= '0' && *p <= '9') {
        if (expo < 100000) expo = expo * 10 + (*p - '0');
        p++;
      }
      expo = expo * esign;
      s = p;
    }
  }

  if (ndig == 0) { if (sign < 0) return -0.0; return 0.0; }
  I64 k = exp10 + expo;               // value = m * 10^k

  // Extreme magnitudes overflow or underflow unambiguously. This also guards the
  // bignum size: everything in [-330, 320] fits 72 limbs and is handled exactly.
  I64 magExp = k + ndig;
  if (magExp > 320) { F64 big = 1.0e308; big = big * 100.0; if (sign < 0) return -big; return big; }
  if (magExp < -330) { if (sign < 0) return -0.0; return 0.0; }

  // Fast path (Clinger): an exact significand and an exact power of ten give one
  // correctly-rounded operation. Powers 10^0..10^22 all fit a double's 53-bit
  // mantissa, so they are built exactly by repeated multiply.
  if (ndig <= 15) {
    F64 fs = (F64)sig, r, p = 1.0;
    I64 j;
    if (k >= -22 && k <= 22) {
      I64 ae = k;
      if (ae < 0) ae = -ae;
      for (j = 0; j < ae; j++) p = p * 10.0;   // p = 10^|k|, exact
      if (k >= 0) r = fs * p;
      else r = fs / p;
      if (sign < 0) r = -r;
      return r;
    }
    if (k > 22) {
      // Pull extra factors of ten into the significand while it stays exact, then
      // one multiply by 10^22 (e.g. 1e30 = (1*10^8)*10^22).
      I64 two53 = 9007199254740992, ex = k - 22, s2 = sig, fits = 1;
      while (ex > 0) {
        if (s2 > (two53 - 1) / 10) { fits = 0; ex = 0; }
        else { s2 = s2 * 10; ex--; }
      }
      if (fits) {
        for (j = 0; j < 22; j++) p = p * 10.0;  // p = 10^22, exact
        r = (F64)s2 * p;
        if (sign < 0) r = -r;
        return r;
      }
    }
  }

  // Slow path (exact). value = num/den with both integers.
  Bn num, den, t;
  BnCopy(&num, &m);
  BnSetU64(&den, 1);
  I64 i;
  if (k >= 0) { for (i = 0; i < k; i++) BnMulAddSmall(&num, 10, 0); }
  else { for (i = 0; i < -k; i++) BnMulAddSmall(&den, 10, 0); }

  // Normalise into 2^52 <= num/den < 2^53 by scaling with powers of two.
  I64 e = 0;
  while (1) {
    BnShlBitsTo(&t, &den, 53);
    if (BnCmp(&num, &t) >= 0) { BnShl1(&den); e++; continue; }
    BnShlBitsTo(&t, &den, 52);
    if (BnCmp(&num, &t) < 0) { BnShl1(&num); e--; continue; }
    break;
  }

  // Mantissa = floor(num/den) (53 bits); the remainder stays in `num`.
  I64 mant = 0;
  for (i = 52; i >= 0; i--) {
    BnShlBitsTo(&t, &den, i);
    if (BnCmp(&num, &t) >= 0) { BnSub(&num, &t); mant = mant | (1 << i); }
  }

  // Round half-to-even on the remainder: compare 2*remainder with den.
  BnShl1(&num);
  I64 c = BnCmp(&num, &den);
  if (c > 0 || (c == 0 && (mant & 1))) {
    mant++;
    if (mant == 9007199254740992) { mant = 4503599627370496; e++; } // 2^53 -> 2^52, exp+1
  }

  // result = mant * 2^e (exact powers of two; overflows to +-inf / underflows to 0).
  F64 r = (F64)mant;
  if (e > 0) { while (e > 0) { r = r * 2.0; e--; } }
  else { while (e < 0) { r = r / 2.0; e++; } }
  if (sign < 0) r = -r;
  return r;
}

// Format `v` as the **shortest** decimal string that `StrToF64` parses back to exactly
// `v` — the round-trip inverse of `StrToF64`. It tries increasing `%g` precision; 17
// significant digits always round-trips a `F64`, so the loop terminates. Reuses the
// correctly-rounded float formatter (`FmtFloat`) and `StrToF64` to verify. `buf` should
// be at least 32 bytes. Non-finite values (`inf`/`nan`) don't round-trip through
// `StrToF64`, so they fall through to the 17-digit form. Returns `buf`.
public U8 *F64ToStr(F64 v, U8 *buf)
{
  I64 p, n;
  for (p = 1; p <= 17; p++) {
    n = FmtFloat(buf, v, 'g', 0, 0, p);
    buf[n] = 0;
    if (StrToF64(buf) == v) return buf;
  }
  return buf; // non-finite: `buf` holds the 17-significant-digit form
}

#endif
