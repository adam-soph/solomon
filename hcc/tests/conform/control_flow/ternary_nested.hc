// Deeply nested ternary.

#include <stdio.hh>
I64 x = 5;
I64 r = (x > 10) ? 3 : (x > 5) ? 2 : (x > 0) ? 1 : 0;
"%d\n", r;

// Ternary in a loop.
I64 i;
for (i = 0; i < 6; i++) {
  "%s ", (i % 2 == 0) ? "even" : "odd";
}
"\n";
