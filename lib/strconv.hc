#ifndef _STRCONV_HC
#define _STRCONV_HC
// strconv.hc — string → number conversion.
//
// `StrToF64` is a **correctly-rounded** decimal→double parser (the freestanding
// `atof`): it returns the IEEE-754 double nearest the decimal value, with ties
// rounded to even — bit-for-bit identical to a good libc `strtod` for inputs in
// the normal range. Pure HolyC, so it's the *same* implementation on the
// interpreter and every backend (no libc `atof`, which the freestanding ELF
// targets don't have), and conformance is automatic.
//
// Grammar (like `atof`): optional leading ASCII whitespace, an optional sign, then
// `digits[.digits]` and an optional `e`/`E` exponent. Parsing stops at the first
// character that doesn't fit; with no digits the result is 0.0.
//
// Two paths:
//   * Fast (Clinger): when the significand has ≤15 digits (so it is exact as a
//     double) and the power of ten is in [-22, 22] (also exact), the value is one
//     correctly-rounded multiply or divide — `sig * 10^k` / `sig / 10^-k`.
//   * Slow (exact): otherwise build the exact value as an integer ratio num/den
//     (num = significand·10^max(k,0), den = 10^max(-k,0)), normalise it into
//     [2^52, 2^53) by powers of two, extract the 53-bit mantissa by binary long
//     division (shift/compare/subtract — no bignum divide), and round half-to-even
//     on the remainder. This is exact, hence correctly rounded.
//
// Limits: significands past ~40 significant digits are truncated (the dropped
// digits are sub-ULP except for adversarial exact-halfway cases — irrelevant in
// practice), and subnormal results (|x| < ~2.2e-308) are best-effort, not
// guaranteed correctly rounded. Everything in the normal double range is exact.
//
// The exact path works over the `Bn` big integer from <bignum.hc>.

#include <bignum.hc>

F64 StrToF64(U8 *str)
{
  U8 *s = str;
  while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r' || *s == '\f' || *s == '\v')
    s++;
  I64 sign = 1;
  if (*s == '-') { sign = -1; s++; }
  else if (*s == '+') s++;

  // Significand digits → bignum `m` (and a fast-path I64 `sig` while it stays
  // exact), with `exp10` the power of ten implied by the decimal point.
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
        // the exponent (0.001 → 1·10^-3); an integer one is a no-op.
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
  I64 k = exp10 + expo;               // value = m · 10^k

  // Extreme magnitudes: unambiguously overflow / underflow, and a size guard for
  // the bignum (everything in [-330, 320] fits 72 limbs and is handled exactly).
  I64 magExp = k + ndig;
  if (magExp > 320) { F64 big = 1.0e308; big = big * 100.0; if (sign < 0) return -big; return big; }
  if (magExp < -330) { if (sign < 0) return -0.0; return 0.0; }

  // Fast path (Clinger): an exact significand and an exact power of ten → one
  // correctly-rounded operation. Powers 10^0..10^22 all fit a double's 53-bit
  // mantissa, so they're built exactly by repeated multiply.
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
      // one multiply by 10^22 (e.g. 1e30 = (1·10^8)·10^22).
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

  // Round half-to-even on the remainder: compare 2·remainder with den.
  BnShl1(&num);
  I64 c = BnCmp(&num, &den);
  if (c > 0 || (c == 0 && (mant & 1))) {
    mant++;
    if (mant == 9007199254740992) { mant = 4503599627370496; e++; } // 2^53 → 2^52, exp+1
  }

  // result = mant · 2^e (exact powers of two; overflows to ±inf / underflows to 0).
  F64 r = (F64)mant;
  if (e > 0) { while (e > 0) { r = r * 2.0; e--; } }
  else { while (e < 0) { r = r / 2.0; e++; } }
  if (sign < 0) r = -r;
  return r;
}

#endif
