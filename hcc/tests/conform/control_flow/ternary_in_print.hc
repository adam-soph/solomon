// Ternary used directly in print args.

#include <stdio.hh>
I64 x = 7;
"%s\n", (x > 5) ? "big" : "small";
"%d\n", (x % 2 == 0) ? x / 2 : x * 3 + 1;
