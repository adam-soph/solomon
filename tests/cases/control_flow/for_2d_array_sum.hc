// Sum elements of a 2D array with nested for.
I64 mat[3][3];
I64 i, j;
for (i = 0; i < 3; i++)
  for (j = 0; j < 3; j++)
    mat[i][j] = i * 3 + j;

I64 total = 0;
for (i = 0; i < 3; i++)
  for (j = 0; j < 3; j++)
    total += mat[i][j];
"total=%d\n", total;
