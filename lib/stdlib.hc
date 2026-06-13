#ifndef _STDLIB_HC
#define _STDLIB_HC
// stdlib.hc — implementation (interface in stdlib.hh).
//
// The generic `Sort`/`BSearch` prototypes are in <stdlib.hh>; their bodies are below.

#include <stdlib.hh>
#include <string.hh>
#include <stdio.hh>
#include <unistd.hh>   // STDERR (Abort)

// ---- generic sorting & searching (templates; prototypes in <stdlib.hh>) ----

// Swap two elements in place.
U0 SortSwap<type T>(T *a, T *b) { T t = *a; *a = *b; *b = t; }

// Insertion-sort the inclusive range [lo, hi].
U0 SortInsertion<type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *))
{
  I64 i = lo + 1;
  while (i <= hi) {
    I64 j = i;
    while (j > lo && cmp(&base[j - 1], &base[j]) > 0) {
      SortSwap<T>(&base[j - 1], &base[j]);
      j--;
    }
    i++;
  }
}

// Quicksort the inclusive range [lo, hi].
U0 SortQuick<type T>(T *base, I64 lo, I64 hi, I64 (*cmp)(T *, T *))
{
  if (hi - lo < SORT_CUTOFF) {
    if (lo < hi) SortInsertion<T>(base, lo, hi, cmp);
    return;
  }
  // Median-of-three of (lo, mid, hi) ordered into those slots, then the median (now at
  // mid) is moved to hi to serve as the pivot.
  I64 mid = lo + (hi - lo) / 2;
  if (cmp(&base[mid], &base[lo]) < 0) SortSwap<T>(&base[mid], &base[lo]);
  if (cmp(&base[hi], &base[lo]) < 0) SortSwap<T>(&base[hi], &base[lo]);
  if (cmp(&base[hi], &base[mid]) < 0) SortSwap<T>(&base[hi], &base[mid]);
  SortSwap<T>(&base[mid], &base[hi]);

  // Lomuto partition around the pivot at `hi`, which stays put during the loop.
  T *pivot = &base[hi];
  I64 i = lo - 1;
  I64 j = lo;
  while (j < hi) {
    if (cmp(&base[j], pivot) <= 0) {
      i++;
      SortSwap<T>(&base[i], &base[j]);
    }
    j++;
  }
  i++;
  SortSwap<T>(&base[i], &base[hi]);

  SortQuick<T>(base, lo, i - 1, cmp);
  SortQuick<T>(base, i + 1, hi, cmp);
}

U0 Sort<type T>(T *base, I64 n, I64 (*cmp)(T *, T *))
{
  if (n > 1) SortQuick<T>(base, 0, n - 1, cmp);
}

T *BSearch<type T>(T *key, T *base, I64 n, I64 (*cmp)(T *, T *))
{
  I64 lo = 0;
  I64 hi = n - 1;
  while (lo <= hi) {
    I64 mid = lo + (hi - lo) / 2;
    I64 c = cmp(key, &base[mid]);
    if (c == 0) return &base[mid];
    if (c < 0) hi = mid - 1;
    else lo = mid + 1;
  }
  return NULL;
}

// =============================================================================
// `Bn` — a minimal arbitrary-precision nonnegative integer (private to `StrToF64`).
//
// Little-endian base-2^32 limbs: `d[i] < 2^32`, `n` active limbs. The fixed `d[72]`
// holds ~2300 bits, enough for the full normal-double range that the correctly-rounded
// `atof` needs. It provides only what decimal→binary conversion requires: build from
// digits, scale by powers of two, compare, subtract (no division, no general multiply).
// =============================================================================

class Bn { I64 n; I64 d[72]; }

// b = v (a 64-bit value, treated as unsigned bits).
U0 BnSetU64(Bn *b, I64 v)
{
  I64 i;
  for (i = 0; i < 72; i++) b->d[i] = 0;
  b->d[0] = v & 0xFFFFFFFF;
  b->d[1] = (v >> 32) & 0xFFFFFFFF;
  b->n = 0;
  if (b->d[1]) b->n = 2;
  else if (b->d[0]) b->n = 1;
}

I64 BnIsZero(Bn *b) { return b->n == 0; }

// b = b*m + add, with small 0 <= m, add < 2^32 (so each limb product fits in I64).
U0 BnMulAddSmall(Bn *b, I64 m, I64 add)
{
  I64 carry = add, i = 0;
  while (i < b->n || carry) {
    I64 cur = b->d[i] * m + carry;
    b->d[i] = cur & 0xFFFFFFFF;
    carry = cur >> 32;
    i++;
  }
  b->n = i;
}

U0 BnCopy(Bn *dst, Bn *src)
{
  I64 i;
  for (i = 0; i < 72; i++) dst->d[i] = src->d[i];
  dst->n = src->n;
}

// dst = src << bits (bits >= 0). Writes every limb, so `dst` needs no pre-clear.
U0 BnShlBitsTo(Bn *dst, Bn *src, I64 bits)
{
  I64 limbs = bits / 32, sh = bits % 32, carry = 0, i;
  for (i = 0; i < 72; i++) dst->d[i] = 0;
  for (i = 0; i < src->n; i++) {
    I64 cur = src->d[i];
    I64 lo = ((cur << sh) & 0xFFFFFFFF) | carry;
    I64 idx = i + limbs;
    if (idx < 72) dst->d[idx] = lo;
    if (sh) carry = cur >> (32 - sh);
    else carry = 0;
  }
  I64 top = src->n + limbs;
  if (carry && top < 72) dst->d[top] = carry & 0xFFFFFFFF;
  dst->n = 0;
  for (i = 71; i >= 0; i--)
    if (dst->d[i]) { dst->n = i + 1; break; }
}

// b *= 2, in place.
U0 BnShl1(Bn *b)
{
  I64 carry = 0, i;
  for (i = 0; i < b->n; i++) {
    I64 v = (b->d[i] << 1) | carry;
    b->d[i] = v & 0xFFFFFFFF;
    carry = (v >> 32) & 1;
  }
  if (carry) { b->d[b->n] = 1; b->n++; }
}

// Compare: -1 if a<b, 0 if a==b, 1 if a>b.
I64 BnCmp(Bn *a, Bn *b)
{
  if (a->n != b->n) { if (a->n > b->n) return 1; return -1; }
  I64 i;
  for (i = a->n - 1; i >= 0; i--)
    if (a->d[i] != b->d[i]) { if (a->d[i] > b->d[i]) return 1; return -1; }
  return 0;
}

// a -= b, in place. Requires a >= b.
U0 BnSub(Bn *a, Bn *b)
{
  I64 borrow = 0, i;
  for (i = 0; i < a->n; i++) {
    I64 bi = 0;
    if (i < b->n) bi = b->d[i];
    I64 v = a->d[i] - bi - borrow;
    if (v < 0) { v = v + 0x100000000; borrow = 1; }
    else borrow = 0;
    a->d[i] = v;
  }
  while (a->n > 0 && a->d[a->n - 1] == 0) a->n--;
}
public U8 *CAlloc(I64 n)
{
  U8 *p = MAlloc(n);
  if (p) MemSet(p, 0, n);
  return p;
}
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
public I64 CmpI64(I64 *a, I64 *b) { return *a < *b ? -1 : *a > *b; }
public I64 CmpU64(U64 *a, U64 *b) { return *a < *b ? -1 : *a > *b; }
public I64 CmpF64(F64 *a, F64 *b) { return *a < *b ? -1 : *a > *b; }
public (I64, I64) Div(I64 num, I64 den) { return (num / den, num % den); }

// =============================================================================
// Number <-> string (the `atoi`/`atof` family)
// =============================================================================

// Digit value of `c` in `base` (2..36), or -1 if `c` is not a digit of that base.
I64 DigitVal(I64 c, I64 base)
{
  I64 v = -1;
  if (c >= '0' && c <= '9') v = c - '0';
  else if (c >= 'a' && c <= 'z') v = c - 'a' + 10;
  else if (c >= 'A' && c <= 'Z') v = c - 'A' + 10;
  if (v < 0 || v >= base) return -1;
  return v;
}
public I64 StrToI64Base(U8 *s, I64 base, U8 **endp)
{
  U8 *p = s;
  while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\f' || *p == '\r' || *p == '\v')
    p++;
  I64 neg = 0;
  if (*p == '-') { neg = 1; p++; }
  else if (*p == '+') p++;
  // A "0x"/"0X" prefix is consumed only when a hex digit follows; otherwise the leading
  // "0" is an ordinary digit (octal under base 0), matching C.
  if ((base == 0 || base == 16) && *p == '0' && (p[1] == 'x' || p[1] == 'X')
      && DigitVal(p[2], 16) >= 0) {
    base = 16;
    p += 2;
  } else if (base == 0) {
    base = (*p == '0') ? 8 : 10;
  }
  I64 n = 0, ndig = 0, d = 0;
  while ((d = DigitVal(*p, base)) >= 0) { n = n * base + d; p++; ndig++; }
  if (ndig == 0) {        // no conversion: report failure at the original start
    if (endp) *endp = s;
    return 0;
  }
  if (endp) *endp = p;
  if (neg) return -n;
  return n;
}
public I64 StrToI64(U8 *s) { return StrToI64Base(s, 10, NULL); }
public U64 StrToU64Base(U8 *s, I64 base, U8 **endp) { return StrToI64Base(s, base, endp); }
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
public F64 StrToF64End(U8 *str, U8 **endp)
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
  if (!sawDigit) { if (endp) *endp = str; return 0.0; } // no conversion: cursor at start

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
  if (endp) *endp = s; // the whole number is consumed; `s` is the endptr from here on

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
public F64 StrToF64(U8 *str) { return StrToF64End(str, NULL); }
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

// =============================================================================
// Pseudo-random (splitmix64). Reproducible by construction: a defined algorithm over a
// 64-bit state, so it yields the same sequence on the interpreter and every backend. The
// seed defaults to 0; call `SeedRand` to start a different deterministic stream.
// =============================================================================

U64 rand_state;  // zero-init (no explicit `= 0`: the deferred-impl batch must not re-init after user code)
public U0 SeedRand(U64 seed) { rand_state = seed; }
public U64 RandU64()
{
  rand_state += 0x9e3779b97f4a7c15;
  U64 z = rand_state;
  z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9;
  z = (z ^ (z >> 27)) * 0x94d049bb133111eb;
  return z ^ (z >> 31);
}
AtExitFn atexit_fns[ATEXIT_MAX];
I64 atexit_n;
public I64 AtExit(AtExitFn fn)
{
  if (atexit_n >= ATEXIT_MAX) return -12; // -ENOMEM
  atexit_fns[atexit_n] = fn;
  atexit_n++;
  return 0;
}

// The handler runner the compiler injects before a primitive `Exit` and at the end of
// the top level. Pops before each call, so a handler that registers another handler
// (or itself calls `Exit`) stays LIFO and never re-runs a finished handler. (Private:
// called by injected code, not by users.)
U0 __AtExitRun()
{
  while (atexit_n > 0) {
    atexit_n--;
    atexit_fns[atexit_n]();
  }
}
public U0 Abort()
{
  StdWrite(STDERR, "Aborted\n", 8);
  ExitRaw(134);
}

// The override table behind `SetEnv`/`UnsetEnv`/`PutEnv`. The process environment
// (`envp`) is read-only, so mutations land here and `Getenv` consults this table
// first. An entry is a heap "name=value" string (set), or just "name" (a tombstone:
// explicitly unset, masking any `envp` entry). The table only ever grows by distinct
// names; setting an existing name replaces its entry in place.
U8 **env_over;
I64 env_over_n, env_over_cap;

// Index of `name` in the override table, or -1. (Private.)
I64 EnvOverFind(U8 *name)
{
  I64 i;
  for (i = 0; i < env_over_n; i++) {
    U8 *e = env_over[i];
    I64 j = 0;
    while (name[j] != 0 && e[j] == name[j]) j++;
    // The whole name matched and the entry's key ends exactly here: a hit.
    if (name[j] == 0 && (e[j] == '=' || e[j] == 0)) return i;
  }
  return -1;
}

// Install `entry` ("name=value" or a "name" tombstone, already heap-owned) for `name`,
// replacing any previous override. (Private.)
U0 EnvOverPut(U8 *name, U8 *entry)
{
  I64 i = EnvOverFind(name);
  if (i >= 0) {
    Free(env_over[i]);
    env_over[i] = entry;
    return;
  }
  if (env_over_n == env_over_cap) {
    I64 ncap = env_over_cap * 2;
    if (ncap < 8) ncap = 8;
    U8 **na = MAlloc(ncap * 8);
    MemCpy(na, env_over, env_over_n * 8);
    Free(env_over);
    env_over = na;
    env_over_cap = ncap;
  }
  env_over[env_over_n] = entry;
  env_over_n++;
}
public U8 *Getenv(U8 *name)
{
  I64 oi = EnvOverFind(name);
  if (oi >= 0) {
    U8 *e = env_over[oi];
    I64 j = StrLen(name);
    if (e[j] == '=') return e + j + 1;
    return NULL; // a tombstone: explicitly unset
  }
  if (envp == NULL) return NULL;   // no environment (e.g. Windows, for now)
  I64 i = 0;
  while (envp[i] != NULL) {
    U8 *e = envp[i];
    I64 j = 0;
    while (name[j] != 0 && e[j] == name[j]) j++;
    // The whole name matched and the entry's key ends exactly here ('='): a hit.
    if (name[j] == 0 && e[j] == '=') return e + j + 1;
    i++;
  }
  return NULL;
}
public I64 SetEnv(U8 *name, U8 *val)
{
  if (name == NULL || name[0] == 0 || StrChr(name, '=') != NULL)
    return -22; // -EINVAL
  I64 nl = StrLen(name), vl = StrLen(val);
  U8 *entry = MAlloc(nl + 1 + vl + 1);
  MemCpy(entry, name, nl);
  entry[nl] = '=';
  MemCpy(entry + nl + 1, val, vl + 1);
  EnvOverPut(name, entry);
  return 0;
}
public I64 UnsetEnv(U8 *name)
{
  if (name == NULL || name[0] == 0 || StrChr(name, '=') != NULL)
    return -22; // -EINVAL
  I64 nl = StrLen(name);
  U8 *entry = MAlloc(nl + 1); // a "name" tombstone (no '=')
  MemCpy(entry, name, nl + 1);
  EnvOverPut(name, entry);
  return 0;
}
public I64 PutEnv(U8 *str)
{
  if (str == NULL || str[0] == 0 || str[0] == '=') return -22; // -EINVAL
  U8 *eq = StrChr(str, '=');
  if (eq == NULL) return UnsetEnv(str);
  I64 nl = eq - str;
  U8 *name = MAlloc(nl + 1);
  MemCpy(name, str, nl);
  name[nl] = 0;
  I64 r = SetEnv(name, eq + 1);
  Free(name);
  return r;
}

#endif
