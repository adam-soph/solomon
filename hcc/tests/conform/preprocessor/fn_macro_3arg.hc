// fn_macro_3arg.hc — 3-argument function macro

#include <stdio.hh>
#define BETWEEN(x, lo, hi) ((x) >= (lo) && (x) <= (hi))
#define LERP(a, b, t) ((a) + ((b) - (a)) * (t))
"%d\n", BETWEEN(5, 1, 10);
"%d\n", BETWEEN(15, 1, 10);
"%d\n", BETWEEN(1, 1, 10);
