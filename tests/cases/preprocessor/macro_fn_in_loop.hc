// macro_fn_in_loop.hc — function macro used in a loop
#define SQUARE(x) ((x) * (x))
I64 i;
for (i = 1; i <= 5; i++)
  "%d ", SQUARE(i);
"\n";
