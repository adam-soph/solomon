// Swap two rows of a 2-D array.

#include <stdio.hh>
I64 m[3][3] = {{1,2,3},{4,5,6},{7,8,9}};
I64 j;
for (j = 0; j < 3; j++) {
  I64 tmp = m[0][j]; m[0][j] = m[2][j]; m[2][j] = tmp;
}
I64 i;
for (i = 0; i < 3; i++) {
  for (j = 0; j < 3; j++) "%d ", m[i][j];
  "\n";
}
