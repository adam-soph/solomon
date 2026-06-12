// "Flatten" a 2-D array into a 1-D array.

#include <stdio.hh>
I64 m[2][4] = {{1,2,3,4},{5,6,7,8}};
I64 flat[8];
I64 i, j;
for (i = 0; i < 2; i++)
  for (j = 0; j < 4; j++)
    flat[i*4+j] = m[i][j];
for (i = 0; i < 8; i++) "%d ", flat[i];
"\n";
