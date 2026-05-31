// mathlib.hc — preprocessor-heavy. Object-like and function-like macros,
// nested macro expansion (CLAMP expands through MAX and MIN), conditional
// compilation, plus integer algorithms using bit operations.

#define ABS(x)           ((x) < 0 ? -(x) : (x))
#define MAX(a, b)        ((a) > (b) ? (a) : (b))
#define MIN(a, b)        ((a) < (b) ? (a) : (b))
#define CLAMP(x, lo, hi) MAX(lo, MIN(x, hi))
#define SQ(x)            ((x) * (x))

#define ENABLE_FANCY

// Fast exponentiation: base^exp via repeated squaring.
I64 IPow(I64 base, I64 exp) {
  I64 result = 1;
  while (exp > 0) {
    if (exp & 1)
      result *= base;
    base = SQ(base);
    exp >>= 1;
  }
  return result;
}

// Integer square root (floor) by Newton's method.
I64 IsqrtFloor(I64 n) {
  if (n < 0)
    return -1;
  if (n == 0)
    return 0;
  I64 x = n;
  I64 y = (x + 1) / 2;
  while (y < x) {
    x = y;
    y = (x + n / x) / 2;
  }
  return x;
}

// Count set bits.
I64 PopCount(I64 v) {
  I64 count = 0;
  while (v != 0) {
    count += v & 1;
    v = (v >> 1) & 0x7FFFFFFFFFFFFFFF;
  }
  return count;
}

U0 Main() {
  I64 a = -7;
  I64 b = 12;
  "abs=%d max=%d min=%d clamp=%d\n", ABS(a), MAX(a, b), MIN(a, b), CLAMP(20, 0, 10);
  "sq6=%d ipow=%d isqrt=%d popcount=%d\n", SQ(6), IPow(2, 10), IsqrtFloor(144), PopCount(255);

#ifdef ENABLE_FANCY
  "fancy enabled\n";
#else
  "fancy disabled\n";
#endif

#ifndef DEBUG
  "release build\n";
#endif
}

Main;
