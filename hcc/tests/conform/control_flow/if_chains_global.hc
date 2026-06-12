// if/else chains at top-level scope modifying a global.

#include <stdio.hh>
I64 result = 0;
I64 v = 42;
if (v < 0)
  result = -1;
else if (v < 10)
  result = 1;
else if (v < 50)
  result = 2;
else
  result = 3;
"result=%d\n", result;
