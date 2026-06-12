// break/continue in nested loops (break/continue applies to innermost loop).

#include <stdio.hh>
I64 i, j;
for (i = 0; i < 4; i++) {
  for (j = 0; j < 4; j++) {
    if (j == 2)
      break;
    "%d%d ", i, j;
  }
}
"\n";

// continue in outer loop.
for (i = 0; i < 5; i++) {
  if (i == 2)
    continue;
  "%d ", i;
}
"\n";
