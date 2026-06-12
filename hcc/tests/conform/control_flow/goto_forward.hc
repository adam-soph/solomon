// goto forward (skip code).

#include <stdio.hh>
I64 x = 5;
if (x > 3)
  goto skip;
"should not print\n";
skip:
"after skip\n";
"x=%d\n", x;
