// macro_in_expr.hc — macros used in expressions
#define BASE 10
#define SCALE 3
I64 result = BASE * SCALE + BASE;
"%d\n", result;
I64 arr[BASE];
I64 i;
for (i = 0; i < BASE; i++) arr[i] = i;
"%d %d\n", arr[0], arr[BASE - 1];
