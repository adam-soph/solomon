// 2-D array: index and row-major stride.
I64 a[3][4];
I64 i, j;
for (i = 0; i < 3; i++)
  for (j = 0; j < 4; j++)
    a[i][j] = i * 4 + j;
"%d %d %d\n", a[0][0], a[1][2], a[2][3];
