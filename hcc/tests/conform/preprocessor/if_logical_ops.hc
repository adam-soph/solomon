// if_logical_ops.hc — && || ! in #if expressions

#include <stdio.hh>
#define A 1
#define B 0
#define C 3

#if A && !B
"A and not B\n";
#endif

#if B || C
"B or C\n";
#endif

#if !(A && B)
"not both\n";
#endif
