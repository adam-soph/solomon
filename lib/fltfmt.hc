#ifndef _FLTFMT_HC
#define _FLTFMT_HC
// fltfmt.hc — the private internals of the correctly-rounded `F64` formatter.
//
// It provides a formatter-sized base-2^32 big integer, the per-conversion *magnitude*
// formatters (`%f`/`%e`/`%g`, no sign/width/flags), and the public entry `FmtFloat`
// (below) that assembles the complete field. Private to the standard library via this
// file's `_`-prefixed name.
//
// The algorithm is the portable replacement for the hand-emitted bignum in the native
// backends. It matches the conformance oracle the interpreter runs byte-for-byte
// (Rust's `{:.P}`).
//
// The big integer is little-endian base-2^32. Each limb is < 2^32, so `limb * small`
// fits in 64 bits. It is sized for the worst case `%.512f` of `1.8e308` (~820 digits).

#include <bits.hc>

class Fbn { I64 n; I64 d[120]; }

// b = v (the low 64 bits as an unsigned magnitude).
U0 FbnSet(Fbn *b, I64 v)
{
  I64 i;
  for (i = 0; i < 120; i++) b->d[i] = 0;
  b->d[0] = v & 0xFFFFFFFF;
  b->d[1] = (v >> 32) & 0xFFFFFFFF;
  b->n = 0;
  if (b->d[1]) b->n = 2;
  else if (b->d[0]) b->n = 1;
}

I64 FbnIsZero(Fbn *b) { return b->n == 0; }

U0 FbnCopy(Fbn *dst, Fbn *src)
{
  I64 i;
  for (i = 0; i < 120; i++) dst->d[i] = src->d[i];
  dst->n = src->n;
}

// b *= m, with 0 < m < 2^32 (so each limb product fits in I64).
U0 FbnMulSmall(Fbn *b, I64 m)
{
  I64 carry = 0, i = 0;
  while (i < b->n || carry) {
    I64 cur = b->d[i] * m + carry;
    b->d[i] = cur & 0xFFFFFFFF;
    carry = cur >> 32;
    i++;
  }
  b->n = i;
}

// b *= 5^p (p >= 0). Each step multiplies by 5^13, the largest power of five below
// 2^32 (5^13 = 1220703125, so limb*chunk + carry still fits I64). That does ~13x
// fewer bignum multiplies than multiplying by 5 one at a time (P can be up to 512).
U0 FbnMul5Pow(Fbn *b, I64 p)
{
  while (p >= 13) { FbnMulSmall(b, 1220703125); p -= 13; } // 5^13
  if (p > 0) {
    I64 m = 1, k;
    for (k = 0; k < p; k++) m *= 5; // 5^p, p in 1..12 (< 2^32)
    FbnMulSmall(b, m);
  }
}

// dst = src << bits (bits >= 0). Writes every limb, so `dst` needs no pre-clear.
U0 FbnShlTo(Fbn *dst, Fbn *src, I64 bits)
{
  I64 limbs = bits / 32, sh = bits % 32, carry = 0, i;
  for (i = 0; i < 120; i++) dst->d[i] = 0;
  for (i = 0; i < src->n; i++) {
    I64 cur = src->d[i];
    I64 lo = ((cur << sh) & 0xFFFFFFFF) | carry;
    I64 idx = i + limbs;
    if (idx < 120) dst->d[idx] = lo;
    if (sh) carry = cur >> (32 - sh);
    else carry = 0;
  }
  I64 top = src->n + limbs;
  if (carry && top < 120) dst->d[top] = carry & 0xFFFFFFFF;
  dst->n = 0;
  for (i = 119; i >= 0; i--)
    if (dst->d[i]) { dst->n = i + 1; break; }
}

// b = round_half_even(b / 2^bits), in place (bits > 0).
U0 FbnShrRound(Fbn *b, I64 bits)
{
  I64 word = bits / 32, bit = bits % 32, i;
  // Round bit = bit (bits-1); sticky = any set bit strictly below it.
  I64 rword = (bits - 1) / 32, rbit = (bits - 1) % 32;
  I64 roundbit = 0;
  if (rword < b->n) roundbit = (b->d[rword] >> rbit) & 1;
  I64 sticky = 0;
  for (i = 0; i < rword; i++)
    if (b->d[i]) sticky = 1;
  if (rword < b->n && (b->d[rword] & ((1 << rbit) - 1))) sticky = 1;
  // Quotient: limb i = (b[i+word] >> bit) | (b[i+word+1] << (32-bit)).
  Fbn out;
  for (i = 0; i < 120; i++) out.d[i] = 0;
  i = 0;
  while (i + word < b->n) {
    I64 lo = b->d[i + word] >> bit;
    if (bit && i + word + 1 < b->n)
      lo |= (b->d[i + word + 1] << (32 - bit)) & 0xFFFFFFFF;
    out.d[i] = lo & 0xFFFFFFFF;
    i++;
  }
  out.n = 0;
  for (i = 119; i >= 0; i--)
    if (out.d[i]) { out.n = i + 1; break; }
  // Round up iff round bit set and (sticky or the quotient is odd).
  if (roundbit && (sticky || (out.d[0] & 1))) {
    I64 k = 0;
    while (k < 120) {
      out.d[k] = (out.d[k] + 1) & 0xFFFFFFFF;
      if (out.d[k]) break;
      k++;
    }
    if (out.n <= k && k < 120) out.n = k + 1;
  }
  FbnCopy(b, &out);
}

// b /= 10^9, returning the remainder (0 .. 10^9-1). Processes the most-significant
// limb first. rem < 10^9 < 2^30, so cur = rem*2^32 + limb < 2^62, which fits in I64.
// Dividing by 10^9 (the largest power of ten below 2^32) extracts 9 decimal digits
// per O(n) pass.
I64 FbnDivPow10_9(Fbn *b)
{
  I64 rem = 0, i;
  for (i = b->n - 1; i >= 0; i--) {
    I64 cur = (rem << 32) | b->d[i];
    b->d[i] = cur / 1000000000;
    rem = cur % 1000000000;
  }
  while (b->n > 0 && b->d[b->n - 1] == 0) b->n--;
  return rem;
}

// Extract every decimal digit of b least-significant-first into `digs` (as ASCII),
// returning the count. b is consumed (left zero). Each `FbnDivPow10_9` pass is O(n)
// and yields 9 digits: zero-padded for non-top chunks, with leading zeros dropped
// from the most-significant chunk. A zero `b` yields the single digit '0'.
I64 FbnDigitsLsb(Fbn *b, U8 *digs)
{
  I64 nd = 0;
  if (FbnIsZero(b)) { digs[nd++] = '0'; return nd; }
  while (!FbnIsZero(b)) {
    I64 rem = FbnDivPow10_9(b);
    if (FbnIsZero(b)) // top chunk: significant digits only
      while (rem > 0) { digs[nd++] = '0' + rem % 10; rem /= 10; }
    else { // a full 9-digit group (higher chunks follow)
      I64 k;
      for (k = 0; k < 9; k++) { digs[nd++] = '0' + rem % 10; rem /= 10; }
    }
  }
  return nd;
}

// The `%f`-style magnitude of `v` (assumed finite) with `prec` fractional digits,
// NUL-terminated; the length is returned. Builds J = round(m*5^P*2^(E+P)) exactly,
// then places the point P digits from the right. Byte-for-byte `format!("{:.P}", |v|)`.
I64 FmtFMag(U8 *out, F64 v, I64 prec)
{
  I64 bits = Float64bits(v);
  I64 expf = (bits >> 52) & 0x7FF;
  I64 frac = bits & 0xFFFFFFFFFFFFF; // 52-bit fraction
  I64 mant, e;
  if (expf == 0) { mant = frac; e = -1074; }       // subnormal
  else { mant = frac | (1 << 52); e = expf - 1075; } // normal

  Fbn bn;
  FbnSet(&bn, mant);
  if (mant) {
    FbnMul5Pow(&bn, prec);
    I64 shift = e + prec;
    if (shift >= 0) {
      Fbn t;
      FbnShlTo(&t, &bn, shift);
      FbnCopy(&bn, &t);
    } else {
      FbnShrRound(&bn, -shift);
    }
  }

  U8 digs[1200];
  I64 nd = FbnDigitsLsb(&bn, digs);

  I64 pos = 0, i;
  if (nd > prec)
    for (i = nd - 1; i >= prec; i--) out[pos++] = digs[i];
  else
    out[pos++] = '0';
  if (prec > 0) {
    out[pos++] = '.';
    for (i = prec - 1; i >= 0; i--) {
      if (i < nd) out[pos++] = digs[i];
      else out[pos++] = '0';
    }
  }
  out[pos] = 0;
  return pos;
}

// The significant decimal digits of the magnitude `mag` (>= 0), rounded half-to-even
// to `nsig` digits (MSB-first in `digs[0..nsig-1]`). The decimal exponent of the
// leading digit is returned in `*xout`, so `mag ≈ digs[0].digs[1..] * 10^X`. This is
// the mantissa+exponent of Rust's `{:.(nsig-1)e}`: `m*2^e = Dint * 10^pe` with
// `Dint = m*5^(-e)` (e<0) or `m*2^e` (e>=0), so all decimal digits are exact.
// Rounding to `nsig` may carry and bump `X`.
U0 SciDigits(F64 mag, I64 nsig, U8 *digs, I64 *xout)
{
  I64 bits = Float64bits(mag);
  I64 expf = (bits >> 52) & 0x7FF;
  I64 frac = bits & 0xFFFFFFFFFFFFF;
  I64 i;
  if (expf == 0 && frac == 0) { // mag == 0
    for (i = 0; i < nsig; i++) digs[i] = '0';
    *xout = 0;
    return;
  }
  I64 m, e;
  if (expf == 0) { m = frac; e = -1074; }
  else { m = frac | (1 << 52); e = expf - 1075; }
  Fbn bn;
  FbnSet(&bn, m);
  I64 pe;
  if (e >= 0) {
    Fbn t;
    FbnShlTo(&t, &bn, e);
    FbnCopy(&bn, &t);
    pe = 0;
  } else {
    FbnMul5Pow(&bn, -e);
    pe = e;
  }
  // Extract all digits least-significant first; index them MSB-first as `lsb[ll-1-k]`.
  // A single buffer keeps the frame under the backend's 4 KiB local limit.
  U8 lsb[1200];
  I64 ll = FbnDigitsLsb(&bn, lsb);
  I64 x0 = (ll - 1) + pe;
  if (ll <= nsig) {
    for (i = 0; i < ll; i++) digs[i] = lsb[ll - 1 - i];
    for (i = ll; i < nsig; i++) digs[i] = '0';
    *xout = x0;
    return;
  }
  for (i = 0; i < nsig; i++) digs[i] = lsb[ll - 1 - i];
  I64 rd = lsb[ll - 1 - nsig] - '0', sticky = 0;
  for (i = nsig + 1; i < ll; i++)
    if (lsb[ll - 1 - i] != '0') { sticky = 1; break; }
  I64 up = 0;
  if (rd > 5) up = 1;
  else if (rd == 5 && (sticky || ((digs[nsig - 1] - '0') & 1))) up = 1;
  if (up) {
    I64 carry = 1, k = nsig - 1;
    while (carry && k >= 0) {
      I64 dv = (digs[k] - '0') + 1;
      if (dv == 10) digs[k] = '0';
      else { digs[k] = '0' + dv; carry = 0; }
      k--;
    }
    if (carry) { // all nines carried out: "1" then zeros, exponent up by one
      digs[0] = '1';
      for (i = 1; i < nsig; i++) digs[i] = '0';
      x0++;
    }
  }
  *xout = x0;
}

// Append `x` as a libc-style exponent (`e`/`E`, sign, >= 2 digits) at `pos`.
I64 PutExp(U8 *out, I64 pos, I64 x, I64 upper)
{
  out[pos++] = upper ? 'E' : 'e';
  I64 ex = x;
  if (ex < 0) { out[pos++] = '-'; ex = -ex; }
  else out[pos++] = '+';
  U8 eb[8];
  I64 en = 0;
  if (ex == 0) eb[en++] = '0';
  else
    while (ex > 0) { eb[en++] = '0' + ex % 10; ex /= 10; }
  while (en < 2) eb[en++] = '0';
  I64 i;
  for (i = en - 1; i >= 0; i--) out[pos++] = eb[i];
  return pos;
}

// The `%e`/`%E` magnitude of `v` (assumed finite): one leading digit, a `prec`-digit
// fraction, then the exponent. Byte-for-byte `fmt::render_exp`.
I64 FmtEMag(U8 *out, F64 v, I64 prec, I64 upper)
{
  I64 bits = Float64bits(v);
  F64 mag = Float64frombits(bits & 0x7FFFFFFFFFFFFFFF);
  U8 digs[600];
  I64 x;
  SciDigits(mag, prec + 1, digs, &x);
  I64 pos = 0;
  out[pos++] = digs[0];
  if (prec > 0) {
    out[pos++] = '.';
    I64 i;
    for (i = 1; i <= prec; i++) out[pos++] = digs[i];
  }
  pos = PutExp(out, pos, x, upper);
  out[pos] = 0;
  return pos;
}

// The `%g`/`%G` magnitude of `v` (assumed finite): `prec` significant figures. It
// chooses fixed vs scientific by the rounded exponent and trims trailing zeros unless
// `alt`. Byte-for-byte `fmt::render_g`.
I64 FmtGMag(U8 *out, F64 v, I64 prec, I64 upper, I64 alt)
{
  I64 bits = Float64bits(v);
  F64 mag = Float64frombits(bits & 0x7FFFFFFFFFFFFFFF);
  I64 p = prec, i;
  if (p < 1) p = 1;
  U8 digs[600];
  I64 x;
  SciDigits(mag, p, digs, &x);
  U8 body[2048];
  I64 bn = 0;
  if (x >= -4 && x < p) {
    I64 fp = p - 1 - x;
    if (fp < 0) fp = 0;
    bn = FmtFMag(body, mag, fp);
  } else {
    body[bn++] = digs[0];
    if (p > 1) {
      body[bn++] = '.';
      for (i = 1; i < p; i++) body[bn++] = digs[i];
    }
    bn = PutExp(body, bn, x, upper);
  }
  body[bn] = 0;
  if (!alt) {
    I64 epos = -1;
    for (i = 0; i < bn; i++)
      if (body[i] == 'e' || body[i] == 'E') { epos = i; break; }
    I64 mend = epos;
    if (epos < 0) mend = bn;
    I64 hasdot = 0;
    for (i = 0; i < mend; i++)
      if (body[i] == '.') hasdot = 1;
    if (hasdot) {
      I64 t = mend;
      while (t > 0 && body[t - 1] == '0') t--;
      if (t > 0 && body[t - 1] == '.') t--;
      if (epos >= 0) {
        I64 d = 0;
        for (i = epos; i < bn; i++) { body[t + d] = body[i]; d++; }
        bn = t + d;
      } else {
        bn = t;
      }
      body[bn] = 0;
    }
  }
  I64 pos = 0;
  for (i = 0; i < bn; i++) out[pos++] = body[i];
  out[pos] = 0;
  return pos;
}

// Write the Inf/NaN *magnitude* ("inf"/"NaN", no sign) for a non-finite `bits`, or
// return -1 when finite. The caller adds any sign.
I64 FmtSpecialMag(U8 *out, I64 bits)
{
  if (((bits >> 52) & 0x7FF) != 0x7FF) return -1;
  I64 pos = 0;
  if (bits & 0xFFFFFFFFFFFFF) {
    out[pos++] = 'N'; out[pos++] = 'a'; out[pos++] = 'N';
  } else {
    out[pos++] = 'i'; out[pos++] = 'n'; out[pos++] = 'f';
  }
  out[pos] = 0;
  return pos;
}

// Field flag bits, matching the backends' packed `printf` flags (`crate::backend`).
#define _FLT_MINUS 4  // '-' left-justify
#define _FLT_ZERO  8  // '0' zero-pad (after the sign)
#define _FLT_PLUS  16 // '+' always show a sign
#define _FLT_SPACE 32 // ' ' space before a non-negative
#define _FLT_HASH  64 // '#' alternate form (keep %g trailing zeros)

// The single entry the native backends' print lowering calls. Formats `v` into `out`
// (NUL-terminated) and returns the length. `conv` is the conversion char
// (`'f'`/`'e'`/`'E'`/`'g'`/`'G'`). `flags` is the packed flag bits above. `width` and
// `prec` are the field width and precision. The output is byte-for-byte the
// interpreter's float rendering: its magnitude renderer wrapped by `render_int`'s
// width/flag layout. The sign comes from the IEEE sign bit (so `-0.0` keeps its `-`)
// or the `+`/space flag, and zero-padding goes *after* the sign. User code prints
// floats via `Print`/`"%f", …`; this is the formatter's entry point and is `public`
// only so the float-formatter conformance tests can pin it byte-for-byte (the rest of
// the formatter — `Fbn`, `FmtFMag`, … — stays private to the stdlib directory).
public I64 FmtFloat(U8 *out, F64 v, I64 conv, I64 flags, I64 width, I64 prec)
{
  I64 bits = Float64bits(v);
  // Magnitude body: "inf"/"NaN" for non-finite, else the per-conversion formatter.
  U8 body[2048];
  I64 blen = FmtSpecialMag(body, bits);
  if (blen < 0) {
    if (conv == 'f') blen = FmtFMag(body, v, prec);
    else if (conv == 'e' || conv == 'E') blen = FmtEMag(body, v, prec, conv == 'E');
    else blen = FmtGMag(body, v, prec, conv == 'G', (flags & _FLT_HASH) != 0);
  }
  // Sign: the value's sign bit, else the `+`/space flag.
  U8 sign = 0;
  if (bits >> 63 & 1) sign = '-';
  else if (flags & _FLT_PLUS) sign = '+';
  else if (flags & _FLT_SPACE) sign = ' ';

  I64 fieldlen = blen;
  if (sign) fieldlen++;
  I64 pos = 0, i;
  if (width <= fieldlen) {
    if (sign) out[pos++] = sign;
    for (i = 0; i < blen; i++) out[pos++] = body[i];
  } else if (flags & _FLT_MINUS) {
    if (sign) out[pos++] = sign;
    for (i = 0; i < blen; i++) out[pos++] = body[i];
    for (i = 0; i < width - fieldlen; i++) out[pos++] = ' ';
  } else if (flags & _FLT_ZERO) {
    if (sign) out[pos++] = sign;
    for (i = 0; i < width - fieldlen; i++) out[pos++] = '0';
    for (i = 0; i < blen; i++) out[pos++] = body[i];
  } else {
    for (i = 0; i < width - fieldlen; i++) out[pos++] = ' ';
    if (sign) out[pos++] = sign;
    for (i = 0; i < blen; i++) out[pos++] = body[i];
  }
  out[pos] = 0;
  return pos;
}

#endif
