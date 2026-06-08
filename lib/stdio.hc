#ifndef _STDIO_HC
#define _STDIO_HC
// stdio.hc — C `<stdio.h>`: formatted output (the printf family + its private rendering
// core and correctly-rounded float formatter), the file-removal primitives
// (`Remove`/`Rename`), and path-based file helpers
// (`ReadFile`/`WriteFile`/`AppendFile`/`FileSize`).
//
// The printf family is ordinary HolyC: `VFmt` (below) walks the format string and renders
// each conversion into a sink (a fd via `StdWrite`, or a buffer); floats go through the
// base-2³² bignum formatter `FmtFloat`. There is no separate Rust formatter — the
// interpreter runs these same bodies and the backends compile them, so every target's
// output is byte-identical by construction. Include with `#include <stdio.hc>`.
//
// Note: a bare string statement (`"hi\n";`) lowers to a raw `StdWrite`, and the
// `"fmt", a, b` comma form desugars to a `Print(...)` call. The compiler auto-includes
// this file when a program prints, so those need no explicit include. You only need this
// file to call `Print`/`StrPrint`/`StrNPrint`/`CatPrint`/`MStrPrint` (or the file
// helpers) by name.
//
//   Print(fmt, ...)               — printf to stdout.
//   StrPrint(dst, fmt, ...)       — sprintf into `dst`; returns `dst`.
//   StrNPrint(dst, cap, fmt, ...) — snprintf: bounded sprintf; returns the would-be length.
//   CatPrint(dst, fmt, ...)       — sprintf appended at `dst + StrLen(dst)`; returns `dst`.
//   MStrPrint(fmt, ...)           — asprintf into a fresh, growing heap buffer.
//
// File errors are returned Unix-style: a non-negative result on success, a negative
// `-errno` on failure.

#include <string.hc>   // StrLen, MemCpy
#include <fcntl.hc>    // Open + O_* flags (file helpers)
#include <unistd.hc>   // Read / Write / Close / LSeek / StdWrite / WriteAll

// =============================================================================
// Correctly-rounded F64 formatter (private). This is the portable replacement for
// the hand-emitted bignum in the native backends; it matches the conformance oracle
// the interpreter runs byte-for-byte (Rust's `{:.P}`). The big integer is little-endian
// base-2³², sized for the worst case `%.512f` of `1.8e308` (~820 digits).
// =============================================================================

// Pun a double to/from its 64-bit pattern, privately (so this needn't depend on
// `<math.hc>`, which would drag all of math into every printing program). `<math.hc>`
// has its own public `Float64bits`/`Float64frombits`.
union FltBits { F64 f; U64 u; }
I64 FltToBits(F64 x)   { FltBits v; v.f = x; return v.u; }
F64 FltFromBits(I64 b) { FltBits v; v.u = b; return v.f; }

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
  I64 bits = FltToBits(v);
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
  I64 bits = FltToBits(mag);
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
  I64 bits = FltToBits(v);
  F64 mag = FltFromBits(bits & 0x7FFFFFFFFFFFFFFF);
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
  I64 bits = FltToBits(v);
  F64 mag = FltFromBits(bits & 0x7FFFFFFFFFFFFFFF);
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
// interpreter's float rendering. The sign comes from the IEEE sign bit (so `-0.0` keeps
// its `-`) or the `+`/space flag, and zero-padding goes *after* the sign. User code
// prints floats via `Print`/`"%f", …`; this is the formatter's entry point and is
// `public` only so the float-formatter conformance tests can pin it byte-for-byte.
public I64 FmtFloat(U8 *out, F64 v, I64 conv, I64 flags, I64 width, I64 prec)
{
  I64 bits = FltToBits(v);
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

// =============================================================================
// printf rendering core (private). `VFmt` walks the format string, pulls the
// variadic slots, and renders each conversion into a sink, mirroring
// `crate::fmt::{parse, render_int, render_str}` byte-for-byte; floats delegate to
// `FmtFloat` above.
// =============================================================================

// Width/precision caps matching `crate::fmt::{MAX_WIDTH, MAX_PRECISION}`.
#define _PF_MAX_WIDTH 1024
#define _PF_MAX_PREC  512

// A formatted-output sink: a fd (when `dst` is NULL) or a buffer at `dst`. `len` is
// the bytes emitted so far, and the write cursor — for a bounded sink it is the
// would-be length, which may exceed what was actually stored. When `grow` is set, a
// write that would overflow `cap` grows `dst` (the `MStrPrint` growing buffer). When
// `bound` is set, `cap` is a hard limit: at most `cap - 1` bytes are stored (the last
// byte left for the caller's NUL) and the overflow is counted but dropped (`StrNPrint`,
// i.e. snprintf). `grow` and `bound` are mutually exclusive; with neither, a `dst` sink
// is unbounded (`StrPrint`/`CatPrint` — the caller guarantees a big-enough buffer).
class Pf { U8 *dst; I64 fd; I64 len; I64 cap; I64 grow; I64 bound; }

// Append `n` bytes to the sink.
U0 PfPut(Pf *p, U8 *buf, I64 n)
{
  if (n <= 0) return;
  if (!p->dst) { StdWrite(p->fd, buf, n); p->len += n; return; }
  if (p->grow && p->len + n + 1 > p->cap) {
    I64 ncap = p->cap * 2;
    if (ncap < p->len + n + 1) ncap = p->len + n + 1;
    U8 *nd = MAlloc(ncap);          // grow the MStrPrint buffer (no `ReAlloc` dependency,
    MemCpy(nd, p->dst, p->len);     // so stdio needn't pull in `<stdlib.hc>`)
    Free(p->dst);
    p->dst = nd;
    p->cap = ncap;
  }
  // How many of the n bytes actually land in the buffer. An unbounded or freshly-grown
  // sink takes all of them; a bounded sink stores only up to `cap - 1`, reserving the
  // last byte for the terminating NUL. Either way `len` advances by the full `n`, so it
  // reports the would-be length, like C's snprintf return value.
  I64 fits = n;
  if (p->bound) {
    I64 room = p->cap - 1 - p->len;
    if (room < 0) room = 0;
    if (fits > room) fits = room;
  }
  I64 i;
  for (i = 0; i < fits; i++) p->dst[p->len + i] = buf[i];
  p->len += n;
}

// Append `n` copies of byte `c` (field padding), batched so the fd sink isn't a
// syscall per byte.
U0 PfFill(Pf *p, I64 c, I64 n)
{
  U8 buf[64];
  I64 i;
  for (i = 0; i < 64; i++) buf[i] = c;
  while (n > 0) {
    I64 k = n;
    if (k > 64) k = 64;
    PfPut(p, buf, k);
    n -= k;
  }
}

// `IFld`/`PfStrField` flag bits.
#define _PF_MINUS 1
#define _PF_ZERO  2

// The layout parameters of an integer field, bundled into one struct so `PfIntField`
// stays within the x86-64 backend's 6-integer-parameter ABI limit. `sign` is one of
// "","-","+"," ". `alt` is an alternate-form prefix ("","0x","0X"). `width`<0 means
// none, and `prec`<0 means none. `flags` packs `_PF_MINUS`/`_PF_ZERO`.
class IFld { U8 *sign; U8 *alt; I64 width; I64 prec; I64 flags; }

// Lay out an integer field (mirrors `crate::fmt::render_int`). `dig`/`ndig` are the
// magnitude digits, MSB-first (`ndig >= 1`, "0" for zero). With a precision the zero
// flag is ignored, and "0" at precision 0 yields no digits.
U0 PfIntField(Pf *p, U8 *dig, I64 ndig, IFld *f)
{
  I64 minus = f->flags & _PF_MINUS, zero = f->flags & _PF_ZERO;
  I64 prec = f->prec;
  I64 slen = StrLen(f->sign), alen = StrLen(f->alt);
  // base = digits actually shown (before precision zero-fill); "0" with prec 0 ⇒ 0.
  I64 base = ndig;
  if (prec == 0 && ndig == 1 && dig[0] == '0') base = 0;
  I64 total = base;
  if (prec > total) total = prec; // precision pads with leading zeros to `prec`
  I64 lead = total - base;        // leading precision zeros
  I64 body = slen + alen + total;
  I64 w = 0;
  if (f->width > 0) w = f->width;
  I64 pad = w - body;

  if (pad > 0 && !minus && !(zero && prec < 0))
    PfFill(p, ' ', pad);          // right-justified space pad goes first
  PfPut(p, f->sign, slen);
  PfPut(p, f->alt, alen);
  if (pad > 0 && !minus && zero && prec < 0)
    PfFill(p, '0', pad);          // zero pad after the sign/prefix
  PfFill(p, '0', lead);           // precision leading zeros
  if (base > 0) PfPut(p, dig, ndig);
  if (pad > 0 && minus)
    PfFill(p, ' ', pad);          // left-justified: trailing spaces
}

// Lay out a string/char field (mirrors `crate::fmt::render_str`): truncate `body` to
// `prec` bytes (prec<0 ⇒ none), then pad to `width` (left-justified with `minus`).
U0 PfStrField(Pf *p, U8 *body, I64 blen, I64 width, I64 prec, I64 minus)
{
  I64 len = blen;
  if (prec >= 0 && prec < len) len = prec;
  I64 w = 0;
  if (width > 0) w = width;
  I64 pad = w - len;
  if (pad > 0 && !minus) PfFill(p, ' ', pad);
  PfPut(p, body, len);
  if (pad > 0 && minus) PfFill(p, ' ', pad);
}

// The magnitude of `n` as an unsigned value (the I64 absolute value, wrapping so
// I64.MIN works).
U64 PfMag(I64 n)
{
  if (n < 0) return -(U64)n;
  return n;
}

// Write the digits of `u` in `base` (MSB-first) into `dig`, returning the count.
// `upper` selects A–F vs a–f for hex.
I64 PfDigits(U64 u, I64 base, I64 upper, U8 *dig)
{
  U8 tmp[24];
  I64 nd = 0;
  if (u == 0) tmp[nd++] = '0';
  else
    while (u) {
      I64 d = u % base;
      if (d < 10) tmp[nd++] = '0' + d;
      else tmp[nd++] = (upper ? 'A' : 'a') + d - 10;
      u /= base;
    }
  I64 i;
  for (i = 0; i < nd; i++) dig[i] = tmp[nd - 1 - i];
  return nd;
}

// Render `fmt` with the variadic slots `vargv[0..vargc)` into the sink `p`. Each slot
// is a raw 8-byte value: an I64 for integer/char conversions, the bit pattern of an
// F64 for `%f`/`%e`/`%g`, a `U8 *` for `%s`. `*` width/precision consume a slot.
U0 VFmt(Pf *p, U8 *fmt, I64 *vargv, I64 vargc)
{
  I64 ai = 0, i = 0;
  while (fmt[i]) {
    if (fmt[i] != '%') {
      I64 run = i;
      while (fmt[i] && fmt[i] != '%') i++;
      PfPut(p, fmt + run, i - run);
      continue;
    }
    i++; // past '%'
    // ---- parse the spec (mirror crate::fmt::parse) ----
    I64 minus = 0, plus = 0, space = 0, zero = 0, hash = 0;
    I64 loop = 1;
    while (loop) {
      if (fmt[i] == '-') minus = 1;
      else if (fmt[i] == '+') plus = 1;
      else if (fmt[i] == ' ') space = 1;
      else if (fmt[i] == '0') zero = 1;
      else if (fmt[i] == '#') hash = 1;
      else loop = 0;
      if (loop) i++;
    }
    I64 width = -1;
    if (fmt[i] == '*') {
      i++;
      I64 w = 0;
      if (ai < vargc) w = vargv[ai++];
      if (w < 0) { minus = 1; width = -w; } else width = w;
    } else {
      I64 have = 0, w = 0;
      while (fmt[i] >= '0' && fmt[i] <= '9') { w = w * 10 + (fmt[i] - '0'); i++; have = 1; }
      if (have) width = w;
    }
    I64 prec = -1;
    if (fmt[i] == '.') {
      i++;
      if (fmt[i] == '*') {
        i++;
        I64 pv = 0;
        if (ai < vargc) pv = vargv[ai++];
        if (pv >= 0) prec = pv; // negative ⇒ omitted
      } else {
        I64 pp = 0;
        while (fmt[i] >= '0' && fmt[i] <= '9') { pp = pp * 10 + (fmt[i] - '0'); i++; }
        prec = pp;
      }
    }
    while (fmt[i] == 'l' || fmt[i] == 'h' || fmt[i] == 'L' || fmt[i] == 'z'
           || fmt[i] == 'j' || fmt[i] == 't') i++; // length modifiers: discard
    I64 conv = fmt[i];
    if (conv) i++;
    if (width > _PF_MAX_WIDTH) width = _PF_MAX_WIDTH;
    if (prec > _PF_MAX_PREC) prec = _PF_MAX_PREC;

    if (conv == '%') { PfFill(p, '%', 1); continue; }

    // ---- dispatch ----
    // Shared integer-field layout params (sign/alt overridden per conversion).
    IFld fld;
    fld.sign = "";
    fld.alt = "";
    fld.width = width;
    fld.prec = prec;
    fld.flags = minus | (zero << 1);
    if (conv == 'd' || conv == 'i') {
      I64 n = 0;
      if (ai < vargc) n = vargv[ai++];
      if (n < 0) fld.sign = "-";
      else if (plus) fld.sign = "+";
      else if (space) fld.sign = " ";
      U8 dig[24];
      I64 nd = PfDigits(PfMag(n), 10, 0, dig);
      PfIntField(p, dig, nd, &fld);
    } else if (conv == 'u') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[24];
      I64 nd = PfDigits(u, 10, 0, dig);
      PfIntField(p, dig, nd, &fld);
    } else if (conv == 'x' || conv == 'X') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[24];
      I64 nd = PfDigits(u, 16, conv == 'X', dig);
      if (hash && u != 0) fld.alt = (conv == 'x') ? "0x" : "0X";
      PfIntField(p, dig, nd, &fld);
    } else if (conv == 'o') {
      U64 u = 0;
      if (ai < vargc) u = vargv[ai++];
      U8 dig[26];
      I64 nd = PfDigits(u, 8, 0, dig);
      if (hash) {
        if (dig[0] != '0') { // prepend a leading 0 (shift right by one)
          I64 k;
          for (k = nd; k > 0; k--) dig[k] = dig[k - 1];
          dig[0] = '0';
          nd++;
        }
        if (fld.prec == 0 && nd == 1 && dig[0] == '0') fld.prec = 1; // keep the 0
      }
      PfIntField(p, dig, nd, &fld);
    } else if (conv == 'c') {
      U8 ch = 0;
      if (ai < vargc) ch = vargv[ai++];
      PfStrField(p, &ch, 1, width, -1, minus); // %c ignores precision
    } else if (conv == 's') {
      U8 *str = "(null)";
      if (ai < vargc) { str = *(U8 **)(&vargv[ai]); ai++; }
      if (!str) str = "(null)";
      PfStrField(p, str, StrLen(str), width, prec, minus);
    } else if (conv == 'f' || conv == 'e' || conv == 'E' || conv == 'g' || conv == 'G') {
      F64 v = 0.0;
      if (ai < vargc) { v = *(F64 *)(&vargv[ai]); ai++; }
      I64 flags = 0;
      if (minus) flags |= _FLT_MINUS;
      if (zero) flags |= _FLT_ZERO;
      if (plus) flags |= _FLT_PLUS;
      if (space) flags |= _FLT_SPACE;
      if (hash) flags |= _FLT_HASH;
      I64 fp = (prec >= 0) ? prec : 6; // float default precision is 6
      U8 fbuf[2400];
      I64 flen = FmtFloat(fbuf, v, conv, flags, width < 0 ? 0 : width, fp);
      PfPut(p, fbuf, flen);
    }
    // an unknown conversion is silently dropped (matches a no-op spec)
  }
}

// =============================================================================
// printf family (public)
// =============================================================================

public U0 Print(U8 *fmt, ...)
{
  Pf p;
  p.dst = NULL;
  p.fd = STDOUT;
  p.len = 0;
  p.cap = 0;
  p.grow = 0;
  p.bound = 0;
  VFmt(&p, fmt, argv, argc);
}

public U8 *StrPrint(U8 *dst, U8 *fmt, ...)
{
  Pf p;
  p.dst = dst;
  p.fd = 0;
  p.len = 0;
  p.cap = 0;
  p.grow = 0;
  p.bound = 0;
  VFmt(&p, fmt, argv, argc);
  dst[p.len] = 0;
  return dst;
}

// Bounded sprintf (snprintf): format into `dst`, writing at most `cap` bytes including
// the terminating NUL, so `dst` is never overflowed. Returns the number of bytes that
// *would* have been written had `cap` been large enough (excluding the NUL), so a return
// value `>= cap` means the output was truncated. `cap <= 0` writes nothing at all (not
// even the NUL) but still returns the would-be length, for sizing a buffer first.
public I64 StrNPrint(U8 *dst, I64 cap, U8 *fmt, ...)
{
  Pf p;
  p.dst = dst;
  p.fd = 0;
  p.len = 0;
  p.cap = cap;
  p.grow = 0;
  p.bound = 1;
  VFmt(&p, fmt, argv, argc);
  if (cap > 0) {
    I64 nul = p.len;
    if (nul > cap - 1) nul = cap - 1;
    dst[nul] = 0;
  }
  return p.len;
}

public U8 *CatPrint(U8 *dst, U8 *fmt, ...)
{
  Pf p;
  p.dst = dst;
  p.fd = 0;
  p.len = StrLen(dst); // append at the existing NUL
  p.cap = 0;
  p.grow = 0;
  p.bound = 0;
  VFmt(&p, fmt, argv, argc);
  dst[p.len] = 0;
  return dst;
}

public U8 *MStrPrint(U8 *fmt, ...)
{
  Pf p;
  p.cap = 64;
  p.dst = MAlloc(p.cap);
  p.fd = 0;
  p.len = 0;
  p.grow = 1;
  p.bound = 0;
  VFmt(&p, fmt, argv, argc);
  p.dst[p.len] = 0;
  return p.dst;
}

// =============================================================================
// character & line output (public)
// =============================================================================

// Write one byte (`putchar`) / a line (`puts`, with a trailing newline) to stdout, via the
// portable `StdWrite` (so they work on every target). `putchar` returns the byte, `puts` a
// non-negative count, or -1 on error — like C.
public I64 PutChar(I64 c)
{
  U8 b = c;
  if (StdWrite(STDOUT, &b, 1) < 1) return -1;
  return c;
}

public I64 Puts(U8 *s)
{
  StdWrite(STDOUT, s, StrLen(s));
  StdWrite(STDOUT, "\n", 1);
  return 0;
}

// =============================================================================
// character & line input (public)
// =============================================================================
//
// Built on the `Read` primitive, a byte at a time — there is no buffered `FILE*`
// stream yet, so these read directly. `STDIN` (fd 0) is the program's standard input.

// Next byte from `fd` (0..255), or -1 at end of file. Like C's fgetc / getc.
public I64 FGetC(I64 fd)
{
  U8 c;
  if (Read(fd, &c, 1) <= 0) return -1;
  return c;
}

// Next byte from stdin, or -1 at EOF. Like C's getchar.
public I64 GetChar() { return FGetC(STDIN); }

// Read a line from `fd` into `buf` (capacity `cap`): up to `cap - 1` bytes, stopping
// after a newline (which is kept) or at EOF, then NUL-terminate. Returns `buf`, or
// NULL if EOF is reached before any byte is read. Like C's fgets.
public U8 *FGetS(U8 *buf, I64 cap, I64 fd)
{
  if (cap <= 0) return NULL;
  I64 i = 0;
  while (i < cap - 1) {
    I64 c = FGetC(fd);
    if (c < 0) break;
    buf[i] = c;
    i++;
    if (c == '\n') break;
  }
  if (i == 0) return NULL; // EOF before any byte
  buf[i] = 0;
  return buf;
}

// POSIX getline: read a whole line from `fd` (including the trailing newline) into
// *`line`, growing the buffer as needed; *`cap` tracks its allocated size. Pass
// `*line = NULL, *cap = 0` to have it allocated for you. Returns the number of bytes
// read (excluding the NUL), or -1 at EOF with nothing read. The caller owns *`line`
// (`Free` it). Grows with `MAlloc`/`MemCpy`/`Free`, so stdio needs no `<stdlib.hc>`.
public I64 GetLine(U8 **line, I64 *cap, I64 fd)
{
  if (*line == NULL || *cap < 2) {
    *cap = 128;
    *line = MAlloc(*cap);
  }
  I64 n = 0;
  while (TRUE) {
    I64 c = FGetC(fd);
    if (c < 0) break;
    if (n + 1 >= *cap) { // keep room for the NUL
      I64 ncap = *cap * 2;
      U8 *nb = MAlloc(ncap);
      MemCpy(nb, *line, n);
      Free(*line);
      *line = nb;
      *cap = ncap;
    }
    (*line)[n] = c;
    n++;
    if (c == '\n') break;
  }
  if (n == 0) return -1; // EOF, nothing read
  (*line)[n] = 0;
  return n;
}

// Read one line from `fd` into a fresh heap buffer with the trailing newline stripped,
// or NULL at EOF. The caller owns the buffer (`Free` it). An ergonomic HolyC sibling
// of `GetLine`.
public U8 *ReadLine(I64 fd)
{
  U8 *line = NULL;
  I64 cap = 0;
  I64 n = GetLine(&line, &cap, fd);
  if (n < 0) {
    if (line) Free(line);
    return NULL;
  }
  if (n > 0 && line[n - 1] == '\n') line[n - 1] = 0;
  return line;
}

// =============================================================================
// formatted input — sscanf (public)
// =============================================================================
//
// `SScan` parses `buf` against `fmt` (the printf conversions in reverse) and stores
// each field through the matching pointer argument, returning the number of fields
// assigned (or -1 at end of input before any match), like C's sscanf. It is
// self-contained — its own integer/float scanners — so stdio stays lean (no
// `<stdlib.hc>`); the `%f` parser is a direct accumulate-and-scale, not the
// correctly-rounded `StrToF64`, which is plenty for scanf. For stdin, read a line with
// `FGetS`/`ReadLine` and `SScan` it.
//
// Supported: whitespace in `fmt` skips any run of input whitespace; an ordinary `fmt`
// char must match the input; the conversions `d i u o x X c s f e E g G` (+ `%%`), an
// optional `*` (scan but don't assign), a max field width, and length modifiers
// (`l`/`h`/`L`/`z`/`j`/`t`), which are ignored — HolyC is uniform-width.

// ASCII whitespace (\t\n\v\f\r and space), without pulling <ctype.hc>.
I64 ScWs(I64 c) { return (c >= 9 && c <= 13) || c == ' '; }

// Digit value of `c` in `base` (2..36), or -1 if it is not a digit of that base.
I64 ScDigit(I64 c, I64 base)
{
  I64 v = -1;
  if (c >= '0' && c <= '9') v = c - '0';
  else if (c >= 'a' && c <= 'z') v = c - 'a' + 10;
  else if (c >= 'A' && c <= 'Z') v = c - 'A' + 10;
  if (v < 0 || v >= base) return -1;
  return v;
}

// Scan an integer from *`sp` in `base` (0 = auto: 0x→16, 0→8, else 10), consuming at
// most `width` chars (0 = unlimited; no leading-whitespace skip — the caller does it).
// Writes *`out` and advances *`sp` on success (returns 1), else returns 0.
I64 ScanInt(U8 **sp, I64 base, I64 width, I64 *out)
{
  U8 *s = *sp;
  I64 used = 0, neg = 0;
  if (*s == '-' || *s == '+') {
    if (*s == '-') neg = 1;
    s++;
    used++;
  }
  if (base == 0) {
    if (*s == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; used += 2; }
    else if (*s == '0') base = 8;
    else base = 10;
  } else if (base == 16 && *s == '0' && (s[1] == 'x' || s[1] == 'X')) {
    s += 2;
    used += 2;
  }
  I64 val = 0, ndig = 0;
  while (*s) {
    if (width > 0 && used >= width) break;
    I64 d = ScDigit(*s, base);
    if (d < 0) break;
    val = val * base + d;
    s++;
    used++;
    ndig++;
  }
  if (ndig == 0) return 0;
  if (neg) val = -val;
  *out = val;
  *sp = s;
  return 1;
}

// Scan a float from *`sp` (sign, int part, '.', fraction, [eE][+-]?exp), consuming at
// most `width` chars (0 = unlimited; no leading-whitespace skip). Writes *`out` and
// advances *`sp` on success (returns 1), else 0. Accumulate-and-scale (see the note).
I64 ScanFloat(U8 **sp, I64 width, F64 *out)
{
  U8 *s = *sp;
  I64 used = 0, neg = 0;
  if (*s == '-' || *s == '+') {
    if (*s == '-') neg = 1;
    s++;
    used++;
  }
  F64 val = 0.0;
  I64 ndig = 0;
  while (*s >= '0' && *s <= '9') {
    if (width > 0 && used >= width) break;
    val = val * 10.0 + (*s - '0');
    s++;
    used++;
    ndig++;
  }
  if (*s == '.' && (width == 0 || used < width)) {
    s++;
    used++;
    F64 scale = 0.1;
    while (*s >= '0' && *s <= '9') {
      if (width > 0 && used >= width) break;
      val = val + (*s - '0') * scale;
      scale = scale / 10.0;
      s++;
      used++;
      ndig++;
    }
  }
  if (ndig == 0) return 0;
  if ((*s == 'e' || *s == 'E') && (width == 0 || used < width)) {
    U8 *save = s;
    I64 esave = used, eneg = 0;
    s++;
    used++;
    if (*s == '-' || *s == '+') {
      if (*s == '-') eneg = 1;
      s++;
      used++;
    }
    I64 ev = 0, edig = 0;
    while (*s >= '0' && *s <= '9') {
      if (width > 0 && used >= width) break;
      ev = ev * 10 + (*s - '0');
      s++;
      used++;
      edig++;
    }
    if (edig == 0) { s = save; used = esave; } // no exponent digits: roll back the 'e'
    else {
      I64 k = 0;
      while (k < ev) { if (eneg) val = val / 10.0; else val = val * 10.0; k++; }
    }
  }
  if (neg) val = -val;
  *out = val;
  *sp = s;
  return 1;
}

public I64 SScan(U8 *buf, U8 *fmt, ...)
{
  U8 *s = buf;
  I64 ai = 0, fi = 0, assigned = 0;
  while (fmt[fi]) {
    U8 fc = fmt[fi];
    if (ScWs(fc)) { // a space in fmt matches any run of input whitespace
      fi++;
      while (ScWs(*s)) s++;
      continue;
    }
    if (fc != '%') { // an ordinary char must match the input
      if (*s != fc) return assigned;
      s++;
      fi++;
      continue;
    }
    fi++; // past '%'
    I64 suppress = 0;
    if (fmt[fi] == '*') { suppress = 1; fi++; }
    I64 width = 0;
    while (fmt[fi] >= '0' && fmt[fi] <= '9') { width = width * 10 + (fmt[fi] - '0'); fi++; }
    while (fmt[fi] == 'l' || fmt[fi] == 'h' || fmt[fi] == 'L' || fmt[fi] == 'z'
           || fmt[fi] == 'j' || fmt[fi] == 't') fi++; // length modifiers: ignored
    I64 conv = fmt[fi];
    if (conv) fi++;

    if (conv == '%') { // a literal percent, after optional whitespace
      while (ScWs(*s)) s++;
      if (*s != '%') return assigned;
      s++;
      continue;
    }
    if (conv == 'c') { // exactly `width` bytes (default 1), no whitespace skip
      I64 w = (width > 0) ? width : 1;
      if (!*s) return assigned > 0 ? assigned : -1;
      U8 *dst = NULL;
      if (!suppress) { dst = *(U8 **)(&argv[ai]); ai++; }
      I64 k = 0;
      while (k < w && *s) {
        if (dst) dst[k] = *s; // %c does not NUL-terminate
        s++;
        k++;
      }
      if (!suppress) assigned++;
      continue;
    }
    // The remaining conversions skip leading whitespace first.
    while (ScWs(*s)) s++;
    if (!*s) return assigned > 0 ? assigned : -1; // input exhausted
    if (conv == 's') {
      U8 *dst = NULL;
      if (!suppress) { dst = *(U8 **)(&argv[ai]); ai++; }
      I64 k = 0;
      while (*s && !ScWs(*s)) {
        if (width > 0 && k >= width) break;
        if (dst) dst[k] = *s;
        s++;
        k++;
      }
      if (dst) dst[k] = 0;
      if (k == 0) return assigned;
      if (!suppress) assigned++;
    } else if (conv == 'd' || conv == 'i' || conv == 'u' || conv == 'x' || conv == 'X'
               || conv == 'o') {
      I64 base = 10;
      if (conv == 'i') base = 0;
      else if (conv == 'x' || conv == 'X') base = 16;
      else if (conv == 'o') base = 8;
      I64 v = 0;
      if (!ScanInt(&s, base, width, &v)) return assigned;
      if (!suppress) { I64 *dst = *(I64 **)(&argv[ai]); ai++; *dst = v; assigned++; }
    } else if (conv == 'f' || conv == 'e' || conv == 'E' || conv == 'g' || conv == 'G') {
      F64 v = 0.0;
      if (!ScanFloat(&s, width, &v)) return assigned;
      if (!suppress) { F64 *dst = *(F64 **)(&argv[ai]); ai++; *dst = v; assigned++; }
    } else {
      return assigned; // unknown conversion: can't tell how to consume it
    }
  }
  return assigned;
}

// =============================================================================
// file removal (intrinsics) + path-based file helpers
// =============================================================================
//
// `Remove`/`Rename` are impure OS calls (`unlink`/`rename`), lowered to syscalls
// freestanding and to libc on Darwin. Each returns 0 on success, or a negative `-errno`.

public I64 Remove(U8 *path);                 // delete a file
public I64 Rename(U8 *oldpath, U8 *newpath); // rename/move

// Size of `path` in bytes, or -errno.
public I64 FileSize(U8 *path)
{
  I64 fd = Open(path, O_RDONLY, 0);
  if (fd < 0) return fd;
  I64 n = LSeek(fd, 0, SEEK_END);
  Close(fd);
  return n;  // LSeek already yields -errno on failure
}

// Read up to `cap` bytes of `path` into `buf`. Returns the byte count, or -errno. The
// caller NUL-terminates / parses the result.
public I64 ReadFile(U8 *path, U8 *buf, I64 cap)
{
  I64 fd = Open(path, O_RDONLY, 0);
  if (fd < 0) return fd;
  I64 total = 0;
  while (total < cap) {
    I64 r = Read(fd, buf + total, cap - total);
    if (r < 0) { Close(fd); return r; }
    if (r == 0) break;  // EOF
    total += r;
  }
  Close(fd);
  return total;
}

// Create/truncate `path` (mode 0644) and write `n` bytes. Returns 0, or -errno.
public I64 WriteFile(U8 *path, U8 *buf, I64 n)
{
  I64 fd = Open(path, O_WRONLY | O_CREAT | O_TRUNC, MODE_0644);
  if (fd < 0) return fd;
  I64 r = WriteAll(fd, buf, n);
  Close(fd);
  return r;
}

// Append `n` bytes to `path` (creating it, mode 0644). Returns 0, or -errno.
public I64 AppendFile(U8 *path, U8 *buf, I64 n)
{
  I64 fd = Open(path, O_WRONLY | O_CREAT | O_APPEND, MODE_0644);
  if (fd < 0) return fd;
  I64 r = WriteAll(fd, buf, n);
  Close(fd);
  return r;
}

#endif
