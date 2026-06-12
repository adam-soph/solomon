// Nested for loops (multiplication table corner).

#include <stdio.hh>
I64 i, j;
for (i = 1; i <= 3; i++) {
  for (j = 1; j <= 3; j++) {
    "%d ", i * j;
  }
  "\n";
}
