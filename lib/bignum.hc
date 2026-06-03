#ifndef _BIGNUM_HC
#define _BIGNUM_HC
// bignum.hc — `Bn`, a minimal arbitrary-precision **nonnegative** integer.
//
// Little-endian base-2^32 limbs: `d[i] < 2^32`, `n` active limbs, and `d[i] == 0`
// for every `i >= n` (the invariant every operation preserves). The fixed `d[72]`
// holds ~2300 bits — enough for the full normal-double range, which is what
// `<strconv.hc>`'s correctly-rounded `atof` needs. It is deliberately small: just
// the operations that decimal→binary conversion requires — build from digits
// (`BnMulAddSmall`), scale by powers of two (`BnShlBitsTo`/`BnShl1`), compare, and
// subtract. No division (the parser extracts a quotient by shift/compare/subtract)
// and no general multiply.
//
// `Bn` values are caller-owned locals (zero-initialised, so a fresh `Bn` is 0);
// methods take `Bn *`.

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

#endif
