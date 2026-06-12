// macro_chained_calls.hc — CUBE defined in terms of SQUARE

#include <stdio.hh>
#define SQUARE(x) ((x) * (x))
#define CUBE(x)   ((x) * SQUARE(x))
"%d\n", CUBE(2);
"%d\n", CUBE(3);
"%d\n", SQUARE(CUBE(2));
