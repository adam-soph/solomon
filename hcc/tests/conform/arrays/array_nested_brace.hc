// Nested brace initialization of a 2-D array.

#include <stdio.hh>
I64 a[2][3] = {{1,2,3},{4,5,6}};
I64 i, j;
for (i = 0; i < 2; i++) {
  for (j = 0; j < 3; j++) "%d ", a[i][j];
  "\n";
}
