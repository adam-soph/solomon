// Ternary on left-hand side of assignment.

#include <stdio.hh>
I64 x = 5, y = 10;
I64 max = (x > y) ? x : y;
I64 min = (x < y) ? x : y;
"max=%d min=%d\n", max, min;

// Chain of ternary assignments.
I64 sign = (x > 0) ? 1 : (x < 0) ? -1 : 0;
"sign=%d\n", sign;
