// macro_multiple_use.hc — same macro used many times
#define HALF(x) ((x) / 2)
I64 a = HALF(100);
I64 b = HALF(HALF(100));
I64 c = HALF(HALF(HALF(100)));
"%d %d %d\n", a, b, c;
