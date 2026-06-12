// 2-D array row-major traversal.

#include <stdio.hh>
I64 m[2][3] = {{1,2,3},{4,5,6}};
I64 i, j;
for (i = 0; i < 2; i++) {
  for (j = 0; j < 3; j++) "%d ", m[i][j];
  "\n";
}
