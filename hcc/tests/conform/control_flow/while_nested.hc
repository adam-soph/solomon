// Nested while loops.

#include <stdio.hh>
I64 i = 1;
while (i <= 4) {
  I64 j = 1;
  while (j <= i) {
    "%d ", j;
    j++;
  }
  "\n";
  i++;
}
