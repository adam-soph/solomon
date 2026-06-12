// fn_macro_1arg.hc — 1-argument function macro

#include <stdio.hh>
#define SQUARE(x) ((x) * (x))
#define DOUBLE(x) ((x) + (x))
#define NEG(x) (-(x))
"%d\n", SQUARE(5);
"%d\n", SQUARE(3 + 1);
"%d\n", DOUBLE(7);
"%d\n", NEG(4);
