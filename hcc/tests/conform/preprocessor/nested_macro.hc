// nested_macro.hc — nested macro expansion (CLAMP through MAX/MIN)

#include <stdio.hh>
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define CLAMP(x, lo, hi) MAX((lo), MIN((x), (hi)))
"%d\n", CLAMP(5, 1, 10);
"%d\n", CLAMP(-3, 1, 10);
"%d\n", CLAMP(15, 1, 10);
"%d\n", CLAMP(1, 1, 10);
