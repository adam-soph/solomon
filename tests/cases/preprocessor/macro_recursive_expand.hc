// macro_recursive_expand.hc — multi-level macro expansion chain
#define A 1
#define B (A + 1)
#define C (B + 1)
#define D (C + 1)
I64 a = A, b = B, c = C, d = D;
"%d %d %d %d\n", a, b, c, d;
